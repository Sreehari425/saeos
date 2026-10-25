//! Task context records and the scheduler's single-CPU model.

extern crate alloc;

mod entity;
mod record;
mod scheduler;
mod types;
mod wait_queue;

pub use entity::{
    BASE_SLICE_NS, EevdfEntity, LAG_LIMIT, MAX_NICE, MIN_NICE, NICE_0_WEIGHT, nice_to_weight,
};
pub use record::{SchedulerError, SchedulerStats, TaskControlBlock, TaskSnapshot};
pub use scheduler::{PolicyKind, Scheduler};
pub use types::{
    AddressSpaceId, BlockReason, DEFAULT_KERNEL_STACK_SIZE, DispatchDecision, ExitStatus, FpuState,
    InterruptContext, KernelStack, Nanos, ProcessId, ResumeContext, ResumeKind, StackStorage,
    SwitchTarget, TaskEntry, TaskId, TaskState, Tick, VirtualTime, VoluntaryContext,
    stack_storage_bounds,
};
pub use wait_queue::WaitQueue;

#[cfg(test)]
mod tests;
