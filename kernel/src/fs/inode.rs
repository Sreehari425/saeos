//! The VFS abstraction: every filesystem exposes its files, directories, and
//! devices as `Inode`s. The VFS (`vfs.rs`) owns everything generic: paths,
//! mounts, open-file offsets, and access-mode checks. A filesystem only
//! answers "read N bytes at offset O", "find child X", and so on.
//!
//! Every method has a default that fails with the errno Linux would return
//! for an inode that does not support it, so each filesystem implements only
//! what it supports.

use super::errno::{Errno, FsResult};
use super::file::OpenFlags;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeKind {
    File,
    Directory,
    CharDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// Unique within its filesystem.
    pub ino: u64,
    /// Identifies the filesystem instance (`EXDEV` checks).
    pub fs_id: u64,
    pub kind: InodeKind,
    pub size: u64,
    pub nlink: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: InodeKind,
}

pub trait Inode: Send + Sync + Any {
    fn stat(&self) -> Stat;

    fn kind(&self) -> InodeKind {
        self.stat().kind
    }

    /// Read up to `buf.len()` bytes at `offset`; 0 means end of file.
    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> FsResult<usize> {
        Err(Errno::EISDIR)
    }

    /// Write `buf` at `offset`, growing the file if needed.
    fn write_at(&self, _offset: u64, _buf: &[u8]) -> FsResult<usize> {
        Err(Errno::EISDIR)
    }

    fn truncate(&self, _size: u64) -> FsResult<()> {
        Err(Errno::EINVAL)
    }

    fn lookup(&self, _name: &str) -> FsResult<Arc<dyn Inode>> {
        Err(Errno::ENOTDIR)
    }

    fn create(&self, _name: &str, _kind: InodeKind) -> FsResult<Arc<dyn Inode>> {
        Err(Errno::ENOTDIR)
    }

    /// Remove a non-directory entry.
    fn unlink(&self, _name: &str) -> FsResult<()> {
        Err(Errno::ENOTDIR)
    }

    /// Remove an empty directory entry.
    fn rmdir(&self, _name: &str) -> FsResult<()> {
        Err(Errno::ENOTDIR)
    }

    /// Move entry `old` of this directory to `new` in `new_dir` (which the
    /// VFS guarantees is on the same filesystem).
    fn rename(&self, _old: &str, _new_dir: &Arc<dyn Inode>, _new: &str) -> FsResult<()> {
        Err(Errno::ENOTDIR)
    }

    /// Directory entries in a stable order.
    fn entries(&self) -> FsResult<Vec<DirEntry>> {
        Err(Errno::ENOTDIR)
    }

    /// Whether `lseek` works: files and directories, not streams. A device
    /// that is memory (`/dev/fb0`) says yes.
    fn seekable(&self) -> bool {
        self.kind() != InodeKind::CharDevice
    }

    /// Whether this is a terminal (`isatty`, terminal `ioctl`s).
    fn is_terminal(&self) -> bool {
        false
    }

    /// Called by `open`. A device that keeps per-open state (`/dev/dsp`)
    /// returns a fresh inode for this open file, released when the last
    /// descriptor to it closes; `None` opens this inode itself.
    fn open(&self, _flags: OpenFlags) -> FsResult<Option<Arc<dyn Inode>>> {
        Ok(None)
    }

    /// An `ioctl` whose argument is one `int`, in and/or out (the
    /// syscall copies it). Terminal ioctls are handled separately.
    fn ioctl(&self, _request: u64, _value: &mut i32) -> FsResult<()> {
        Err(Errno::ENOTTY)
    }

    /// An `ioctl` that fills in a struct (`FBIOGET_VSCREENINFO`, ...):
    /// its bytes, which the syscall copies to the argument. `None`: not a
    /// request this inode answers this way.
    fn ioctl_out(&self, _request: u64) -> FsResult<Option<Vec<u8>>> {
        Ok(None)
    }

    /// `mmap(MAP_SHARED)` of `length` bytes at `offset`: the physical
    /// address of device memory to map (page-aligned). Only devices that
    /// are memory (`/dev/fb0`) can.
    fn mmap(&self, _offset: u64, _length: u64) -> FsResult<u64> {
        Err(Errno::ENODEV)
    }

    /// For filesystems that downcast directories (`rename`).
    fn as_any(&self) -> &dyn Any;
}

pub trait FileSystem: Send + Sync {
    fn root(&self) -> Arc<dyn Inode>;
    fn name(&self) -> &'static str;
}

/// Longest name a directory entry may have.
pub const NAME_MAX: usize = 255;

/// Validate a single path component for creation.
pub fn check_name(name: &str) -> FsResult<()> {
    if name.is_empty() || name.contains('/') {
        Err(Errno::ENOENT)
    } else if name.len() > NAME_MAX {
        Err(Errno::ENAMETOOLONG)
    } else if name == "." || name == ".." {
        Err(Errno::EINVAL)
    } else {
        Ok(())
    }
}

static NEXT_FS_ID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

/// A fresh filesystem-instance id.
pub fn new_fs_id() -> u64 {
    NEXT_FS_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}
