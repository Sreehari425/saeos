//! Scheduler façade and interrupt-safe ownership boundary.

use crate::arch::x86_64::{context, cpu, interrupt_controller, pic};
use crate::process::{ProcessSnapshot, ProcessTable};
use crate::sync::IrqSpinMutex;
use alloc::vec::Vec;

use crate::task::{
    BlockReason, DispatchDecision, ExitStatus, ProcessId, ResumeContext, ResumeKind, Scheduler,
    SchedulerStats, StackStorage, TaskEntry, TaskId, TaskSnapshot, VoluntaryContext, WaitQueue,
};

mod dispatch;
mod lifecycle;
#[cfg(feature = "scheduler-selftest")]
mod selftest;
mod wait;

pub use dispatch::TimerDispatch;
use dispatch::{Dispatch, assert_switch_boundary_unlocked, dispatch_for, switch_dispatch};
pub use lifecycle::{collect_child_status, exit_current, register_child_waiter, spawn_kernel_task};
#[cfg(feature = "scheduler-selftest")]
pub use selftest::start_selftest;
pub use wait::exit;
pub use wait::{block_current, block_on, sleep_ms, sleep_until, wake_all, wake_one, yield_now};

/// The scheduler is single-CPU for now. Access is still protected by an
/// interrupt-disabling lock so a timer IRQ cannot recursively acquire it.
pub static SCHEDULER: IrqSpinMutex<Scheduler> = IrqSpinMutex::new(Scheduler::new());
pub static PROCESSES: IrqSpinMutex<ProcessTable> = IrqSpinMutex::new(ProcessTable::new());
static WAIT_QUEUE_SNAPSHOT: IrqSpinMutex<Vec<(u64, usize)>> = IrqSpinMutex::new(Vec::new());

#[cfg(feature = "scheduler-debug")]
fn scheduler_debug(message: core::fmt::Arguments<'_>) {
    crate::serial_println!("[SCHED] {}", message);
}

#[cfg(not(feature = "scheduler-debug"))]
fn scheduler_debug(_: core::fmt::Arguments<'_>) {}
/// Look up a stack's owner from a fault handler: never blocks, and gives up
/// if the scheduler lock is held by the code that faulted.
pub fn try_task_on_stack(base: u64) -> Option<(TaskId, &'static str)> {
    SCHEDULER.try_lock()?.task_on_stack(base)
}

pub fn current() -> Option<TaskId> {
    SCHEDULER.lock().current()
}

pub fn snapshots() -> Vec<TaskSnapshot> {
    SCHEDULER.lock().snapshots()
}

pub fn stats() -> SchedulerStats {
    SCHEDULER.lock().stats()
}

pub fn run_queue_snapshot() -> Vec<TaskId> {
    SCHEDULER.lock().run_queue().to_vec()
}

pub fn sleepers_snapshot() -> Vec<(u64, TaskId)> {
    SCHEDULER.lock().sleepers().to_vec()
}

pub fn process_snapshots() -> Vec<ProcessSnapshot> {
    PROCESSES.lock().snapshots()
}

pub fn waitqueue_snapshots() -> Vec<(u64, usize)> {
    WAIT_QUEUE_SNAPSHOT.lock().clone()
}

fn record_waitqueue(queue: &WaitQueue) {
    let mut snapshots = WAIT_QUEUE_SNAPSHOT.lock();
    if let Some((_, length)) = snapshots.iter_mut().find(|(id, _)| *id == queue.id) {
        *length = queue.len();
    } else {
        snapshots.push((queue.id, queue.len()));
    }
}

pub fn set_address_space(
    task: TaskId,
    address_space: Option<crate::task::AddressSpaceId>,
) -> Result<(), crate::task::SchedulerError> {
    let process = with_scheduler(|scheduler| {
        scheduler
            .task(task)
            .ok_or(crate::task::SchedulerError::UnknownTask)
            .map(|record| record.process_id)
    })?;
    if let Some(process) = process {
        PROCESSES
            .lock()
            .set_address_space(process, address_space)
            .map_err(|_| crate::task::SchedulerError::UnknownTask)?;
    }
    with_scheduler(|scheduler| scheduler.set_address_space(task, address_space))
}

/// Execute a scheduler operation with interrupts excluded. The operation must
/// not perform a context switch while this guard is alive; callers must release
/// it before switching stacks, restoring an interrupt frame, or halting.
pub fn with_scheduler<R>(operation: impl FnOnce(&mut Scheduler) -> R) -> R {
    let mut scheduler = SCHEDULER.lock();
    operation(&mut scheduler)
}

/// Allocate a guarded kernel stack. Called before taking the scheduler lock:
/// stack setup maps pages, and the scheduler must never call into the memory
/// manager while locked.
pub(crate) fn new_kernel_stack(name: &str) -> StackStorage {
    crate::mm::kvm::KernelStackAllocation::new()
        .unwrap_or_else(|error| panic!("no kernel stack for task '{name}': {error:?}"))
}

pub fn init() {
    let stacks = [
        new_kernel_stack("idle"),
        new_kernel_stack("shell"),
        new_kernel_stack("reaper"),
    ];
    let (idle, shell, reaper) = with_scheduler(|scheduler| {
        let [idle_stack, shell_stack, reaper_stack] = stacks;
        let idle = scheduler.spawn_kernel_task(idle_task, "idle", true, idle_stack);
        let shell = scheduler.spawn_kernel_task(crate::shell::run, "shell", false, shell_stack);
        let reaper = scheduler.spawn_kernel_task(reaper_task, "reaper", false, reaper_stack);
        (idle, shell, reaper)
    });
    for task in [idle, shell, reaper] {
        attach_task_to_new_process(task);
    }
    #[cfg(feature = "mm-guard-test")]
    spawn_kernel_task(guard_test_task, "guard-test");
}

/// Recurse until the kernel stack runs into its guard. The kernel must
/// report the overflow by task name instead of corrupting memory.
#[cfg(feature = "mm-guard-test")]
fn guard_test_task() -> ! {
    #[inline(never)]
    fn recurse(depth: u64) -> u64 {
        let frame = core::hint::black_box([depth; 32]);
        recurse(depth + 1).wrapping_add(frame[depth as usize % 32])
    }
    crate::serial_println!("guard-test: recursing until the stack guard is hit.");
    let depth = recurse(0);
    panic!("guard-test: recursion returned {depth} without hitting the guard");
}

#[cfg(not(feature = "scheduler-selftest"))]
pub fn start_selftest() -> Result<(), &'static str> {
    Err("scheduler self-test is disabled in this build")
}

