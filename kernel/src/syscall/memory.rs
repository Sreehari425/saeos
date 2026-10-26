//! `brk`, `mmap`, `munmap`, `mprotect`, `arch_prctl`: argument decoding.
//! The memory map decides what changes (`mm/vma.rs`); the process side
//! ([`super::ProcessOps`]) applies it to the page tables.

use super::{Context, ProcessOps};
use crate::fs::Errno;
#[cfg(not(host_model))]
use crate::mm::uaccess::{UserMemory, check_range};
#[cfg(not(host_model))]
use crate::mm::vma::Protection;
#[cfg(host_model)]
use crate::uaccess::{UserMemory, check_range};
#[cfg(host_model)]
use crate::vma::Protection;

pub const MAP_SHARED: u64 = 0x01;
pub const MAP_PRIVATE: u64 = 0x02;
pub const MAP_FIXED: u64 = 0x10;
pub const MAP_ANONYMOUS: u64 = 0x20;
pub const MAP_FIXED_NOREPLACE: u64 = 0x10_0000;

pub const ARCH_SET_FS: u64 = 0x1002;
pub const ARCH_GET_FS: u64 = 0x1003;

/// `brk` never fails: it returns the (possibly unchanged) break.
pub fn brk(process: &dyn ProcessOps, address: u64) -> u64 {
    process.brk(address)
}

/// `mmap(addr, length, prot, flags, fd, offset)`: private anonymous
/// memory, or a device's memory shared (`MAP_SHARED` of `/dev/fb0`; the
/// file says where, [`crate::fs::Inode::mmap`]). Other file mappings are
/// `ENODEV`; a private device mapping is `EINVAL` (a private copy of video
/// memory makes no sense). Flags SaeOS has no use for (`MAP_NORESERVE`,
/// `MAP_STACK`, `MAP_POPULATE`, ...) are accepted, as Linux ignores what
/// does not apply.
pub fn mmap<M: UserMemory>(
    context: &Context<M>,
    hint: u64,
    length: u64,
    protection: u64,
    flags: u64,
    fd: u64,
    offset: u64,
) -> Result<u64, Errno> {
    let process = context.process;
    let protection = Protection::from_linux(protection)?;
    if flags & MAP_ANONYMOUS == 0 {
        if !offset.is_multiple_of(4096) || length == 0 {
            return Err(Errno::EINVAL);
        }
        let file = context
            .files
            .get(u32::try_from(fd).map_err(|_| Errno::EBADF)?)?;
        let physical = file.inode.mmap(offset, length)?;
        if flags & (MAP_SHARED | MAP_PRIVATE) != MAP_SHARED || flags & MAP_FIXED_NOREPLACE != 0 {
            return Err(Errno::EINVAL);
        }
        // As on Linux: the file must be readable, and open for writing to
        // be mapped writable.
        if !file.flags.contains(crate::fs::OpenFlags::READ)
            || (protection.write && !file.flags.contains(crate::fs::OpenFlags::WRITE))
        {
            return Err(Errno::EACCES);
        }
        return process.mmap_device(hint, length, protection, physical, flags & MAP_FIXED != 0);
    }
    if flags & (MAP_SHARED | MAP_PRIVATE) != MAP_PRIVATE || flags & MAP_FIXED_NOREPLACE != 0 {
        return Err(Errno::EINVAL);
    }
    if !offset.is_multiple_of(4096) {
        return Err(Errno::EINVAL);
    }
    process.mmap(hint, length, protection, flags & MAP_FIXED != 0)
}

pub fn munmap(process: &dyn ProcessOps, start: u64, length: u64) -> Result<u64, Errno> {
    process.munmap(start, length).map(|()| 0)
}

pub fn mprotect(
    process: &dyn ProcessOps,
    start: u64,
    length: u64,
    protection: u64,
) -> Result<u64, Errno> {
    let protection = Protection::from_linux(protection)?;
    process.mprotect(start, length, protection).map(|()| 0)
}

/// `arch_prctl(code, address)`: the FS base (thread pointer) only.
pub fn arch_prctl<M: UserMemory>(
    context: &Context<M>,
    code: u64,
    address: u64,
) -> Result<u64, Errno> {
    match code {
        ARCH_SET_FS => {
            // Linux: a base outside the user half is EPERM.
            check_range(address, 0).map_err(|_| Errno::EPERM)?;
            context.process.set_fs_base(address);
            Ok(0)
        }
        ARCH_GET_FS => {
            context
                .memory
                .write(address, &context.process.fs_base().to_le_bytes())?;
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
}
