pub type Tick = u64;
pub type VirtualTime = i128;
pub type TaskEntry = fn() -> !;
pub const DEFAULT_KERNEL_STACK_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AddressSpaceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitStatus(pub i32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockReason {
    SleepUntil(Tick),
    WaitQueue(u64),
    WaitingForIo(u64),
    WaitingForChild(ProcessId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    New,
    Ready,
    Running,
    Blocked,
    Exited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeKind {
    Voluntary,
    Interrupt,
}

/// A scheduler decision carries a copy of the context that is valid for the
/// selected task. The architecture layer converts this into the appropriate
/// register/stack operation; it never guesses which layout is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeContext {
    Voluntary(VoluntaryContext),
    Interrupt { stack_pointer: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatchDecision {
    pub task_id: TaskId,
    pub resume_context: ResumeContext,
    pub address_space: Option<AddressSpaceId>,
}

impl TaskState {
    pub const fn is_runnable(self) -> bool {
        matches!(self, Self::Ready | Self::Running)
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Exited)
    }

    pub const fn can_transition(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::New, Self::Ready)
                | (Self::Ready, Self::Running)
                | (Self::Running, Self::Ready | Self::Blocked | Self::Exited)
                | (Self::Blocked, Self::Ready)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct VoluntaryContext {
    pub stack_pointer: u64,
    pub instruction_pointer: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rflags: u64,
}

// These offsets are consumed directly by context.rs assembly. Keep the C
// layout explicit so an innocent field edit cannot corrupt a restored stack.
const _: () = {
    assert!(core::mem::size_of::<VoluntaryContext>() == 9 * 8);
    assert!(core::mem::offset_of!(VoluntaryContext, stack_pointer) == 0);
    assert!(core::mem::offset_of!(VoluntaryContext, instruction_pointer) == 8);
    assert!(core::mem::offset_of!(VoluntaryContext, rbx) == 16);
    assert!(core::mem::offset_of!(VoluntaryContext, rflags) == 64);
};

impl VoluntaryContext {
    pub const ZERO: Self = Self {
        stack_pointer: 0,
        instruction_pointer: 0,
        rbx: 0,
        rbp: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        // New task bootstrap enables interrupts after its entry stack is live.
        rflags: 0x2,
    };
}

/// The CPU-provided interrupt-return frame is deliberately distinct from the
/// software-defined voluntary-switch context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct InterruptContext {
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelStack {
    pub base: u64,
    pub top: u64,
}
