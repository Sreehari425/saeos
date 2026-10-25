//! System calls, with Linux x86_64 numbers and calling convention:
//!
//! ```text
//! rax = number, args in rdi rsi rdx r10 r8 r9
//! result in rax: >= 0 on success, -errno on failure
//! ```
//!
//! [`dispatch`] is the whole table. It reaches user memory only through a
//! [`UserMemory`] and the process only through [`ProcessOps`], so the host
//! model runs it against a fake address space, and the kernel against the
//! current process. The `SYSCALL` instruction entry (later, with ring 3)
//! only has to call [`dispatch_current`]; until then kernel tasks call it
//! directly.

pub mod abi;
pub mod memory;
pub mod numbers;

use crate::fs::vfs::Vfs;
use crate::fs::{Errno, FileTable, InodeKind};
use alloc::string::String;
// The model includes `uaccess.rs` at the crate root rather than under `mm`.
#[cfg(not(host_model))]
use crate::mm::uaccess::{UserMemory, check_range};
#[cfg(not(host_model))]
use crate::mm::vma::Protection;
#[cfg(host_model)]
use crate::uaccess::{UserMemory, check_range};
#[cfg(host_model)]
use crate::vma::Protection;
use alloc::vec::Vec;

/// Longest path accepted, including the NUL (Linux's `PATH_MAX`).
pub const PATH_MAX: usize = 4096;

/// Largest single read or write; longer requests are short transfers, as
/// POSIX allows.
const MAX_TRANSFER: usize = 64 * 1024;

/// What the process side of the kernel provides to syscalls.
pub trait ProcessOps {
    fn getpid(&self) -> u64;
    fn getppid(&self) -> u64;
    /// Wait for a child (`None`: any child) to exit; returns its pid and
    /// exit status, or `None` with `no_hang` when none has exited yet.
    fn wait(&self, pid: Option<u64>, no_hang: bool) -> Result<Option<(u64, i32)>, Errno>;
    /// `fork`: the child's pid (the child itself sees 0).
    fn fork(&self) -> Result<u64, Errno>;
    /// Replace the program; on success the syscall returns into the new
    /// one. On failure the caller's program is untouched.
    fn execve(&self, path: &str, argv: &[&str], envp: &[&str]) -> Result<(), Errno>;
    /// The current directory (absolute, normalised).
    fn cwd(&self) -> String;
    fn set_cwd(&self, path: String);
    /// Move the program break; returns the new break, or the old one if it
    /// could not move.
    fn brk(&self, address: u64) -> u64;
    /// Map anonymous private memory; returns its address.
    fn mmap(
        &self,
        hint: u64,
        length: u64,
        protection: Protection,
        fixed: bool,
    ) -> Result<u64, Errno>;
    fn munmap(&self, start: u64, length: u64) -> Result<(), Errno>;
    fn mprotect(&self, start: u64, length: u64, protection: Protection) -> Result<(), Errno>;
    /// The calling thread's FS base (already validated as a user address).
    fn set_fs_base(&self, base: u64);
    fn fs_base(&self) -> u64;
}

pub struct Context<'a, M: UserMemory> {
    pub memory: &'a M,
    pub vfs: &'a Vfs,
    pub files: &'a FileTable,
    pub process: &'a dyn ProcessOps,
}

/// The result of a syscall. `exit` does not return to the caller; the
/// caller (kernel glue) ends the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Return(i64),
    Exit(i32),
}

fn result(value: Result<u64, Errno>) -> Outcome {
    Outcome::Return(match value {
        Ok(value) => value as i64,
        Err(error) => -i64::from(error.code()),
    })
}

