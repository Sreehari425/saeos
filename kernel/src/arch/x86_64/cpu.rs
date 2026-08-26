//! Low-level CPU instruction helpers for x86_64.

#[inline]
pub fn hlt() {
    unsafe {
        core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub fn sti() {
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub fn cli() {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack, preserves_flags));
    }
}

#[inline]
pub fn interrupts_enabled() -> bool {
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq", "pop {}", out(reg) flags, options(nomem, preserves_flags));
    }
    flags & (1 << 9) != 0
}

#[inline]
pub fn pause() {
    core::hint::spin_loop();
}

/// Reads a byte from the specified I/O port.
///
/// # Safety
/// Reading from arbitrary hardware I/O ports may trigger hardware side effects.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Writes a byte to the specified I/O port.
///
/// # Safety
/// Writing to arbitrary hardware I/O ports may trigger hardware side effects.
#[inline]
pub unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

/// Reads a 16-bit word from the specified I/O port.
///
/// # Safety
/// Reading from arbitrary hardware I/O ports may trigger hardware side effects.
#[inline]
pub unsafe fn inw(port: u16) -> u16 {
    let val: u16;
    unsafe {
        core::arch::asm!("in ax, dx", out("ax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Writes a 16-bit word to the specified I/O port.
///
/// # Safety
/// Writing to arbitrary hardware I/O ports may trigger hardware side effects.
#[inline]
pub unsafe fn outw(port: u16, val: u16) {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack, preserves_flags));
    }
}

/// Reads a 32-bit dword from the specified I/O port.
///
/// # Safety
/// Reading from arbitrary hardware I/O ports may trigger hardware side effects.
#[inline]
pub unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    unsafe {
        core::arch::asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Writes a 32-bit dword to the specified I/O port.
///
/// # Safety
/// Writing to arbitrary hardware I/O ports may trigger hardware side effects.
#[inline]
pub unsafe fn outl(port: u16, val: u32) {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack, preserves_flags));
    }
}

/// Pauses momentarily for an I/O operation to complete by writing to port 0x80.
///
/// # Safety
/// Port 0x80 is commonly used as a POST checkpoint scratch port on x86.
#[inline]
pub unsafe fn io_wait() {
    unsafe {
        outb(0x80, 0);
    }
}

/// Reads a 64-bit Model-Specific Register (MSR).
///
/// # Safety
/// Reading unsupported or invalid MSR numbers triggers a General Protection Fault.
#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    ((high as u64) << 32) | (low as u64)
}

/// Writes to a 64-bit Model-Specific Register (MSR).
///
/// # Safety
/// Writing invalid values or writing to unsupported MSR numbers triggers a General Protection Fault.
#[inline]
pub unsafe fn wrmsr(msr: u32, val: u64) {
    let low = val as u32;
    let high = (val >> 32) as u32;
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[inline]
pub fn cpuid(eax: u32) -> core::arch::x86_64::CpuidResult {
    core::arch::x86_64::__cpuid(eax)
}

/// Enable execution-disable page-table bits when the processor supports them.
/// Firmware commonly leaves this enabled, but the BIOS path must establish it
/// explicitly before the kernel installs mappings containing NX.
pub fn enable_nxe() {
    let extended_max = cpuid(0x8000_0000).eax;
    if extended_max < 0x8000_0001 || cpuid(0x8000_0001).edx & (1 << 20) == 0 {
        return;
    }
    unsafe {
        const EFER: u32 = 0xc000_0080;
        const NXE: u64 = 1 << 11;
        let efer = rdmsr(EFER);
        if efer & NXE == 0 {
            wrmsr(EFER, efer | NXE);
        }
    }
}
