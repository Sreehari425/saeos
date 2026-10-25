//! An in-memory filesystem: the root filesystem that the initramfs is
//! unpacked into, and the reference implementation of the `Inode` traits.
//!
//! Files are byte vectors and directories are sorted maps, so listings are
//! deterministic. File bytes count against a per-mount limit; growth uses
//! `try_reserve`, so running out of space is `ENOSPC` rather than a panic.
//!
//! Lock order: a directory's entry map before any of its descendants'. A
//! rename between two directories holds the per-filesystem rename lock (so
//! the tree's shape cannot change underneath it) and locks an ancestor before
//! its descendant, or the lower inode number first when they are unrelated,
//! as Linux's `lock_rename` does. Parent links have their own lock and are
//! only read with entry maps unlocked.

use super::errno::{Errno, FsResult};
use super::inode::{DirEntry, FileSystem, Inode, InodeKind, Stat, check_name, new_fs_id};
use super::lock::Mutex;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Default byte limit for a ramfs mount.
pub const DEFAULT_LIMIT: u64 = 64 << 20;

struct Shared {
    fs_id: u64,
    next_ino: AtomicU64,
    used: AtomicU64,
    limit: u64,
    /// Serializes renames: only rename changes which directory is whose
    /// parent, so ancestry checks stay valid while it is held.
    rename_lock: Mutex<()>,
}

impl Shared {
    fn allocate_ino(&self) -> u64 {
        self.next_ino.fetch_add(1, Ordering::Relaxed)
    }

    /// Account for `bytes` more file data, or fail with `ENOSPC`.
    fn reserve(&self, bytes: u64) -> FsResult<()> {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|total| *total <= self.limit)
            })
            .map(|_| ())
            .map_err(|_| Errno::ENOSPC)
    }

    fn release(&self, bytes: u64) {
        self.used.fetch_sub(bytes, Ordering::AcqRel);
    }
}

pub struct RamFs {
    root: Arc<RamDir>,
    shared: Arc<Shared>,
}

impl RamFs {
    pub fn new() -> Arc<Self> {
        Self::with_limit(DEFAULT_LIMIT)
    }

    pub fn with_limit(limit: u64) -> Arc<Self> {
        let shared = Arc::new(Shared {
            fs_id: new_fs_id(),
            next_ino: AtomicU64::new(1),
            used: AtomicU64::new(0),
            limit,
            rename_lock: Mutex::new(()),
        });
        let root = RamDir::new(&shared, Weak::new());
        Arc::new(Self { root, shared })
    }

    /// File bytes currently stored.
    pub fn used_bytes(&self) -> u64 {
        self.shared.used.load(Ordering::Acquire)
    }
}

impl FileSystem for RamFs {
    fn root(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    fn name(&self) -> &'static str {
        "ramfs"
    }
}

pub struct RamFile {
    ino: u64,
    shared: Arc<Shared>,
    data: Mutex<Vec<u8>>,
}

impl Drop for RamFile {
    fn drop(&mut self) {
        // The last reference (directory entry or open file) is gone.
        let length = self.data.lock().len() as u64;
        self.shared.release(length);
    }
}

impl RamFile {
    /// Resize to `new_length`, keeping the byte accounting exact.
    fn resize(&self, data: &mut Vec<u8>, new_length: usize) -> FsResult<()> {
        let old_length = data.len();
        if new_length > old_length {
            let grow = new_length - old_length;
            self.shared.reserve(grow as u64)?;
            if data.try_reserve(grow).is_err() {
                self.shared.release(grow as u64);
                return Err(Errno::ENOSPC);
            }
            data.resize(new_length, 0);
        } else {
            data.truncate(new_length);
            data.shrink_to_fit();
            self.shared.release((old_length - new_length) as u64);
        }
        Ok(())
    }
}

impl Inode for RamFile {
    fn stat(&self) -> Stat {
        Stat {
            ino: self.ino,
            fs_id: self.shared.fs_id,
            kind: InodeKind::File,
            size: self.data.lock().len() as u64,
            nlink: 1,
        }
    }