/// Run syscall `number` with `args`.
pub fn dispatch<M: UserMemory>(context: &Context<M>, number: u64, args: [u64; 6]) -> Outcome {
    use numbers::*;
    let [a0, a1, a2, a3, a4, a5] = args;
    let value = match number {
        READ => read(context, a0, a1, a2),
        WRITE => write(context, a0, a1, a2),
        OPEN => open(context, a0, a1),
        CLOSE => fd(a0).and_then(|fd| context.vfs.close(context.files, fd).map(|()| 0)),
        STAT => stat(context, a0, a1),
        FSTAT => fd(a0).and_then(|fd| {
            let stat = context.vfs.fstat(context.files, fd)?;
            context.memory.write(a1, &abi::encode_stat(&stat))?;
            Ok(0)
        }),
        LSEEK => lseek(context, a0, a1, a2),
        MMAP => memory::mmap(context.process, a0, a1, a2, a3, a4, a5),
        MPROTECT => memory::mprotect(context.process, a0, a1, a2),
        MUNMAP => memory::munmap(context.process, a0, a1),
        BRK => Ok(memory::brk(context.process, a0)),
        ARCH_PRCTL => memory::arch_prctl(context, a0, a1),
        GETPID => Ok(context.process.getpid()),
        GETPPID => Ok(context.process.getppid()),
        DUP => fd(a0).and_then(|fd| context.files.dup(fd).map(u64::from)),
        DUP2 => fd(a0)
            .and_then(|old| Ok((old, fd(a1)?)))
            .and_then(|(old, new)| context.files.dup2(old, new).map(u64::from)),
        FORK => context.process.fork(),
        EXECVE => execve(context, a0, a1, a2),
        GETCWD => getcwd(context, a0, a1),
        CHDIR => chdir(context, a0),
        EXIT | EXIT_GROUP => return Outcome::Exit((a0 & 0xff) as i32),
        WAIT4 => wait4(context, a0, a1, a2, a3),
        RENAME => path(context, a0)
            .and_then(|old| Ok((old, path(context, a1)?)))
            .and_then(|(old, new)| context.vfs.rename(&old, &new).map(|()| 0)),
        MKDIR => path(context, a0).and_then(|path| context.vfs.mkdir(&path).map(|()| 0)),
        RMDIR => path(context, a0).and_then(|path| context.vfs.rmdir(&path).map(|()| 0)),
        UNLINK => path(context, a0).and_then(|path| context.vfs.unlink(&path).map(|()| 0)),
        GETDENTS64 => getdents64(context, a0, a1, a2),
        _ => Err(Errno::ENOSYS),
    };
    result(value)
}

fn fd(value: u64) -> Result<u32, Errno> {
    u32::try_from(value).map_err(|_| Errno::EBADF)
}

/// A path argument: NUL-terminated, at most `PATH_MAX`, not empty, and
/// resolved against the current directory.
fn path<M: UserMemory>(context: &Context<M>, address: u64) -> Result<String, Errno> {
    let path = context.memory.read_cstr(address, PATH_MAX - 1)?;
    if path.is_empty() {
        return Err(Errno::ENOENT);
    }
    Ok(crate::fs::path::resolve(&context.process.cwd(), &path))
}

/// Most argv + envp entries `execve` accepts, and their total size (the
/// initial stack's argument area, as on Linux a quarter of the stack).
pub const MAX_ARGUMENTS: usize = 1024;
pub const MAX_ARGUMENT_BYTES: usize = 32 * 1024;

/// Read a NULL-terminated array of string pointers (`argv`, `envp`). A
/// NULL array is empty. `E2BIG` past the limits shared by both arrays.
pub fn read_string_array<M: UserMemory>(
    memory: &M,
    address: u64,
    budget: &mut usize,
) -> Result<Vec<String>, Errno> {
    let mut strings = Vec::new();
    if address == 0 {
        return Ok(strings);
    }
    for index in 0..=MAX_ARGUMENTS {
        if index == MAX_ARGUMENTS {
            return Err(Errno::E2BIG);
        }
        let mut pointer = [0u8; 8];
        memory.read(address + index as u64 * 8, &mut pointer)?;
        let pointer = u64::from_le_bytes(pointer);
        if pointer == 0 {
            break;
        }
        let string = memory
            .read_cstr(pointer, (*budget).min(PATH_MAX * 8))
            .map_err(|error| match error {
                Errno::ENAMETOOLONG => Errno::E2BIG,
                error => error,
            })?;
        *budget = budget.checked_sub(string.len() + 1).ok_or(Errno::E2BIG)?;
        strings.push(string);
    }
    Ok(strings)
}

fn execve<M: UserMemory>(
    context: &Context<M>,
    path_address: u64,
    argv: u64,
    envp: u64,
) -> Result<u64, Errno> {
    let path = path(context, path_address)?;
    let mut budget = MAX_ARGUMENT_BYTES;
    let argv = read_string_array(context.memory, argv, &mut budget)?;
    let envp = read_string_array(context.memory, envp, &mut budget)?;
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let envp: Vec<&str> = envp.iter().map(String::as_str).collect();
    context.process.execve(&path, &argv, &envp)?;
    Ok(0)
}

