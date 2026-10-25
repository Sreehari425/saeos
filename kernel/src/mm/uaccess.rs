//! Kernel access to user memory, as Linux does it.
//!
//! ```text
//! copy_from_user(dst, user_ptr)
//!   check_range: the whole range must lie below USER_TOP     -> else EFAULT
//!   saeos_copy_user (rep movsb)   a #PF here is not fatal:
//!     page-fault entry finds the faulting RIP in FIXUPS and resumes at the
//!     fixup label, with rcx = bytes not copied              -> EFAULT
//! ```
//!
//! So a bad pointer from a program is an error code, never a kernel crash,
//! and no page tables are walked on the fast path. With SMAP the CPU also
//! faults on any kernel access to user pages outside these routines, which
//! bracket their access with `stac`/`clac`.
//!
//! `check_range` and the [`UserMemory`] trait are pure and are tested in the
//! host model; syscalls take a `UserMemory`, so the same code runs against
//! real user memory in the kernel and a fake region on the host.

use crate::fs::Errno;
use alloc::string::String;
use alloc::vec::Vec;

/// First address above the user half (Linux's `TASK_SIZE`). The last page
/// below the canonical boundary is never mapped, so a range ending exactly
/// here cannot run into non-canonical addresses.
pub const USER_TOP: u64 = 0x0000_7fff_ffff_f000;

const PAGE_SIZE: u64 = 4096;

/// `address..address + length` must lie entirely in the user half.
pub fn check_range(address: u64, length: usize) -> Result<(), Errno> {
    let end = address.checked_add(length as u64).ok_or(Errno::EFAULT)?;
    if end > USER_TOP {
        return Err(Errno::EFAULT);
    }
    Ok(())
}

/// User memory as seen by syscalls.
pub trait UserMemory {
    /// Fill `buffer` from user `address`; `EFAULT` if any byte is not
    /// readable.
    fn read(&self, address: u64, buffer: &mut [u8]) -> Result<(), Errno>;

    /// Copy `data` to user `address`; `EFAULT` if any byte is not writable.
    fn write(&self, address: u64, data: &[u8]) -> Result<(), Errno>;

    /// A NUL-terminated string of at most `max` bytes (excluding the NUL).
    /// Reads at most to the end of each page, so a string that ends right
    /// before an unmapped page works. `ENAMETOOLONG` if no NUL within
    /// `max`, `EINVAL` if not UTF-8 (SaeOS paths are text).
    fn read_cstr(&self, address: u64, max: usize) -> Result<String, Errno> {
        let mut bytes = Vec::new();
        let mut cursor = address;
        let mut chunk = [0u8; 256];
        loop {
            let to_page_end = (PAGE_SIZE - cursor % PAGE_SIZE) as usize;
            let wanted = to_page_end
                .min(chunk.len())
                .min(max.saturating_add(1) - bytes.len());
            self.read(cursor, &mut chunk[..wanted])?;
            if let Some(end) = chunk[..wanted].iter().position(|&byte| byte == 0) {
                bytes.extend_from_slice(&chunk[..end]);
                return String::from_utf8(bytes).map_err(|_| Errno::EINVAL);
            }
            bytes.extend_from_slice(&chunk[..wanted]);
            if bytes.len() > max {
                return Err(Errno::ENAMETOOLONG);
            }
            cursor = cursor.checked_add(wanted as u64).ok_or(Errno::EFAULT)?;
        }
    }
}

#[cfg(not(host_model))]
mod kernel {
    use super::{Errno, UserMemory, check_range};
    use core::sync::atomic::{AtomicBool, Ordering};

    /// Read by the copy routines: whether to bracket user access with
    /// `stac`/`clac` (those instructions fault on CPUs without SMAP).
    static SAEOS_SMAP_ENABLED: AtomicBool = AtomicBool::new(false);
    static SMEP_ENABLED: AtomicBool = AtomicBool::new(false);

    core::arch::global_asm!(
        // rdi = dst, rsi = src, rdx = length -> rax = bytes NOT copied.
        // On a fault inside `rep movsb`, rcx still counts the bytes left.
        ".global saeos_copy_user",
        "saeos_copy_user:",
        "    mov rcx, rdx",
        "    cmp byte ptr [rip + {smap}], 0",
        "    je 2f",
        "    stac",
        "2:",
        ".global saeos_copy_user_fault",
        "saeos_copy_user_fault:",
        "    rep movsb",
        ".global saeos_copy_user_fixup",
        "saeos_copy_user_fixup:",
        "    cmp byte ptr [rip + {smap}], 0",
        "    je 3f",
        "    clac",
        "3:",
        "    mov rax, rcx",
        "    ret",
        // rdi = address -> rax = 0 if one byte was readable, 1 if it
        // faulted. Deliberately no `stac`: the SMAP self-test uses this to
        // show the CPU blocks kernel access to user pages.
        ".global saeos_probe_user_read",
        "saeos_probe_user_read:",
        "    xor eax, eax",
        ".global saeos_probe_user_fault",
        "saeos_probe_user_fault:",
        "    mov cl, byte ptr [rdi]",
        "    ret",
        ".global saeos_probe_user_fixup",
        "saeos_probe_user_fixup:",
        "    mov eax, 1",
        "    ret",
        smap = sym SAEOS_SMAP_ENABLED,
    );

