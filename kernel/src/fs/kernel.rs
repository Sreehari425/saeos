//! The kernel's filesystem instance: the global VFS, boot-time mounts, and
//! the file API on behalf of the current process.

use super::devfs::DevFs;
use super::errno::{Errno, FsResult};
use super::file::{Fd, FileTable, OpenFlags, Whence};
use super::initramfs;
use super::inode::{DirEntry, Stat};
use super::ramfs::RamFs;
use super::vfs::Vfs;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// The archive built from the repository's `initramfs/` directory by
/// `kernel/build.rs`.
pub static INITRAMFS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/initramfs.tar"));

/// `(path, size, FNV-1a 64)` of every file in the embedded archive.
pub static INITRAMFS_MANIFEST: &[(&str, u64, u64)] =
    include!(concat!(env!("OUT_DIR"), "/initramfs_manifest.rs"));

pub static VFS: Vfs = Vfs::new();

/// Mount ramfs at `/`, unpack the initramfs into it, and mount devfs at
/// `/dev`. Needs the heap.
pub fn init() {
    let root = RamFs::new();
    VFS.mount("/", root.clone()).expect("mount ramfs at /");
    match initramfs::unpack(&super::FileSystem::root(&*root), INITRAMFS) {
        Ok((files, bytes)) => crate::kinfo!("fs: initramfs {} files, {} bytes", files, bytes),
        Err(error) => crate::kerror!("fs: initramfs rejected: {:?}", error),
    }
    if let Err(error) = VFS.mkdir("/dev").or_else(|error| match error {
        Errno::EEXIST => Ok(()),
        error => Err(error),
    }) {
        crate::kerror!("fs: cannot create /dev: {}", error);
    }
    if let Err(error) = VFS.mount("/dev", DevFs::new()) {
        crate::kerror!("fs: cannot mount devfs: {}", error);
    }
}

/// The current task's process file table. Taken under a brief process-table
/// lock and released before any I/O, so file operations never run with
/// interrupts masked.
pub fn current_files() -> FsResult<Arc<FileTable>> {
    let task = crate::scheduler::current().ok_or(Errno::EBADF)?;
    let process = crate::scheduler::SCHEDULER
        .lock()
        .task(task)
        .and_then(|task| task.process_id)
        .ok_or(Errno::EBADF)?;
    crate::scheduler::PROCESSES
        .lock()
        .process(process)
        .map(|process| process.files.clone())
        .ok_or(Errno::EBADF)
}

pub fn open(path: &str, flags: OpenFlags) -> FsResult<Fd> {
    VFS.open(&*current_files()?, path, flags)
}

pub fn close(fd: Fd) -> FsResult<()> {
    VFS.close(&*current_files()?, fd)
}

pub fn read(fd: Fd, buf: &mut [u8]) -> FsResult<usize> {
    VFS.read(&*current_files()?, fd, buf)
}

pub fn write(fd: Fd, buf: &[u8]) -> FsResult<usize> {
    VFS.write(&*current_files()?, fd, buf)
}

pub fn lseek(fd: Fd, offset: i64, whence: Whence) -> FsResult<u64> {
    VFS.lseek(&*current_files()?, fd, offset, whence)
}

pub fn fstat(fd: Fd) -> FsResult<Stat> {
    VFS.fstat(&*current_files()?, fd)
}

pub fn readdir(fd: Fd) -> FsResult<Vec<DirEntry>> {
    VFS.readdir(&*current_files()?, fd)
}

pub fn stat(path: &str) -> FsResult<Stat> {
    VFS.stat(path)
}

pub fn mkdir(path: &str) -> FsResult<()> {
    VFS.mkdir(path)
}

pub fn unlink(path: &str) -> FsResult<()> {
    VFS.unlink(path)
}

pub fn rmdir(path: &str) -> FsResult<()> {
    VFS.rmdir(path)
}

pub fn rename(old: &str, new: &str) -> FsResult<()> {
    VFS.rename(old, new)
}