/// `getcwd(buf, size)`: the path and its NUL; returns the length written
/// (with the NUL), `ERANGE` if it does not fit.
fn getcwd<M: UserMemory>(context: &Context<M>, buffer: u64, size: u64) -> Result<u64, Errno> {
    let mut cwd = context.process.cwd().into_bytes();
    cwd.push(0);
    if (cwd.len() as u64) > size {
        return Err(Errno::ERANGE);
    }
    context.memory.write(buffer, &cwd)?;
    Ok(cwd.len() as u64)
}

fn chdir<M: UserMemory>(context: &Context<M>, path_address: u64) -> Result<u64, Errno> {
    let path = path(context, path_address)?;
    if context.vfs.stat(&path)?.kind != InodeKind::Directory {
        return Err(Errno::ENOTDIR);
    }
    context.process.set_cwd(path);
    Ok(0)
}

fn read<M: UserMemory>(
    context: &Context<M>,
    fd_value: u64,
    buffer: u64,
    count: u64,
) -> Result<u64, Errno> {
    let fd = fd(fd_value)?;
    let count = usize::try_from(count)
        .unwrap_or(usize::MAX)
        .min(MAX_TRANSFER);
    // Check the destination first, so a bad pointer does not consume data.
    check_range(buffer, count)?;
    let mut data = alloc::vec![0u8; count];
    let read = context.vfs.read(context.files, fd, &mut data)?;
    if let Err(error) = context.memory.write(buffer, &data[..read]) {
        // Nothing reached the caller, so give the data back: the position
        // only advances by what was copied, as on Linux. (Devices have no
        // position; their data is gone either way.)
        let _ = context.vfs.lseek(
            context.files,
            fd,
            -(read as i64),
            crate::fs::Whence::Current,
        );
        return Err(error);
    }
    Ok(read as u64)
}

fn write<M: UserMemory>(
    context: &Context<M>,
    fd_value: u64,
    buffer: u64,
    count: u64,
) -> Result<u64, Errno> {
    let fd = fd(fd_value)?;
    let count = usize::try_from(count)
        .unwrap_or(usize::MAX)
        .min(MAX_TRANSFER);
    check_range(buffer, count)?;
    let mut data = alloc::vec![0u8; count];
    context.memory.read(buffer, &mut data)?;
    Ok(context.vfs.write(context.files, fd, &data)? as u64)
}

fn open<M: UserMemory>(context: &Context<M>, path_address: u64, flags: u64) -> Result<u64, Errno> {
    let flags = abi::open_flags(flags)?;
    let path = path(context, path_address)?;
    Ok(u64::from(context.vfs.open(context.files, &path, flags)?))
}

fn stat<M: UserMemory>(context: &Context<M>, path_address: u64, buffer: u64) -> Result<u64, Errno> {
    let path = path(context, path_address)?;
    let stat = context.vfs.stat(&path)?;
    context.memory.write(buffer, &abi::encode_stat(&stat))?;
    Ok(0)
}

fn lseek<M: UserMemory>(
    context: &Context<M>,
    fd_value: u64,
    offset: u64,
    whence: u64,
) -> Result<u64, Errno> {
    let fd = fd(fd_value)?;
    context
        .vfs
        .lseek(context.files, fd, offset as i64, abi::whence(whence)?)
}

fn wait4<M: UserMemory>(
    context: &Context<M>,
    pid: u64,
    status: u64,
    options: u64,
    _rusage: u64,
) -> Result<u64, Errno> {
    // WUNTRACED, WCONTINUED and the like come with signals.
    const WNOHANG: u64 = 1;
    if options & !WNOHANG != 0 {
        return Err(Errno::EINVAL);
    }
    let pid = match pid as i64 {
        -1 => None,
        pid if pid > 0 => Some(pid as u64),
        // Process groups do not exist yet.
        _ => return Err(Errno::EINVAL),
    };
    if status != 0 {
        check_range(status, 4)?;
    }
    let Some((child, code)) = context.process.wait(pid, options & WNOHANG != 0)? else {
        // WNOHANG and no child has exited yet.
        return Ok(0);
    };
    if status != 0 {
        context
            .memory
            .write(status, &abi::exit_status_word(code).to_le_bytes())?;
    }
    Ok(child)
}

