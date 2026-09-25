//! x86_64 context-switch ABI.
//!
//! The task owns one of two documented resume records: `VoluntaryContext`
//! (rsp, resume IP, RFLAGS, and SysV callee-saved registers) or an interrupt
//! frame (all GPRs followed by the CPU's RIP/CS/RFLAGS frame). Both contain the
//! selected task's resume IP, flags, and stack pointer; they intentionally use
//! separate restore sequences because a call boundary only promises the
//! callee-saved registers while an interrupt must preserve every GPR.

use crate::task::VoluntaryContext;

core::arch::global_asm!(
    r#"
    .text
    .global saeos_switch_context
saeos_switch_context:
    mov [rdi + 0], rsp
    lea rax, [rip + .Lsaeos_context_resume]
    mov [rdi + 8], rax
    mov [rdi + 16], rbx
    mov [rdi + 24], rbp
    mov [rdi + 32], r12
    mov [rdi + 40], r13
    mov [rdi + 48], r14
    mov [rdi + 56], r15
    pushfq
    pop rax
    mov [rdi + 64], rax

    // Restore arithmetic/status flags while keeping IF clear. The Rust
    // dispatch wrapper reenables interrupts only after the resumed call has
    // finished restoring its caller state.
    mov rax, [rsi + 64]
    btr rax, 9
    push rax
    popfq
    mov rsp, [rsi + 0]
    mov rbx, [rsi + 16]
    mov rbp, [rsi + 24]
    mov r12, [rsi + 32]
    mov r13, [rsi + 40]
    mov r14, [rsi + 48]
    mov r15, [rsi + 56]
    // A fresh task enables interrupts in saeos_task_bootstrap.
    mov rax, [rsi + 8]
    jmp rax

.Lsaeos_context_resume:
    ret

    .global saeos_save_context
saeos_save_context:
    // Resume directly at the caller of the Rust save_context wrapper. Its
    // wrapper and return-address slots are reused by the following
    // start_interrupt_context call on this same task stack.
    lea rax, [rsp + 24]
    mov [rdi + 0], rax
    mov rax, [rsp + 16]
    mov [rdi + 8], rax
    mov [rdi + 16], rbx
    mov [rdi + 24], rbp
    mov [rdi + 32], r12
    mov [rdi + 40], r13
    mov [rdi + 48], r14
    mov [rdi + 56], r15
    pushfq
    pop rax
    mov [rdi + 64], rax
    xor eax, eax
    ret

    // A fresh kernel task starts through a context with its entry address in
    // r12, a callee-saved register that is already part of VoluntaryContext.
    .global saeos_task_bootstrap
saeos_task_bootstrap:
    sti
    jmp r12

    // Timer IRQ entry. The CPU has already pushed the kernel interrupt
    // return frame. Save all general-purpose registers above it, then let
    // Rust return the stack pointer of the task whose frame should resume.
    .global saeos_timer_interrupt_entry
saeos_timer_interrupt_entry:
    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    // Interrupts can arrive at any point in a kernel function, so the
    // interrupted RSP does not guarantee the SysV call-site alignment.
    // Preserve the saved-frame pointer in RDI and align the callback stack
    // independently below the saved registers.
    mov rdi, rsp
    and rsp, -16
    call saeos_timer_interrupt_rust
    // RAX is the selected context pointer. RDX distinguishes an interrupt
    // frame from a saved voluntary context.
    test rdx, rdx
    jnz .Lsaeos_restore_voluntary
    mov rsp, rax
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq

.Lsaeos_restore_voluntary:
    mov rsi, rax
    mov rax, [rsi + 64]
    btr rax, 9
    push rax
    popfq
    mov rsp, [rsi + 0]
    mov rbx, [rsi + 16]
    mov rbp, [rsi + 24]
    mov r12, [rsi + 32]
    mov r13, [rsi + 40]
    mov r14, [rsi + 48]
    mov r15, [rsi + 56]
    mov r11, [rsi + 8]
    // save_context() resumes after its assembly call and tests RAX for the
    // resumed=true result. RAX is caller-saved for ordinary switches.
    mov eax, 1
    jmp r11

    // Start a task from a prepared kernel-mode interrupt frame.
    .global saeos_start_interrupt_context
saeos_start_interrupt_context:
    mov rsp, rdi
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    iretq

    .global saeos_start_kernel_task
saeos_start_kernel_task:
    cli
    mov rsp, rsi
    and rsp, -16
    sub rsp, 8
    sti
    jmp rdi
"#
);

// The assembly uses SysV registers on both boot paths. UEFI's `extern "C"`
// ABI is Microsoft x64, so these boundaries must stay explicitly SysV.
unsafe extern "sysv64" {
    fn saeos_switch_context(current: *mut VoluntaryContext, next: *const VoluntaryContext);
    fn saeos_save_context(current: *mut VoluntaryContext) -> u64;
    fn saeos_timer_interrupt_entry();
    fn saeos_start_interrupt_context(stack_pointer: u64) -> !;
    fn saeos_start_kernel_task(entry: u64, stack_top: u64) -> !;
    fn saeos_task_bootstrap();
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

// The save stub walks through this wrapper's stack frame using the SysV
// layout; keep its ABI explicit even for the UEFI target.
pub unsafe extern "sysv64" fn save_context(current: &mut VoluntaryContext) -> bool {
    unsafe { saeos_save_context(current) != 0 }
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
