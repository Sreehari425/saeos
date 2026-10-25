//! Entering and leaving ring 3.
//!
//! ```text
//! user: syscall ──▶ saeos_syscall_entry (IF=0, CPL 0, still on the user stack)
//!         save user rsp, switch to this task's kernel stack (TSS.rsp0)
//!         push an iretq frame (ss, rsp, rflags=r11, cs, rip=rcx) and the
//!         15 GPRs: the same layout as an interrupt frame
//!         sti; saeos_syscall_rust(frame) ─▶ syscall::dispatch_current
//!         cli; restore; iretq ──▶ user, rax = result
//! ```
//!
//! A syscall that blocks is an ordinary voluntary switch on the task's own
//! kernel stack, and a timer interrupt in user mode saves the same frame
//! layout, so preemption needs nothing special. The return path always uses
//! `iretq`: unlike `sysretq` it is safe for any saved RIP (no
//! non-canonical-RCX hazard); a `sysretq` fast path can come later.
//!
//! Single CPU: the user stack pointer is parked in one static slot while
//! interrupts are off, so no `swapgs`/per-CPU data is needed. SMP will need
//! a per-CPU area reached through GS.

use super::cpu::{rdmsr, wrmsr};
use super::gdt::{KERNEL_CODE, KERNEL_DATA, USER_CODE, USER_DATA};
use core::sync::atomic::{AtomicU64, Ordering};

const EFER: u32 = 0xc000_0080;
const STAR: u32 = 0xc000_0081;
const LSTAR: u32 = 0xc000_0082;
const FMASK: u32 = 0xc000_0084;
const EFER_SCE: u64 = 1 << 0;

/// RFLAGS bits cleared on `syscall` entry: trap, interrupts, direction, and
/// alignment check (so SMAP is enforced inside the kernel).
const ENTRY_CLEARED_FLAGS: u64 = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 18);

/// The user RSP while the entry stub switches stacks (interrupts are off).
static USER_RSP_SCRATCH: AtomicU64 = AtomicU64::new(0);
/// Top of the current task's kernel stack; mirrors TSS.rsp0
/// (`gdt::set_kernel_stack` writes both).
pub static KERNEL_STACK_TOP: AtomicU64 = AtomicU64::new(0);

/// Saved user registers, in push order reversed (r15 first), then the
/// `iretq` frame. Identical to the timer interrupt's saved frame.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

core::arch::global_asm!(
    ".global saeos_syscall_entry",
    "saeos_syscall_entry:",
    "    mov [rip + {scratch}], rsp",
    "    mov rsp, [rip + {kstack}]",
    "    push {user_data}",
    "    push qword ptr [rip + {scratch}]",
    "    push r11",
    "    push {user_code}",
    "    push rcx",
    "    push rax",
    "    push rbx",
    "    push rcx",
    "    push rdx",
    "    push rbp",
    "    push rsi",
    "    push rdi",
    "    push r8",
    "    push r9",
    "    push r10",
    "    push r11",
    "    push r12",
    "    push r13",
    "    push r14",
    "    push r15",
    // 20 qwords from a 16-aligned stack top: aligned for the call.
    "    mov rdi, rsp",
    "    sti",
    "    call {rust}",
    "    cli",
    "    pop r15",
    "    pop r14",
    "    pop r13",
    "    pop r12",
    "    pop r11",
    "    pop r10",
    "    pop r9",
    "    pop r8",
    "    pop rdi",
    "    pop rsi",
    "    pop rbp",
    "    pop rdx",
    "    pop rcx",
    "    pop rbx",
    "    pop rax",
    "    iretq",
    // rdi = entry, rsi = user stack pointer. Enter ring 3 with every
    // general-purpose register zeroed, so nothing from the kernel leaks.
    ".global saeos_enter_user",
    "saeos_enter_user:",
    "    cli",
    "    push {user_data}",
    "    push rsi",
    "    push 0x202",
    "    push {user_code}",
    "    push rdi",
    "    xor eax, eax",
    "    xor ebx, ebx",
    "    xor ecx, ecx",
    "    xor edx, edx",
    "    xor esi, esi",
    "    xor edi, edi",
    "    xor ebp, ebp",
    "    xor r8d, r8d",
    "    xor r9d, r9d",
    "    xor r10d, r10d",
    "    xor r11d, r11d",
    "    xor r12d, r12d",
    "    xor r13d, r13d",
    "    xor r14d, r14d",
    "    xor r15d, r15d",
    "    iretq",
    scratch = sym USER_RSP_SCRATCH,
    kstack = sym KERNEL_STACK_TOP,
    rust = sym saeos_syscall_rust,
    user_data = const USER_DATA as u64,
    user_code = const USER_CODE as u64,
);

unsafe extern "sysv64" {
    fn saeos_syscall_entry();
    fn saeos_enter_user(entry: u64, stack_pointer: u64) -> !;
}

/// Called by the entry stub with interrupts enabled, on the task's kernel
/// stack. `exit` never returns here; every other call writes rax.
extern "sysv64" fn saeos_syscall_rust(frame: &mut UserFrame) {
    let args = [
        frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9,
    ];
    frame.rax = crate::syscall::dispatch_current(frame.rax, args) as u64;
}

/// Enable `syscall`/`sysret` and point `syscall` at the entry stub.
pub fn init() {
    unsafe {
        wrmsr(EFER, rdmsr(EFER) | EFER_SCE);
        // syscall: CS = STAR[47:32], SS = +8. sysret (64-bit): CS =
        // STAR[63:48] + 16, SS = +8, both with RPL 3 (gdt.rs order).
        wrmsr(
            STAR,
            (u64::from(KERNEL_DATA) << 48) | (u64::from(KERNEL_CODE) << 32),
        );
        wrmsr(LSTAR, saeos_syscall_entry as *const () as u64);
        wrmsr(FMASK, ENTRY_CLEARED_FLAGS);
    }
}

/// Leave the kernel for good on this stack: start user code at `entry`
/// with `stack_pointer`, interrupts on, all registers zeroed. The task's
/// kernel stack is reused from its top for every later kernel entry.
///
/// # Safety
/// The current address space must map `entry` (user, executable) and the
/// user stack; TSS.rsp0 must be this task's kernel stack top.
pub unsafe fn enter_user(entry: u64, stack_pointer: u64) -> ! {
    unsafe { saeos_enter_user(entry, stack_pointer) }
}

/// Whether a saved code segment selector is a user one (CPL 3).
pub const fn from_user(code_segment: u64) -> bool {
    code_segment & 3 == 3
}

/// Read by the scheduler self-check: the stub reads the stack top from
/// here, so it must always equal TSS.rsp0.
pub fn kernel_stack_top() -> u64 {
    KERNEL_STACK_TOP.load(Ordering::Relaxed)
}
