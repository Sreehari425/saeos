//! Task context records and the scheduler's single-CPU model.

extern crate alloc;

mod entity;
mod record;
mod scheduler;
mod types;
mod wait_queue;

pub use entity::{EevdfEntity, MIN_GRANULARITY, NICE_0_WEIGHT, TARGET_LATENCY};
pub use record::{SchedulerError, SchedulerStats, TaskControlBlock, TaskSnapshot};
pub use scheduler::{PolicyKind, Scheduler};
pub use types::{
    AddressSpaceId, BlockReason, DEFAULT_KERNEL_STACK_SIZE, DispatchDecision, ExitStatus,
    InterruptContext, KernelStack, ProcessId, ResumeContext, ResumeKind, TaskEntry, TaskId,
    TaskState, Tick, VirtualTime, VoluntaryContext,
};
pub use wait_queue::WaitQueue;

#[cfg(test)]
mod tests;
