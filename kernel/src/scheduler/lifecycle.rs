use super::*;

pub fn exit_current(status: ExitStatus) -> ! {
    // Keep the exiting stack from receiving a timer frame after its TCB has
    // become terminal and the scheduler has selected its replacement.
    cpu::cli();
    let now = crate::time::now_ns();
    let (dispatch, process) = with_scheduler(|scheduler| {
        let current = scheduler.current().expect("exit without current task");
        let process = scheduler.task(current).and_then(|task| task.process_id);
        let next = scheduler
            .exit_current(status, now)
            .ok()
            .flatten()
            .or_else(|| scheduler.idle_task())
            .expect("exit has no replacement task");
        (dispatch_for(scheduler, current, next), process)
    });
    scheduler_debug(format_args!("task exit status={}", status.0));
    if process == Some(crate::process::INIT_PID) {
        crate::init::init_exited(status);
    }
    wake_one(&REAPER_WAIT);
    if let Some(process) = process {
        let waiters = {
            let mut table = PROCESSES.lock();
            let _ = table.exit(process, status);
            table
                .process(process)
                .map(|record| record.child_waiters.clone())
                .unwrap_or_default()
        };
        if !waiters.is_empty() {
            with_scheduler(|scheduler| {
                for waiter in waiters {
                    let _ = scheduler.wake_task(waiter);
                }
            });
        }
    }
    assert_switch_boundary_unlocked();
    match dispatch {
        // The exiting task's FPU state is never needed again.
        Dispatch::Voluntary(current, next, _, target) => unsafe {
            prepare_switch(core::ptr::null_mut(), &target);
            context::switch_context(&mut *current, &*next);
            loop {
                assert_switch_boundary_unlocked();
                cpu::hlt();
            }
        },
        Dispatch::Interrupt(_, stack_pointer, _, target) => unsafe {
            prepare_switch(core::ptr::null_mut(), &target);
            context::start_interrupt_context(stack_pointer)
        },
    }
}

/// Register a task waiting for this process's exit status. The waiter must
/// already be blocked on its child wait condition; exit will make it runnable.
pub fn register_child_waiter(
    child: ProcessId,
    waiter: TaskId,
) -> Result<(), crate::process::ProcessError> {
    PROCESSES.lock().add_child_waiter(child, waiter)
}

pub fn collect_child_status(
    parent: ProcessId,
    child: ProcessId,
) -> Result<ExitStatus, crate::process::ProcessError> {
    let (status, waiters) = {
        let mut processes = PROCESSES.lock();
        let status = processes.wait_child(parent, child)?;
        let waiters = processes
            .process(child)
            .map(|record| record.child_waiters.clone())
            .unwrap_or_default();
        (status, waiters)
    };
    let mut processes = PROCESSES.lock();
    for waiter in waiters {
        let _ = processes.remove_child_waiter(child, waiter);
    }
    processes.reap_if_empty(child);
    drop(processes);
    release_reaped_spaces();
    Ok(status)
}

/// Free the address spaces of processes reaped so far. Called with no
/// scheduler or process lock held: teardown takes the frame lock.
pub fn release_reaped_spaces() {
    let spaces = PROCESSES.lock().take_released_spaces();
    for space in spaces {
        if let Err(error) = crate::mm::address_space::release(space) {
            crate::kerror!(
                "process: cannot free address space {:?}: {:?}",
                space,
                error
            );
        }
    }
}

/// The current task's process.
fn current_process() -> Option<ProcessId> {
    with_scheduler(|scheduler| {
        scheduler
            .current()
            .and_then(|task| scheduler.task(task))
            .and_then(|task| task.process_id)
    })
}

/// Block until `child` (a child of the current process) has exited and its
/// last thread is retired, then collect its status. The child's address
/// space is freed before this returns.
pub fn wait_child(child: ProcessId) -> Result<ExitStatus, crate::process::ProcessError> {
    let parent = current_process().ok_or(crate::process::ProcessError::UnknownProcess)?;
    {
        let table = PROCESSES.lock();
        let record = table
            .process(child)
            .ok_or(crate::process::ProcessError::UnknownProcess)?;
        if record.parent != Some(parent) {
            return Err(crate::process::ProcessError::UnknownProcess);
        }
    }
    wait_event(&CHILD_WAIT, || {
        PROCESSES
            .lock()
            .process(child)
            .is_none_or(|record| record.exit_status.is_some() && record.threads.is_empty())
    });
    collect_child_status(parent, child)
}

/// Spawn a kernel task in a new child process (of the current process, if
/// any) that runs in `space`, described by `map` (for `brk`/`mmap`): its CR3 is `space`'s root from the first
/// instruction. The space is owned by the process and freed when the
/// process is reaped. This is the shape a user process will have.
pub fn spawn_in_new_process(
    entry: TaskEntry,
    name: &'static str,
    space: crate::mm::paging::AddressSpace,
    map: crate::mm::vma::MemoryMap,
) -> (TaskId, ProcessId) {
    let stack = new_kernel_stack(name);
    let parent = current_process();
    let space = crate::mm::address_space::register_owned_with_map(space, map);
    with_scheduler(|scheduler| {
        let task = scheduler.spawn_kernel_task(entry, name, false, stack);
        let mut processes = PROCESSES.lock();
        let process = match parent {
            Some(parent) => processes
                .create_child(parent, Some(space), task)
                .expect("parent process exists"),
            None => processes.create(Some(space), task),
        };
        drop(processes);
        let record = scheduler
            .task_mut(task)
            .expect("task spawned under this lock");
        record.process_id = Some(process);
        record.address_space = Some(space);
        (task, process)
    })
}

/// Spawn a kernel task in a new process. The process is attached in the
/// same scheduler critical section that makes the task runnable, so the task
/// can never run (and look up its file table) without one.
pub fn spawn_kernel_task(entry: TaskEntry, name: &'static str) -> TaskId {
    let stack = new_kernel_stack(name);
    with_scheduler(|scheduler| {
        let task = scheduler.spawn_kernel_task(entry, name, false, stack);
        // Lock order: scheduler, then the process table.
        let process = PROCESSES.lock().create(None, task);
        scheduler
            .task_mut(task)
            .expect("task spawned under this lock")
            .process_id = Some(process);
        task
    })
}
