//! x86_64 context-switch ABI.
//!
//! The task owns one of two documented resume records: `VoluntaryContext`
//! (rsp, resume IP, RFLAGS, and SysV callee-saved registers) or an interrupt
//! frame (all GPRs followed by the CPU's RIP/CS/RFLAGS frame). Both contain the
//! selected task's resume IP, flags, and stack pointer; they intentionally use
//! separate restore sequences because a call boundary only promises the
//! callee-saved registers while an interrupt must preserve every GPR.

use crate::task::{TaskEntry, VoluntaryContext};

// The assembly uses SysV registers on both boot paths. UEFI's `extern "C"`
// ABI is Microsoft x64, so these boundaries must stay explicitly SysV.
unsafe extern "sysv64" {
    fn saeos_switch_context(current: *mut VoluntaryContext, next: *const VoluntaryContext);
    fn saeos_switch_to_interrupt_context(current: *mut VoluntaryContext, stack_pointer: u64);
    fn saeos_timer_interrupt_entry();
    fn saeos_start_interrupt_context(stack_pointer: u64) -> !;
    fn saeos_start_kernel_task(entry: u64, stack_top: u64) -> !;
    fn saeos_task_bootstrap();
}

/// Enter a task through Rust's target-native function ABI. The assembly
/// bootstrap calls this using SysV on both boot paths; this Rust function then
/// calls the task entry using the ABI encoded by `TaskEntry` (Win64 on UEFI).
#[unsafe(no_mangle)]
pub extern "sysv64" fn saeos_task_entry_trampoline(entry: u64) -> ! {
    let entry: TaskEntry = unsafe { core::mem::transmute(entry) };
    entry()
}

pub fn task_bootstrap_ip() -> u64 {
    saeos_task_bootstrap as *const () as usize as u64
}

pub const TIMER_INTERRUPT_ENTRY: unsafe extern "sysv64" fn() = saeos_timer_interrupt_entry;

/// Switch between two kernel contexts.
///
/// The caller must ensure both contexts and their stacks remain valid for the
/// duration of the switch. Interrupt/preemptive contexts must use the separate
/// interrupt entry/return mechanism instead.
///
/// # Safety
/// `current` and `next` must point to valid contexts. Their stack pointers and
/// resume instruction pointers must refer to live kernel stacks and executable
/// kernel code. The caller must also ensure that no scheduler lock is held
/// across this call.
pub unsafe fn switch_context(current: &mut VoluntaryContext, next: &VoluntaryContext) {
    unsafe { saeos_switch_context(current, next) }
}

/// Save the current call-boundary continuation and resume a task from its
/// saved interrupt frame. The outgoing continuation returns here when it is
/// selected again.
///
/// # Safety
/// `current` must be a valid context and `stack_pointer` must point to a
/// complete interrupt frame on the selected task's live kernel stack. No
/// scheduler lock may be held.
pub unsafe fn switch_to_interrupt_context(current: &mut VoluntaryContext, stack_pointer: u64) {
    unsafe { saeos_switch_to_interrupt_context(current, stack_pointer) }
}

/// Start execution from a task-owned kernel interrupt frame.
///
/// # Safety
/// `stack_pointer` must point to the exact register/frame layout emitted by
/// `TaskControlBlock`'s initial-frame builder.
pub unsafe fn start_interrupt_context(stack_pointer: u64) -> ! {
    unsafe { saeos_start_interrupt_context(stack_pointer) }
}

/// Start a kernel task directly on its prepared stack. The first task has no
/// interrupted context yet; subsequent preemptions use the interrupt-frame
/// path above.
///
/// # Safety
/// `entry` must be a valid kernel function pointer with a `fn() -> !` ABI, and
/// `stack_top` must point to a mapped, writable kernel stack with sufficient
/// space for the task's execution. No scheduler lock may be held.
pub unsafe fn start_kernel_task(entry: u64, stack_top: u64) -> ! {
    unsafe { saeos_start_kernel_task(entry, stack_top) }
}
