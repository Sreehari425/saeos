//! Path resolution, mounts, and the syscall-shaped file API.
//!
//! Paths are resolved from `/` (relative paths too: every task's working
//! directory is `/` until userspace adds `chdir`). `.` is skipped and `..`
//! moves to the parent, never above `/`, crossing back out of mounts. A
//! filesystem mounted on a path replaces that directory.
//!
//! The operations mirror their Linux system calls, including which errno
//! each failure returns, so the syscall layer can be a thin wrapper.

use super::errno::{Errno, FsResult};
use super::file::{Fd, FileTable, OpenFile, OpenFlags, Whence};
use super::inode::{DirEntry, FileSystem, Inode, InodeKind, NAME_MAX, Stat};
use super::lock::Mutex;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

struct Mount {
    /// Normalized absolute path ("/" or "/dev").
    path: String,
    root: Arc<dyn Inode>,
    filesystem: Arc<dyn FileSystem>,
}

pub struct Vfs {
    mounts: Mutex<Vec<Mount>>,
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new()
    }
}

/// A directory plus the final name of a path, for operations that create or
/// remove an entry.
struct Parent {
    directory: Arc<dyn Inode>,
    name: String,
    /// Normalized path of the entry itself.
    path: String,
}

fn join(parent: &str, name: &str) -> String {
    if parent == "/" {
        let mut path = String::from("/");
        path.push_str(name);
        path
    } else {
        let mut path = String::from(parent);
        path.push('/');
        path.push_str(name);
        path
    }
}

impl Vfs {
    pub const fn new() -> Self {
        Self {
            mounts: Mutex::new(Vec::new()),
        }
    }

    /// Mount `filesystem` at `path`. The first mount must be `/`; later
    /// mount points must be existing directories.
    pub fn mount(&self, path: &str, filesystem: Arc<dyn FileSystem>) -> FsResult<()> {
        let normalized = if self.mounts.lock().is_empty() {
            if path != "/" {
                return Err(Errno::ENOENT);
            }
            String::from("/")
        } else {
            let (inode, normalized) = self.walk(path)?;
            if inode.kind() != InodeKind::Directory {
                return Err(Errno::ENOTDIR);
            }
            normalized
        };
        let mut mounts = self.mounts.lock();
        if mounts.iter().any(|mount| mount.path == normalized) {
            return Err(Errno::EBUSY);
        }
        let root = filesystem.root();
        mounts.push(Mount {
            path: normalized,
            root,
            filesystem,
        });
        Ok(())
    }

    /// `(mount path, filesystem name)` for every mount.
    pub fn mounts(&self) -> Vec<(String, &'static str)> {
        self.mounts
            .lock()
            .iter()
            .map(|mount| (mount.path.clone(), mount.filesystem.name()))
            .collect()
    }

    fn mounted_root(&self, path: &str) -> Option<Arc<dyn Inode>> {
        self.mounts
            .lock()
            .iter()
            .find(|mount| mount.path == path)
            .map(|mount| mount.root.clone())
    }

    fn is_mount_point(&self, path: &str) -> bool {
        self.mounts.lock().iter().any(|mount| mount.path == path)
    }

    /// Resolve a user-supplied `path`; empty paths do not exist (Linux).
    fn walk(&self, path: &str) -> FsResult<(Arc<dyn Inode>, String)> {
        if path.is_empty() {
            return Err(Errno::ENOENT);
        }
        self.walk_from_root(path)
    }

