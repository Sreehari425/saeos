//! Open files and per-process file-descriptor tables.

use super::errno::{Errno, FsResult};
use super::inode::Inode;
use super::lock::Mutex;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// `open` flags, with Linux's meaning (not its bit values: those are
/// translated at the syscall boundary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags(u32);

impl OpenFlags {
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const READ_WRITE: Self = Self(Self::READ.0 | Self::WRITE.0);
    /// Create the file if it does not exist.
    pub const CREATE: Self = Self(1 << 2);
    /// With `CREATE`: fail with `EEXIST` if it exists.
    pub const EXCL: Self = Self(1 << 3);
    /// Truncate a regular file opened for writing.
    pub const TRUNC: Self = Self(1 << 4);
    /// Every write goes to the current end of file.
    pub const APPEND: Self = Self(1 << 5);
    /// Fail with `ENOTDIR` unless the path is a directory.
    pub const DIRECTORY: Self = Self(1 << 6);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for OpenFlags {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whence {
    Set,
    Current,
    End,
}

/// A file descriptor number.
pub type Fd = u32;

/// An open file description: shared by every descriptor that refers to it
/// (after `dup` or `fork`), so they also share the offset, as in POSIX.
pub struct OpenFile {
    pub inode: Arc<dyn Inode>,
    pub flags: OpenFlags,
    /// Held for the whole read or write, so concurrent users of one open
    /// file see whole, sequential transfers.
    pub offset: Mutex<u64>,
}

impl OpenFile {
    pub fn new(inode: Arc<dyn Inode>, flags: OpenFlags) -> Arc<Self> {
        Arc::new(Self {
            inode,
            flags,
            offset: Mutex::new(0),
        })
    }
}

/// Most descriptors a process may hold open.
pub const MAX_FDS: usize = 256;

pub struct FileTable {
    slots: Mutex<Vec<Option<Arc<OpenFile>>>>,
}

impl Default for FileTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FileTable {
    pub const fn new() -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
        }
    }

    /// Install `file` at the lowest free descriptor.
    pub fn insert(&self, file: Arc<OpenFile>) -> FsResult<Fd> {
        let mut slots = self.slots.lock();
        if let Some(index) = slots.iter().position(Option::is_none) {
            slots[index] = Some(file);
            return Ok(index as Fd);
        }
        if slots.len() >= MAX_FDS {
            return Err(Errno::EMFILE);
        }
        slots.push(Some(file));
        Ok((slots.len() - 1) as Fd)
    }

    pub fn get(&self, fd: Fd) -> FsResult<Arc<OpenFile>> {
        self.slots
            .lock()
            .get(fd as usize)
            .and_then(Option::clone)
            .ok_or(Errno::EBADF)
    }

    pub fn remove(&self, fd: Fd) -> FsResult<Arc<OpenFile>> {
        let mut slots = self.slots.lock();
        let file = slots
            .get_mut(fd as usize)
            .and_then(Option::take)
            .ok_or(Errno::EBADF)?;
        while slots.last().is_some_and(Option::is_none) {
            slots.pop();
        }
        Ok(file)
    }

    pub fn open_count(&self) -> usize {
        self.slots.lock().iter().flatten().count()
    }

    /// `dup`: another descriptor (the lowest free one) for the same open
    /// file, sharing its offset.
    pub fn dup(&self, fd: Fd) -> FsResult<Fd> {
        let file = self.get(fd)?;
        self.insert(file)
    }

    /// `dup2`: make `new` refer to the same open file as `old`, closing
    /// whatever `new` referred to. `dup2(fd, fd)` only checks `fd`.
    pub fn dup2(&self, old: Fd, new: Fd) -> FsResult<Fd> {
        let file = self.get(old)?;
        if old == new {
            return Ok(new);
        }
        let index = new as usize;
        if index >= MAX_FDS {
            return Err(Errno::EBADF);
        }
        let mut slots = self.slots.lock();
        if slots.len() <= index {
            slots.resize(index + 1, None);
        }
        // The previous file (if any) is dropped after the lock is released.
        let previous = slots[index].replace(file);
        drop(slots);
        drop(previous);
        Ok(new)
    }

    /// The child's table after `fork`: the same open files at the same
    /// descriptors, sharing offsets with the parent's (POSIX).
    pub fn fork(&self) -> Self {
        Self {
            slots: Mutex::new(self.slots.lock().clone()),
        }
    }
}

impl core::fmt::Debug for FileTable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "FileTable({} open)", self.open_count())
    }
}
