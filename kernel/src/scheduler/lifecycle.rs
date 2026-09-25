use super::*;

pub fn exit_current(status: ExitStatus) -> ! {
    // Keep the exiting stack from receiving a timer frame after its TCB has
    // become terminal and the scheduler has selected its replacement.
    cpu::cli();
    let (dispatch, process) = with_scheduler(|scheduler| {
        let current = scheduler.current().expect("exit without current task");
        let process = scheduler.task(current).and_then(|task| task.process_id);
        let next = scheduler
            .exit_current(status)
            .ok()
            .flatten()
            .or_else(|| scheduler.idle_task())
            .expect("exit has no replacement task");
        (dispatch_for(scheduler, current, next), process)
    });
    scheduler_debug(format_args!("task exit status={}", status.0));
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
        Dispatch::Voluntary(current, next, address_space) => unsafe {
            crate::mm::address_space::activate_if_changed(address_space)
                .expect("selected address space is not registered");
            context::switch_context(&mut *current, &*next);
            loop {
                assert_switch_boundary_unlocked();
                cpu::hlt();
            }
        },
        Dispatch::Interrupt(_, stack_pointer, address_space) => unsafe {
            crate::mm::address_space::activate_if_changed(address_space)
                .expect("selected address space is not registered");
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
    Ok(status)
}

pub fn spawn_kernel_task(entry: TaskEntry, name: &'static str) -> TaskId {
    let task = with_scheduler(|scheduler| scheduler.spawn_kernel_task(entry, name, false));
    attach_task_to_new_process(task);
    task
}
