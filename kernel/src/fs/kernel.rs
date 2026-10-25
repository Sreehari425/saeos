//! The kernel's filesystem instance: the global VFS, boot-time mounts, and
//! the file API on behalf of the current process.

use super::devfs::DevFs;
use super::errno::{Errno, FsResult};
use super::file::{Fd, FileTable, OpenFlags, Whence};
use super::initramfs;
use super::inode::{DirEntry, FileSystem, Stat};
use super::procfs::{Generator, ProcFs};
use super::ramfs::RamFs;
use super::vfs::Vfs;
use crate::boot::boot_info::{BootInfo, PhysicalRange};
use crate::mm::PhysAddr;
use crate::mm::paging::phys_to_virt;
use alloc::sync::Arc;
use alloc::vec::Vec;

pub static VFS: Vfs = Vfs::new();

/// Mount ramfs at `/` and unpack every initramfs archive the bootloader
/// loaded into it (later archives override earlier files), then hand their
/// memory back. Then mount devfs at `/dev` and procfs at `/proc`. Needs the
/// heap.
pub fn init(boot_info: &BootInfo) {
    let root = RamFs::new();
    VFS.mount("/", root.clone()).expect("mount ramfs at /");
    let archives: Vec<PhysicalRange> = boot_info.initramfs.iter().flatten().copied().collect();
    if archives.is_empty() {
        crate::kwarn!("fs: no initramfs; / starts empty");
    }
    for range in archives {
        // Bootloader memory is ordinary RAM, reachable through the direct map.
        let archive = unsafe {
            core::slice::from_raw_parts(
                phys_to_virt(PhysAddr(range.start)).0 as *const u8,
                (range.end - range.start) as usize,
            )
        };
        match initramfs::unpack(&root.root(), archive) {
            Ok((files, bytes)) => crate::kinfo!(
                "fs: initramfs at {:#x}: {} files, {} bytes",
                range.start,
                files,
                bytes
            ),
            Err(error) => {
                crate::kerror!("fs: initramfs at {:#x} rejected: {:?}", range.start, error)
            }
        }
    }
    let held = crate::mm::frame::held_frames();
    crate::mm::release_initramfs();
    let released = held - crate::mm::frame::held_frames();
    if released != 0 {
        crate::kinfo!("fs: released {} KiB of initramfs memory", released * 4);
    }

    mount_pseudo("/dev", DevFs::new());
    mount_pseudo(
        "/proc",
        ProcFs::new(alloc::vec![
            ("cmdline", generator(proc_cmdline)),
            ("mounts", generator(proc_mounts)),
        ]),
    );
}

fn generator(function: fn() -> Vec<u8>) -> Generator {
    Arc::new(function)
}

/// Create `path` (unless the initramfs did) and mount `filesystem` there.
fn mount_pseudo(path: &str, filesystem: Arc<dyn FileSystem>) {
    if let Err(error) = VFS.mkdir(path).or_else(|error| match error {
        Errno::EEXIST => Ok(()),
        error => Err(error),
    }) {
        crate::kerror!("fs: cannot create {}: {}", path, error);
    }
    if let Err(error) = VFS.mount(path, filesystem) {
        crate::kerror!("fs: cannot mount {}: {}", path, error);
    }
}

/// `/proc/cmdline`: the command line and a newline, exactly as Linux.
fn proc_cmdline() -> Vec<u8> {
    let mut contents = Vec::from(crate::cmdline::raw().as_bytes());
    contents.push(b'\n');
    contents
}

/// `/proc/mounts`: `device mountpoint type options 0 0`, as Linux.
fn proc_mounts() -> Vec<u8> {
    let mut contents = alloc::string::String::new();
    for (path, name) in VFS.mounts() {
        contents.push_str(&alloc::format!("{name} {path} {name} rw 0 0\n"));
    }
    contents.into_bytes()
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