    /// Resolve `path` to an inode and its normalized path; "" is `/`.
    fn walk_from_root(&self, path: &str) -> FsResult<(Arc<dyn Inode>, String)> {
        let root = self.mounted_root("/").ok_or(Errno::ENOENT)?;
        let mut stack: Vec<(String, Arc<dyn Inode>)> = alloc::vec![(String::from("/"), root)];
        for component in path.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    if stack.len() > 1 {
                        stack.pop();
                    }
                }
                name => {
                    if name.len() > NAME_MAX {
                        return Err(Errno::ENAMETOOLONG);
                    }
                    let (current_path, current) = stack.last().expect("walk stack is never empty");
                    if current.kind() != InodeKind::Directory {
                        return Err(Errno::ENOTDIR);
                    }
                    let child_path = join(current_path, name);
                    let child = match self.mounted_root(&child_path) {
                        Some(mounted) => {
                            // The mount point must still exist underneath.
                            current.lookup(name)?;
                            mounted
                        }
                        None => current.lookup(name)?,
                    };
                    stack.push((child_path, child));
                }
            }
        }
        let (normalized, inode) = stack.pop().expect("walk stack is never empty");
        if path.ends_with('/') && inode.kind() != InodeKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        Ok((inode, normalized))
    }

    /// Resolve all but the last component. Paths that end in `.`, `..`, or
    /// name `/` itself have no entry to create or remove: `EINVAL`, or
    /// `EBUSY` for the root.
    fn parent(&self, path: &str) -> FsResult<Parent> {
        if path.is_empty() {
            return Err(Errno::ENOENT);
        }
        let trimmed = path.trim_end_matches('/');
        let (directory_path, name) = match trimmed.rfind('/') {
            Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
            None => ("", trimmed),
        };
        if name.is_empty() {
            return Err(Errno::EBUSY);
        }
        if name == "." || name == ".." {
            return Err(Errno::EINVAL);
        }
        if name.len() > NAME_MAX {
            return Err(Errno::ENAMETOOLONG);
        }
        let (directory, directory_normalized) = self.walk_from_root(directory_path)?;
        if directory.kind() != InodeKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        Ok(Parent {
            path: join(&directory_normalized, name),
            directory,
            name: name.to_string(),
        })
    }

    pub fn stat(&self, path: &str) -> FsResult<Stat> {
        self.walk(path).map(|(inode, _)| inode.stat())
    }

    pub fn open(&self, files: &FileTable, path: &str, flags: OpenFlags) -> FsResult<Fd> {
        if !flags.contains(OpenFlags::READ) && !flags.contains(OpenFlags::WRITE) {
            return Err(Errno::EINVAL);
        }
        let inode = match self.walk(path) {
            Ok((inode, _)) => {
                if flags.contains(OpenFlags::CREATE | OpenFlags::EXCL) {
                    return Err(Errno::EEXIST);
                }
                inode
            }
            Err(Errno::ENOENT) if flags.contains(OpenFlags::CREATE) => {
                let parent = self.parent(path)?;
                match parent.directory.create(&parent.name, InodeKind::File) {
                    Ok(inode) => inode,
                    // Lost a race with another creator.
                    Err(Errno::EEXIST) if !flags.contains(OpenFlags::EXCL) => {
                        parent.directory.lookup(&parent.name)?
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        match inode.kind() {
            InodeKind::Directory => {
                if flags.contains(OpenFlags::WRITE) || flags.contains(OpenFlags::TRUNC) {
                    return Err(Errno::EISDIR);
                }
            }
            _ if flags.contains(OpenFlags::DIRECTORY) => return Err(Errno::ENOTDIR),
            InodeKind::File => {
                if flags.contains(OpenFlags::TRUNC) && flags.contains(OpenFlags::WRITE) {
                    inode.truncate(0)?;
                }
            }
            InodeKind::CharDevice => {}
        }
        files.insert(OpenFile::new(inode, flags))
    }

    pub fn close(&self, files: &FileTable, fd: Fd) -> FsResult<()> {
        files.remove(fd).map(|_| ())
    }

    pub fn read(&self, files: &FileTable, fd: Fd, buf: &mut [u8]) -> FsResult<usize> {
        let file = files.get(fd)?;
        if !file.flags.contains(OpenFlags::READ) {
            return Err(Errno::EBADF);
        }
        if file.inode.kind() == InodeKind::Directory {
            return Err(Errno::EISDIR);
        }
        let mut offset = file.offset.lock();
        let count = file.inode.read_at(*offset, buf)?;
        *offset += count as u64;
        Ok(count)
    }

    pub fn write(&self, files: &FileTable, fd: Fd, buf: &[u8]) -> FsResult<usize> {
        let file = files.get(fd)?;
        if !file.flags.contains(OpenFlags::WRITE) {
            return Err(Errno::EBADF);
        }
        let mut offset = file.offset.lock();
        if file.flags.contains(OpenFlags::APPEND) {
            *offset = file.inode.stat().size;
        }
        let count = file.inode.write_at(*offset, buf)?;
        *offset += count as u64;
        Ok(count)
    }

    /// Positional read that leaves the file offset alone (`pread`).
    pub fn read_at(
        &self,
        files: &FileTable,
        fd: Fd,
        offset: u64,
        buf: &mut [u8],
    ) -> FsResult<usize> {
        let file = files.get(fd)?;
        if !file.flags.contains(OpenFlags::READ) {
            return Err(Errno::EBADF);
        }
        match file.inode.kind() {
            InodeKind::Directory => Err(Errno::EISDIR),
            InodeKind::CharDevice => Err(Errno::ESPIPE),
            InodeKind::File => file.inode.read_at(offset, buf),
        }
    }

    pub fn lseek(&self, files: &FileTable, fd: Fd, offset: i64, whence: Whence) -> FsResult<u64> {
        let file = files.get(fd)?;
        if file.inode.kind() == InodeKind::CharDevice {
            return Err(Errno::ESPIPE);
        }
        let mut position = file.offset.lock();
        let base = match whence {
            Whence::Set => 0,
            Whence::Current => *position as i64,
            Whence::End => file.inode.stat().size as i64,
        };
        let target = base.checked_add(offset).ok_or(Errno::EINVAL)?;
        if target < 0 {
            return Err(Errno::EINVAL);
        }
        *position = target as u64;
        Ok(*position)
    }

    pub fn fstat(&self, files: &FileTable, fd: Fd) -> FsResult<Stat> {
        Ok(files.get(fd)?.inode.stat())
    }

    pub fn ftruncate(&self, files: &FileTable, fd: Fd, size: u64) -> FsResult<()> {
        let file = files.get(fd)?;
        if !file.flags.contains(OpenFlags::WRITE) {
            return Err(Errno::EBADF);
        }
        match file.inode.kind() {
            InodeKind::Directory => Err(Errno::EISDIR),
            InodeKind::CharDevice => Err(Errno::EINVAL),
            InodeKind::File => file.inode.truncate(size),
        }
    }

    /// The directory's entries from the current position on; advances the
    /// position past them (`getdents`). `.` and `..` are not listed.
    pub fn readdir(&self, files: &FileTable, fd: Fd) -> FsResult<Vec<DirEntry>> {
        let file = files.get(fd)?;
        if file.inode.kind() != InodeKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        let mut position = file.offset.lock();
        let entries = file.inode.entries()?;
        let remaining: Vec<DirEntry> = entries.into_iter().skip(*position as usize).collect();
        *position += remaining.len() as u64;
        Ok(remaining)
    }

    /// Like [`Self::readdir`], but offers entries one at a time, with each
    /// entry's position, and advances only past those `take` accepts. The
    /// first entry it refuses (say, because a `getdents` buffer is full)
    /// stays next.
    pub fn readdir_with(
        &self,
        files: &FileTable,
        fd: Fd,
        mut take: impl FnMut(u64, &DirEntry) -> bool,
    ) -> FsResult<()> {
        let file = files.get(fd)?;
        if file.inode.kind() != InodeKind::Directory {
            return Err(Errno::ENOTDIR);
        }
        let mut position = file.offset.lock();
        for entry in file.inode.entries()?.iter().skip(*position as usize) {
            if !take(*position, entry) {
                break;
            }
            *position += 1;
        }
        Ok(())
    }

    pub fn mkdir(&self, path: &str) -> FsResult<()> {
        let parent = match self.parent(path) {
            // `mkdir /` and `mkdir /a/..`: the target exists.
            Err(Errno::EBUSY | Errno::EINVAL) => return Err(Errno::EEXIST),
            result => result?,
        };
        parent
            .directory
            .create(&parent.name, InodeKind::Directory)
            .map(|_| ())
    }

    pub fn unlink(&self, path: &str) -> FsResult<()> {
        let parent = match self.parent(path) {
            Err(Errno::EBUSY) => return Err(Errno::EISDIR),
            result => result?,
        };
        if self.is_mount_point(&parent.path) {
            return Err(Errno::EBUSY);
        }
        parent.directory.unlink(&parent.name)
    }

    pub fn rmdir(&self, path: &str) -> FsResult<()> {
        let parent = self.parent(path)?;
        if self.is_mount_point(&parent.path) {
            return Err(Errno::EBUSY);
        }
        parent.directory.rmdir(&parent.name)
    }

    pub fn rename(&self, old: &str, new: &str) -> FsResult<()> {
        let source = self.parent(old)?;
        let target = self.parent(new)?;
        if self.is_mount_point(&source.path) || self.is_mount_point(&target.path) {
            return Err(Errno::EBUSY);
        }
        if source.directory.stat().fs_id != target.directory.stat().fs_id {
            return Err(Errno::EXDEV);
        }
        source
            .directory
            .rename(&source.name, &target.directory, &target.name)
    }

    /// The inode at `path` (for in-kernel users that hold no descriptor).
    pub fn lookup(&self, path: &str) -> FsResult<Arc<dyn Inode>> {
        self.walk(path).map(|(inode, _)| inode)
    }
}
