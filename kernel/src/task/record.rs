use super::{
    AddressSpaceId, BlockReason, EevdfEntity, ExitStatus, KernelStack, ProcessId, ResumeKind,
    TaskEntry, TaskId, TaskState, Tick, VirtualTime, VoluntaryContext,
};
use alloc::boxed::Box;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerError {
    UnknownTask,
    InvalidTransition,
    AlreadyQueued,
    NotBlocked,
    CannotReapCurrent,
    InvalidAddressSpace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub task_id: TaskId,
    pub name: &'static str,
    pub process_id: Option<ProcessId>,
    pub state: TaskState,
    pub virtual_runtime: VirtualTime,
    pub virtual_deadline: VirtualTime,
    pub exec_runtime: Tick,
    pub address_space: Option<AddressSpaceId>,
    pub is_idle: bool,
    pub resume_kind: ResumeKind,
    pub block_reason: Option<BlockReason>,
    pub exit_status: Option<ExitStatus>,
    pub enqueue_time: Tick,
    pub eligible: bool,
    pub lag: VirtualTime,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerStats {
    pub context_switches: u64,
    pub timer_preemptions: u64,
    pub voluntary_yields: u64,
    pub tasks_woken: u64,
    pub tasks_reaped: u64,
}

pub struct TaskControlBlock {
    pub task_id: TaskId,
    pub process_id: Option<ProcessId>,
    pub(super) state: TaskState,
    pub block_reason: Option<BlockReason>,
    pub exit_status: Option<ExitStatus>,
    pub wait_link: Option<u64>,
    pub cpu_context: VoluntaryContext,
    /// Points at the saved register frame on this task's kernel stack.
    pub interrupt_stack_pointer: u64,
    pub resume_kind: ResumeKind,
    pub kernel_stack: KernelStack,
    pub stack_storage: Option<Box<[u8]>>,
    pub entry: Option<TaskEntry>,
    pub address_space: Option<AddressSpaceId>,
    pub scheduler_entity: EevdfEntity,
    pub is_idle: bool,
    pub name: &'static str,
}

impl TaskControlBlock {
    pub fn new(task_id: TaskId, kernel_stack: KernelStack, name: &'static str) -> Self {
        Self {
            task_id,
            process_id: None,
            state: TaskState::New,
            block_reason: None,
            exit_status: None,
            wait_link: None,
            cpu_context: VoluntaryContext::ZERO,
            interrupt_stack_pointer: 0,
            resume_kind: ResumeKind::Voluntary,
            kernel_stack,
            stack_storage: None,
            entry: None,
            address_space: None,
            scheduler_entity: EevdfEntity::default(),
            is_idle: false,
            name,
        }
    }

    pub(super) fn transition(&mut self, next: TaskState) -> Result<(), SchedulerError> {
        if !self.state.can_transition(next) {
            return Err(SchedulerError::InvalidTransition);
        }
        self.state = next;
        Ok(())
    }
}