    fn kind(&self) -> InodeKind {
        InodeKind::File
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let data = self.data.lock();
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        if start >= data.len() {
            return Ok(0);
        }
        let count = buf.len().min(data.len() - start);
        buf[..count].copy_from_slice(&data[start..start + count]);
        Ok(count)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> FsResult<usize> {
        let start = usize::try_from(offset).map_err(|_| Errno::EINVAL)?;
        let end = start.checked_add(buf.len()).ok_or(Errno::EINVAL)?;
        let mut data = self.data.lock();
        if end > data.len() {
            // Bytes between the old end and `start` read back as zeros.
            self.resize(&mut data, end)?;
        }
        data[start..end].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn truncate(&self, size: u64) -> FsResult<()> {
        let size = usize::try_from(size).map_err(|_| Errno::EINVAL)?;
        let mut data = self.data.lock();
        self.resize(&mut data, size)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct RamDir {
    ino: u64,
    shared: Arc<Shared>,
    this: Weak<RamDir>,
    parent: Mutex<Weak<RamDir>>,
    entries: Mutex<BTreeMap<String, Arc<dyn Inode>>>,
    /// Set (with `entries` locked) when the directory is removed, so a task
    /// that already resolved it cannot create entries in it afterwards.
    dead: AtomicBool,
}

impl RamDir {
    fn new(shared: &Arc<Shared>, parent: Weak<RamDir>) -> Arc<Self> {
        let ino = shared.allocate_ino();
        Arc::new_cyclic(|this| Self {
            ino,
            shared: shared.clone(),
            this: this.clone(),
            parent: Mutex::new(parent),
            entries: Mutex::new(BTreeMap::new()),
            dead: AtomicBool::new(false),
        })
    }

    /// Whether `self` is `ancestor` or lies below it.
    fn is_within(&self, ancestor: u64) -> bool {
        let mut current = self.this.upgrade();
        while let Some(directory) = current {
            if directory.ino == ancestor {
                return true;
            }
            current = directory.parent.lock().upgrade();
        }
        false
    }

    fn is_dead(&self) -> bool {
        self.dead.load(Ordering::Acquire)
    }
}

impl Inode for RamDir {
    fn stat(&self) -> Stat {
        let subdirectories = self
            .entries
            .lock()
            .values()
            .filter(|entry| entry.kind() == InodeKind::Directory)
            .count() as u32;
        Stat {
            ino: self.ino,
            fs_id: self.shared.fs_id,
            kind: InodeKind::Directory,
            size: 0,
            nlink: 2 + subdirectories,
        }
    }

    fn kind(&self) -> InodeKind {
        InodeKind::Directory
    }

    fn lookup(&self, name: &str) -> FsResult<Arc<dyn Inode>> {
        self.entries.lock().get(name).cloned().ok_or(Errno::ENOENT)
    }

    fn create(&self, name: &str, kind: InodeKind) -> FsResult<Arc<dyn Inode>> {
        check_name(name)?;
        let mut entries = self.entries.lock();
        if self.is_dead() {
            return Err(Errno::ENOENT);
        }
        if entries.contains_key(name) {
            return Err(Errno::EEXIST);
        }
        let inode: Arc<dyn Inode> = match kind {
            InodeKind::File => Arc::new(RamFile {
                ino: self.shared.allocate_ino(),
                shared: self.shared.clone(),
                data: Mutex::new(Vec::new()),
            }),
            InodeKind::Directory => RamDir::new(&self.shared, self.this.clone()),
            // Device nodes live in devfs.
            InodeKind::CharDevice => return Err(Errno::EINVAL),
        };
        entries.insert(name.to_string(), inode.clone());
        Ok(inode)
    }

    fn unlink(&self, name: &str) -> FsResult<()> {
        let mut entries = self.entries.lock();
        match entries.get(name) {
            None => Err(Errno::ENOENT),
            Some(entry) if entry.kind() == InodeKind::Directory => Err(Errno::EISDIR),
            Some(_) => {
                // Open files keep their own reference until closed.
                entries.remove(name);
                Ok(())
            }
        }
    }

    fn rmdir(&self, name: &str) -> FsResult<()> {
        let mut entries = self.entries.lock();
        let entry = entries.get(name).ok_or(Errno::ENOENT)?;
        let directory = entry
            .as_any()
            .downcast_ref::<RamDir>()
            .ok_or(Errno::ENOTDIR)?;
        // Hold the child's map across the check and the removal so nothing
        // can be created in it in between.
        let children = directory.entries.lock();
        if !children.is_empty() {
            return Err(Errno::ENOTEMPTY);
        }
        directory.dead.store(true, Ordering::Release);
        drop(children);
        entries.remove(name);
        Ok(())
    }

    fn rename(&self, old: &str, new_dir: &Arc<dyn Inode>, new: &str) -> FsResult<()> {
        check_name(new)?;
        let target_dir = new_dir
            .as_any()
            .downcast_ref::<RamDir>()
            .ok_or(Errno::EXDEV)?;
        if target_dir.shared.fs_id != self.shared.fs_id {
            return Err(Errno::EXDEV);
        }
        let same_dir = target_dir.ino == self.ino;
        let _rename = self.shared.rename_lock.lock();
        if same_dir && old == new {
            return self.lookup(old).map(|_| ());
        }
        if target_dir.is_dead() {
            return Err(Errno::ENOENT);
        }
        // A directory cannot move into itself or below itself. Ancestry is
        // stable while the rename lock is held.
        let source = self.lookup(old)?;
        if let Some(moved) = source.as_any().downcast_ref::<RamDir>()
            && target_dir.is_within(moved.ino)
        {
            return Err(Errno::EINVAL);
        }
        let source_first = if target_dir.is_within(self.ino) {
            true
        } else if self.is_within(target_dir.ino) {
            false
        } else {
            self.ino < target_dir.ino
        };
        let (mut source_entries, mut target_entries) = if same_dir {
            (self.entries.lock(), None)
        } else if source_first {
            let source_entries = self.entries.lock();
            (source_entries, Some(target_dir.entries.lock()))
        } else {
            let target_entries = target_dir.entries.lock();
            (self.entries.lock(), Some(target_entries))
        };
        let source = source_entries.get(old).cloned().ok_or(Errno::ENOENT)?;
        let targets = target_entries.as_deref_mut().unwrap_or(&mut source_entries);
        if let Some(existing) = targets.get(new) {
            match (source.kind(), existing.kind()) {
                (InodeKind::Directory, InodeKind::Directory) => {
                    // The replaced directory must be empty, and stays empty:
                    // it is marked dead while its map is locked.
                    if let Some(existing_dir) = existing.as_any().downcast_ref::<RamDir>() {
                        // Replacing the source's own directory (`/a/b` onto
                        // `/a`): it holds the source, so it is not empty, and
                        // its map is already locked here.
                        if existing_dir.ino == self.ino {
                            return Err(Errno::ENOTEMPTY);
                        }
                        let children = existing_dir.entries.lock();
                        if !children.is_empty() {
                            return Err(Errno::ENOTEMPTY);
                        }
                        existing_dir.dead.store(true, Ordering::Release);
                    }
                }
                (InodeKind::Directory, _) => return Err(Errno::ENOTDIR),
                (_, InodeKind::Directory) => return Err(Errno::EISDIR),
                _ => {}
            }
        }
        targets.insert(new.to_string(), source.clone());
        source_entries.remove(old);
        if let Some(moved) = source.as_any().downcast_ref::<RamDir>() {
            *moved.parent.lock() = target_dir.this.clone();
        }
        Ok(())
    }

    fn entries(&self) -> FsResult<Vec<DirEntry>> {
        Ok(self
            .entries
            .lock()
            .iter()
            .map(|(name, inode)| {
                let stat = inode.stat();
                DirEntry {
                    name: name.clone(),
                    ino: stat.ino,
                    kind: stat.kind,
                }
            })
            .collect())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
