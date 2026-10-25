//! x87/SSE state. The kernel is soft-float and never touches these
//! registers, but code that uses floating point or SIMD (user programs, the
//! FPU self-test) does, so every task owns a [`FpuState`] and the scheduler
//! saves and restores it eagerly on each switch, as Linux does.
//!
//! Only the 512-byte `FXSAVE` area (x87 + SSE) is handled; AVX would need
//! `XSAVE` and `XCR0`.

use crate::task::FpuState;

const CR0_MP: u64 = 1 << 1;
const CR0_EM: u64 = 1 << 2;
const CR0_TS: u64 = 1 << 3;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;

/// Enable SSE (BIOS firmware leaves it off; UEFI firmware usually turns it
/// on) and reset the registers to the clean state new tasks start from.
pub fn init() {
    unsafe {
        let mut cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        cr0 = (cr0 & !(CR0_EM | CR0_TS)) | CR0_MP;
        core::arch::asm!("mov cr0, {}", in(reg) cr0, options(nostack, preserves_flags));
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags));
        restore(&FpuState::INITIAL);
    }
}

/// Save the live registers into `state`.
///
/// # Safety
/// `state` must be valid for writes; [`init`] must have run.
pub unsafe fn save(state: *mut FpuState) {
    unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) state, options(nostack, preserves_flags)) };
}

/// Load the registers from `state`.
///
/// # Safety
/// `state` must hold an image produced by `fxsave64` or
/// [`FpuState::INITIAL`] (reserved MXCSR bits set would fault).
pub unsafe fn restore(state: *const FpuState) {
    unsafe {
        core::arch::asm!("fxrstor64 [{}]", in(reg) state, options(nostack, preserves_flags));
    }
}

/// Switch FPU ownership: save the outgoing task's registers (unless it is
/// exiting, `outgoing == null`) and load the incoming task's.
///
/// # Safety
/// Both pointers must refer to live TCBs; no other code may use the FPU
/// between this and the context switch (the kernel is soft-float).
pub unsafe fn switch(outgoing: *mut FpuState, incoming: *const FpuState) {
    unsafe {
        if !outgoing.is_null() {
            save(outgoing);
        }
        restore(incoming);
    }
}
