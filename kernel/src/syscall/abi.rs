//! Linux x86_64 user ABI layouts: `open` flags, `struct stat`,
//! `struct linux_dirent64`, and `wait` status words. Pure byte encoding,
//! tested in the host model.

use crate::fs::{DirEntry, Errno, InodeKind, OpenFlags, Stat, Whence};
use alloc::vec::Vec;

pub const O_ACCMODE: u64 = 0o3;
pub const O_RDONLY: u64 = 0o0;
pub const O_WRONLY: u64 = 0o1;
pub const O_RDWR: u64 = 0o2;
pub const O_CREAT: u64 = 0o100;
pub const O_EXCL: u64 = 0o200;
pub const O_TRUNC: u64 = 0o1000;
pub const O_APPEND: u64 = 0o2000;
pub const O_DIRECTORY: u64 = 0o200000;

/// Translate Linux `open` flags. Flags SaeOS does not act on (`O_CLOEXEC`,
/// `O_NOCTTY`, `O_LARGEFILE`, ...) are accepted and ignored, as Linux
/// ignores flags that do not apply.
pub fn open_flags(flags: u64) -> Result<OpenFlags, Errno> {
    let mut result = match flags & O_ACCMODE {
        O_RDONLY => OpenFlags::READ,
        O_WRONLY => OpenFlags::WRITE,
        O_RDWR => OpenFlags::READ_WRITE,
        _ => return Err(Errno::EINVAL),
    };
    for (bit, flag) in [
        (O_CREAT, OpenFlags::CREATE),
        (O_EXCL, OpenFlags::EXCL),
        (O_TRUNC, OpenFlags::TRUNC),
        (O_APPEND, OpenFlags::APPEND),
        (O_DIRECTORY, OpenFlags::DIRECTORY),
    ] {
        if flags & bit != 0 {
            result = result | flag;
        }
    }
    Ok(result)
}

pub fn whence(value: u64) -> Result<Whence, Errno> {
    match value {
        0 => Ok(Whence::Set),
        1 => Ok(Whence::Current),
        2 => Ok(Whence::End),
        _ => Err(Errno::EINVAL),
    }
}

pub const S_IFCHR: u32 = 0o020000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;

/// `st_mode`: file type and the permissions every SaeOS file has (there
/// are no owners or permission checks yet).
pub fn mode(kind: InodeKind) -> u32 {
    match kind {
        InodeKind::File => S_IFREG | 0o644,
        InodeKind::Directory => S_IFDIR | 0o755,
        InodeKind::CharDevice => S_IFCHR | 0o666,
    }
}

/// `sizeof(struct stat)` on x86_64.
pub const STAT_SIZE: usize = 144;

/// Encode `struct stat` (x86_64 layout). Times are zero: SaeOS keeps none.
pub fn encode_stat(stat: &Stat) -> [u8; STAT_SIZE] {
    let mut bytes = [0u8; STAT_SIZE];
    let mut put = |offset: usize, value: &[u8]| {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    };
    put(0, &stat.fs_id.to_le_bytes()); // st_dev
    put(8, &stat.ino.to_le_bytes()); // st_ino
    put(16, &u64::from(stat.nlink).to_le_bytes()); // st_nlink
    put(24, &mode(stat.kind).to_le_bytes()); // st_mode
    // st_uid, st_gid, __pad0, st_rdev: 0.
    put(48, &stat.size.to_le_bytes()); // st_size
    put(56, &4096u64.to_le_bytes()); // st_blksize
    put(64, &stat.size.div_ceil(512).to_le_bytes()); // st_blocks
    bytes
}

const DT_CHR: u8 = 2;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;

/// Size of one `struct linux_dirent64` for `name`: 19 header bytes, the
/// name and its NUL, padded to 8.
pub fn dirent_size(name: &str) -> usize {
    (19 + name.len() + 1).next_multiple_of(8)
}

/// Append one `struct linux_dirent64`. `next_offset` is the `d_off` a
/// later `lseek` would use to resume after this entry.
pub fn encode_dirent(buffer: &mut Vec<u8>, entry: &DirEntry, next_offset: u64) {
    let size = dirent_size(&entry.name);
    let start = buffer.len();
    buffer.extend_from_slice(&entry.ino.to_le_bytes()); // d_ino
    buffer.extend_from_slice(&next_offset.to_le_bytes()); // d_off
    buffer.extend_from_slice(&(size as u16).to_le_bytes()); // d_reclen
    buffer.push(match entry.kind {
        InodeKind::File => DT_REG,
        InodeKind::Directory => DT_DIR,
        InodeKind::CharDevice => DT_CHR,
    }); // d_type
    buffer.extend_from_slice(entry.name.as_bytes());
    buffer.resize(start + size, 0);
}

/// The `wait` status word for an `ExitStatus` value: a normal exit is
/// `code << 8` (`WIFEXITED`, `WEXITSTATUS`); a process killed by a signal
/// has the signal number in the low 7 bits (`WIFSIGNALED`, `WTERMSIG`).
pub fn exit_status_word(status: i32) -> i32 {
    if status < 0 {
        (-status) & 0x7f
    } else {
        (status & 0xff) << 8
    }
}