    unsafe extern "sysv64" {
        fn saeos_copy_user(destination: *mut u8, source: *const u8, length: usize) -> usize;
        fn saeos_probe_user_read(address: u64) -> u64;
        static saeos_copy_user_fault: u8;
        static saeos_copy_user_fixup: u8;
        static saeos_probe_user_fault: u8;
        static saeos_probe_user_fixup: u8;
    }

    /// The one place a page fault in kernel mode is recoverable: an
    /// instruction listed here touching a user address resumes at its fixup.
    pub fn fixup_for(instruction_pointer: u64, fault_address: u64) -> Option<u64> {
        if fault_address >= super::USER_TOP {
            return None;
        }
        let table = [
            (
                &raw const saeos_copy_user_fault,
                &raw const saeos_copy_user_fixup,
            ),
            (
                &raw const saeos_probe_user_fault,
                &raw const saeos_probe_user_fixup,
            ),
        ];
        table
            .iter()
            .find(|(fault, _)| *fault as u64 == instruction_pointer)
            .map(|(_, fixup)| *fixup as u64)
    }

    /// Copy `source.len()` bytes to user `destination`; returns the number
    /// of bytes that could NOT be copied (0 on success), like Linux.
    pub fn copy_to_user_partial(destination: u64, source: &[u8]) -> usize {
        if check_range(destination, source.len()).is_err() {
            return source.len();
        }
        unsafe { saeos_copy_user(destination as *mut u8, source.as_ptr(), source.len()) }
    }

    /// Fill `destination` from user `source`; returns the bytes NOT copied.
    pub fn copy_from_user_partial(destination: &mut [u8], source: u64) -> usize {
        if check_range(source, destination.len()).is_err() {
            return destination.len();
        }
        unsafe {
            saeos_copy_user(
                destination.as_mut_ptr(),
                source as *const u8,
                destination.len(),
            )
        }
    }

    pub fn copy_to_user(destination: u64, source: &[u8]) -> Result<(), Errno> {
        match copy_to_user_partial(destination, source) {
            0 => Ok(()),
            _ => Err(Errno::EFAULT),
        }
    }

    pub fn copy_from_user(destination: &mut [u8], source: u64) -> Result<(), Errno> {
        match copy_from_user_partial(destination, source) {
            0 => Ok(()),
            _ => Err(Errno::EFAULT),
        }
    }

    /// Read one byte of user memory WITHOUT `stac`. Under SMAP this always
    /// faults (and is fixed up); without SMAP it succeeds if mapped.
    /// For the self-test only.
    pub fn probe_without_stac(address: u64) -> bool {
        check_range(address, 1).is_ok() && unsafe { saeos_probe_user_read(address) } == 0
    }

    /// The current address space's user memory.
    pub struct KernelUserMemory;

    impl UserMemory for KernelUserMemory {
        fn read(&self, address: u64, buffer: &mut [u8]) -> Result<(), Errno> {
            copy_from_user(buffer, address)
        }

        fn write(&self, address: u64, data: &[u8]) -> Result<(), Errno> {
            copy_to_user(address, data)
        }
    }

    const CR0_WP: u64 = 1 << 16;
    const CR4_SMEP: u64 = 1 << 20;
    const CR4_SMAP: u64 = 1 << 21;

    /// Make ring 0 honour read-only pages (CR0.WP: without it the kernel
    /// could write into a program's code, or its own `.rodata`), then turn
    /// on SMEP (the kernel may not execute user pages) and SMAP (the kernel
    /// may not touch user pages outside the copy routines) when the CPU has
    /// them.
    pub fn init_protections() {
        use crate::arch::x86_64::cpu;
        unsafe {
            let mut cr0: u64;
            core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
            core::arch::asm!("mov cr0, {}", in(reg) cr0 | CR0_WP, options(nostack, preserves_flags));
        }
        if cpu::cpuid(0).eax < 7 {
            return;
        }
        let features = core::arch::x86_64::__cpuid_count(7, 0).ebx;
        let smep = features & (1 << 7) != 0;
        let smap = features & (1 << 20) != 0;
        let mut cr4: u64;
        unsafe {
            core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
            if smep {
                cr4 |= CR4_SMEP;
            }
            if smap {
                cr4 |= CR4_SMAP;
            }
            core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags));
        }
        SMEP_ENABLED.store(smep, Ordering::Release);
        SAEOS_SMAP_ENABLED.store(smap, Ordering::Release);
        crate::kinfo!(
            "cpu: SMEP {}, SMAP {}",
            if smep { "on" } else { "unavailable" },
            if smap { "on" } else { "unavailable" }
        );
    }

    pub fn smap_enabled() -> bool {
        SAEOS_SMAP_ENABLED.load(Ordering::Acquire)
    }

    pub fn smep_enabled() -> bool {
        SMEP_ENABLED.load(Ordering::Acquire)
    }
}

#[cfg(not(host_model))]
pub use kernel::*;