fn attach_task_to_new_process(task: TaskId) -> ProcessId {
    let process = PROCESSES.lock().create(None, task);
    with_scheduler(|scheduler| {
        scheduler
            .task_mut(task)
            .expect("new task disappeared before process attachment")
            .process_id = Some(process);
    });
    process
}

pub fn start_first_task() -> ! {
    let next = with_scheduler(|scheduler| scheduler.schedule(0).ok().flatten())
        .expect("scheduler has no initial task");
    // Keep the context in the scheduler's stable task allocation. Copying it
    // onto this firmware-owned stack leaves the restore stub reading from a
    // stack that the final UEFI page map may no longer cover.
    let next_context = with_scheduler(|scheduler| {
        let task = scheduler.task(next).expect("initial task disappeared");
        (
            &task.cpu_context as *const VoluntaryContext,
            task.name,
            task.kernel_stack,
        )
    });
    scheduler_debug(format_args!(
        "first dispatch task={:?} name={} context={:#x} rsp={:#x} rip={:#x} entry={:#x} stack={:#x}..{:#x}",
        next,
        next_context.1,
        next_context.0 as u64,
        unsafe { (*next_context.0).stack_pointer },
        unsafe { (*next_context.0).instruction_pointer },
        unsafe { (*next_context.0).r12 },
        next_context.2.base,
        next_context.2.top
    ));
    crate::serial_println!(
        "Scheduler: starting {} through voluntary bootstrap.",
        next_context.1
    );
    let mut boot_context = VoluntaryContext::ZERO;
    unsafe { context::switch_context(&mut boot_context, &*next_context.0) };
    unreachable!("the boot context must never be scheduled");
}

pub fn idle_task() -> ! {
    loop {
        assert_switch_boundary_unlocked();
        cpu::hlt();
    }
}

pub fn reaper_task() -> ! {
    loop {
        let reaped = with_scheduler(|scheduler| scheduler.reap_one().ok().flatten());
        if let Some(task) = reaped {
            if let Some(process) = task.process_id {
                let mut table = PROCESSES.lock();
                let _ = table.remove_thread(process, task.task_id);
                table.reap_if_empty(process);
            }
        } else {
            assert_switch_boundary_unlocked();
            cpu::hlt();
        }
    }
}