fn getdents64<M: UserMemory>(
    context: &Context<M>,
    fd_value: u64,
    buffer: u64,
    count: u64,
) -> Result<u64, Errno> {
    let fd = fd(fd_value)?;
    let count = usize::try_from(count)
        .unwrap_or(usize::MAX)
        .min(MAX_TRANSFER);
    check_range(buffer, count)?;
    let mut encoded = Vec::new();
    let mut too_small = false;
    // Take entries only while they fit, so the next call resumes after the
    // last one returned.
    context
        .vfs
        .readdir_with(context.files, fd, |position, entry| {
            if encoded.len() + abi::dirent_size(&entry.name) > count {
                too_small = encoded.is_empty();
                return false;
            }
            abi::encode_dirent(&mut encoded, entry, position + 1);
            true
        })?;
    if too_small {
        return Err(Errno::EINVAL);
    }
    context.memory.write(buffer, &encoded)?;
    Ok(encoded.len() as u64)
}

#[cfg(not(host_model))]
mod kernel {
    use super::{Context, Outcome, ProcessOps, Protection, dispatch};
    use crate::arch::x86_64::syscall::UserFrame;
    use crate::fs::Errno;
    use crate::mm::uaccess::KernelUserMemory;
    use crate::mm::user_memory;
    use crate::task::ProcessId;

    struct CurrentProcess;

    impl ProcessOps for CurrentProcess {
        fn getpid(&self) -> u64 {
            current_process().map_or(0, |process| process.0)
        }

        fn getppid(&self) -> u64 {
            let Some(process) = current_process() else {
                return 0;
            };
            crate::scheduler::PROCESSES
                .lock()
                .process(process)
                .and_then(|record| record.parent)
                .map_or(0, |parent| parent.0)
        }

        fn wait(&self, pid: Option<u64>, no_hang: bool) -> Result<Option<(u64, i32)>, Errno> {
            crate::scheduler::wait_for_child(pid.map(ProcessId), no_hang)
                .map(|ready| ready.map(|(child, status)| (child.0, status.0)))
                .map_err(|_| Errno::ECHILD)
        }

        fn fork(&self) -> Result<u64, Errno> {
            let frame = current_user_frame().ok_or(Errno::ENOSYS)?;
            let task = crate::scheduler::current().ok_or(Errno::ENOSYS)?;
            let (name, space, fs_base) = crate::scheduler::with_scheduler(|scheduler| {
                scheduler
                    .task(task)
                    .map(|task| (task.name, task.address_space, task.fs_base))
            })
            .ok_or(Errno::ENOSYS)?;
            let space = space.ok_or(Errno::ENOSYS)?;
            let (child_space, map) = crate::mm::address_space::with_owned_mut(space, |owned| {
                owned
                    .space
                    .fork_cow()
                    .map(|child| (child, owned.map.clone()))
            })
            .map_err(|_| Errno::ENOMEM)?
            .map_err(|_| Errno::ENOMEM)?;
            // The FPU registers are live (the kernel never touches them).
            let mut fpu = crate::task::FpuState::INITIAL;
            unsafe { crate::arch::x86_64::fpu::save(&mut fpu) };
            let process = current_process().ok_or(Errno::ENOSYS)?;
            let (files, cwd) = {
                let table = crate::scheduler::PROCESSES.lock();
                let record = table.process(process).ok_or(Errno::ENOSYS)?;
                (record.files.clone(), record.cwd.clone())
            };
            let child = crate::scheduler::spawn_forked(
                name,
                child_space,
                map,
                frame,
                fpu,
                fs_base,
                files.fork(),
                cwd,
            );
            Ok(child.0)
        }

        fn execve(&self, path: &str, argv: &[&str], envp: &[&str]) -> Result<(), Errno> {
            let frame = current_user_frame_mut().ok_or(Errno::ENOSYS)?;
            let image = crate::exec::prepare(path, argv, envp)?;
            // Past this point the old program is gone.
            let (entry, stack_pointer) = crate::exec::install_current(image);
            *frame = UserFrame {
                rip: entry,
                cs: u64::from(crate::arch::x86_64::gdt::USER_CODE),
                rflags: 0x202,
                rsp: stack_pointer,
                ss: u64::from(crate::arch::x86_64::gdt::USER_DATA),
                ..UserFrame::ZERO
            };
            Ok(())
        }

        fn cwd(&self) -> alloc::string::String {
            current_process()
                .and_then(|process| {
                    crate::scheduler::PROCESSES
                        .lock()
                        .process(process)
                        .map(|record| record.cwd.clone())
                })
                .unwrap_or_else(|| alloc::string::String::from("/"))
        }

        fn set_cwd(&self, path: alloc::string::String) {
            if let Some(process) = current_process()
                && let Some(record) = crate::scheduler::PROCESSES.lock().process_mut(process)
            {
                record.cwd = path;
            }
        }

        fn brk(&self, address: u64) -> u64 {
            current_space().map_or(0, |space| user_memory::brk(space, address))
        }

        fn mmap(
            &self,
            hint: u64,
            length: u64,
            protection: Protection,
            fixed: bool,
        ) -> Result<u64, Errno> {
            user_memory::mmap(
                current_space().ok_or(Errno::ENOMEM)?,
                hint,
                length,
                protection,
                fixed,
            )
        }

        fn munmap(&self, start: u64, length: u64) -> Result<(), Errno> {
            user_memory::munmap(current_space().ok_or(Errno::ENOMEM)?, start, length)
        }

        fn mprotect(&self, start: u64, length: u64, protection: Protection) -> Result<(), Errno> {
            user_memory::mprotect(
                current_space().ok_or(Errno::ENOMEM)?,
                start,
                length,
                protection,
            )
        }

        fn set_fs_base(&self, base: u64) {
            crate::scheduler::with_scheduler(|scheduler| {
                if let Some(task) = scheduler
                    .current()
                    .and_then(|task| scheduler.task_mut(task))
                {
                    task.fs_base = base;
                }
            });
            crate::arch::x86_64::cpu::set_fs_base(base);
        }

        fn fs_base(&self) -> u64 {
            crate::scheduler::with_scheduler(|scheduler| {
                scheduler
                    .current()
                    .and_then(|task| scheduler.task(task))
                    .map_or(0, |task| task.fs_base)
            })
        }
    }

    /// The current task's saved user registers: a syscall from user mode
    /// pushed them at the top of its kernel stack. `None` when the caller is
    /// a kernel task (the boot test calls syscalls directly).
    fn current_user_frame_mut() -> Option<&'static mut UserFrame> {
        let task = crate::scheduler::current()?;
        let top = crate::scheduler::with_scheduler(|scheduler| {
            scheduler.task(task).map(|task| task.kernel_stack.top)
        })?;
        let frame = unsafe { &mut *((top - size_of::<UserFrame>() as u64) as *mut UserFrame) };
        crate::arch::x86_64::syscall::from_user(frame.cs).then_some(frame)
    }

    fn current_user_frame() -> Option<UserFrame> {
        current_user_frame_mut().map(|frame| *frame)
    }

    /// The current process's address space (its memory map lives with it).
    fn current_space() -> Option<crate::task::AddressSpaceId> {
        crate::scheduler::with_scheduler(|scheduler| {
            scheduler
                .current()
                .and_then(|task| scheduler.task(task))
                .and_then(|task| task.address_space)
        })
    }

    fn current_process() -> Option<ProcessId> {
        crate::scheduler::with_scheduler(|scheduler| {
            scheduler
                .current()
                .and_then(|task| scheduler.task(task))
                .and_then(|task| task.process_id)
        })
    }

    /// Run a syscall for the current task against its own address space and
    /// file table. Returns the value for rax; `exit` does not return.
    pub fn dispatch_current(number: u64, args: [u64; 6]) -> i64 {
        let files = match crate::fs::current_files() {
            Ok(files) => files,
            Err(error) => return -i64::from(error.code()),
        };
        let context = Context {
            memory: &KernelUserMemory,
            vfs: &crate::fs::VFS,
            files: &files,
            process: &CurrentProcess,
        };
        match dispatch(&context, number, args) {
            Outcome::Return(value) => value,
            Outcome::Exit(code) => {
                drop(files);
                crate::scheduler::exit(crate::task::ExitStatus(code))
            }
        }
    }
}

#[cfg(not(host_model))]
pub use kernel::dispatch_current;
