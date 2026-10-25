//! Raw system calls (Linux x86_64 convention): rax = number, args in rdi,
//! rsi, rdx, r10, r8, r9; the result in rax, negative errno on failure.
//! The typed layer (`fs`, `io`, `process`, ...) is built on these.

use core::arch::asm;

pub mod nr {
    pub const READ: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const OPEN: u64 = 2;
    pub const CLOSE: u64 = 3;
    pub const STAT: u64 = 4;
    pub const FSTAT: u64 = 5;
    pub const LSEEK: u64 = 8;
    pub const MMAP: u64 = 9;
    pub const MPROTECT: u64 = 10;
    pub const MUNMAP: u64 = 11;
    pub const BRK: u64 = 12;
    pub const IOCTL: u64 = 16;
    pub const DUP: u64 = 32;
    pub const DUP2: u64 = 33;
    pub const GETPID: u64 = 39;
    pub const FORK: u64 = 57;
    pub const EXECVE: u64 = 59;
    pub const WAIT4: u64 = 61;
    pub const GETCWD: u64 = 79;
    pub const CHDIR: u64 = 80;
    pub const RENAME: u64 = 82;
    pub const MKDIR: u64 = 83;
    pub const RMDIR: u64 = 84;
    pub const UNLINK: u64 = 87;
    pub const GETPPID: u64 = 110;
    pub const GETDENTS64: u64 = 217;
    pub const ARCH_PRCTL: u64 = 158;
    pub const EXIT: u64 = 60;
    pub const EXIT_GROUP: u64 = 231;
}

/// Raw syscall: rax = number, args in rdi rsi rdx r10 r8 r9; the CPU
/// clobbers rcx and r11. Returns rax (negative errno on failure).
///
/// # Safety
/// Pointer arguments must be valid for what the syscall does with them.
pub unsafe fn syscall6(number: u64, args: [u64; 6]) -> i64 {
    let result: i64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") number as i64 => result,
            in("rdi") args[0],
            in("rsi") args[1],
            in("rdx") args[2],
            in("r10") args[3],
            in("r8") args[4],
            in("r9") args[5],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    result
}

/// # Safety
/// As [`syscall6`].
pub unsafe fn syscall3(number: u64, a0: u64, a1: u64, a2: u64) -> i64 {
    unsafe { syscall6(number, [a0, a1, a2, 0, 0, 0]) }
}

pub fn write(fd: u32, bytes: &[u8]) -> i64 {
    unsafe {
        syscall3(
            nr::WRITE,
            u64::from(fd),
            bytes.as_ptr() as u64,
            bytes.len() as u64,
        )
    }
}

pub fn read(fd: u32, buffer: &mut [u8]) -> i64 {
    unsafe {
        syscall3(
            nr::READ,
            u64::from(fd),
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
        )
    }
}

/// `path` must end with a NUL byte.
pub fn open(path: &[u8], flags: u64) -> i64 {
    if path.last() != Some(&0) {
        return -22; // EINVAL
    }
    unsafe { syscall3(nr::OPEN, path.as_ptr() as u64, flags, 0) }
}

pub fn close(fd: u32) -> i64 {
    unsafe { syscall3(nr::CLOSE, u64::from(fd), 0, 0) }
}

pub fn getpid() -> i64 {
    unsafe { syscall3(nr::GETPID, 0, 0, 0) }
}

/// The program break after asking for `address` (0: just report it).
pub fn brk(address: u64) -> u64 {
    unsafe { syscall3(nr::BRK, address, 0, 0) as u64 }
}

pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;
pub const MAP_PRIVATE: u64 = 0x02;
pub const MAP_FIXED: u64 = 0x10;
pub const MAP_ANONYMOUS: u64 = 0x20;

/// Anonymous private memory: the address, or a negative errno.
pub fn mmap_anonymous(hint: u64, length: u64, protection: u64, extra_flags: u64) -> i64 {
    unsafe {
        syscall6(
            nr::MMAP,
            [
                hint,
                length,
                protection,
                MAP_PRIVATE | MAP_ANONYMOUS | extra_flags,
                u64::MAX,
                0,
            ],
        )
    }
}

pub fn munmap(address: u64, length: u64) -> i64 {
    unsafe { syscall3(nr::MUNMAP, address, length, 0) }
}

pub fn mprotect(address: u64, length: u64, protection: u64) -> i64 {
    unsafe { syscall3(nr::MPROTECT, address, length, protection) }
}

pub const ARCH_SET_FS: u64 = 0x1002;
pub const ARCH_GET_FS: u64 = 0x1003;

pub fn arch_prctl(code: u64, address: u64) -> i64 {
    unsafe { syscall3(nr::ARCH_PRCTL, code, address, 0) }
}

pub const O_RDONLY: u64 = 0;
pub const O_WRONLY: u64 = 1;
pub const O_RDWR: u64 = 2;
pub const O_CREAT: u64 = 0o100;
pub const O_TRUNC: u64 = 0o1000;

/// The child's pid in the parent, 0 in the child, or a negative errno.
pub fn fork() -> i64 {
    unsafe { syscall3(nr::FORK, 0, 0, 0) }
}

/// `path`, `argv` and `envp` entries must end with a NUL byte. Only
/// returns on failure (a negative errno).
pub fn execve(path: &[u8], argv: &[&[u8]], envp: &[&[u8]]) -> i64 {
    const MAX: usize = 32;
    if argv.len() >= MAX || envp.len() >= MAX {
        return -7; // E2BIG
    }
    let mut argv_pointers = [0u64; MAX];
    let mut envp_pointers = [0u64; MAX];
    for (slot, argument) in argv_pointers.iter_mut().zip(argv) {
        *slot = argument.as_ptr() as u64;
    }
    for (slot, variable) in envp_pointers.iter_mut().zip(envp) {
        *slot = variable.as_ptr() as u64;
    }
    unsafe {
        syscall3(
            nr::EXECVE,
            path.as_ptr() as u64,
            argv_pointers.as_ptr() as u64,
            envp_pointers.as_ptr() as u64,
        )
    }
}

pub const WNOHANG: u64 = 1;

/// `wait4(pid, &status, options, NULL)`: the collected child's pid (0 with
/// `WNOHANG` if none has exited), or a negative errno.
pub fn wait4(pid: i64, status: &mut i32, options: u64) -> i64 {
    unsafe {
        syscall6(
            nr::WAIT4,
            [pid as u64, status as *mut i32 as u64, options, 0, 0, 0],
        )
    }
}

/// Decoded `wait` status.
pub fn exited_with(status: i32) -> Option<i32> {
    (status & 0x7f == 0).then_some((status >> 8) & 0xff)
}

pub fn killed_by(status: i32) -> Option<i32> {
    (status & 0x7f != 0).then_some(status & 0x7f)
}

pub fn getppid() -> i64 {
    unsafe { syscall3(nr::GETPPID, 0, 0, 0) }
}

pub fn dup2(old: u32, new: u32) -> i64 {
    unsafe { syscall3(nr::DUP2, u64::from(old), u64::from(new), 0) }
}

/// `path` must end with a NUL byte.
pub fn chdir(path: &[u8]) -> i64 {
    unsafe { syscall3(nr::CHDIR, path.as_ptr() as u64, 0, 0) }
}

/// The length written (with the NUL), or a negative errno.
pub fn getcwd(buffer: &mut [u8]) -> i64 {
    unsafe {
        syscall3(
            nr::GETCWD,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
        )
    }
}

pub const TIOCGWINSZ: u64 = 0x5413;

/// `ioctl(fd, request, argument)`: 0 or a negative errno.
pub fn ioctl(fd: u32, request: u64, argument: u64) -> i64 {
    unsafe { syscall3(nr::IOCTL, u64::from(fd), request, argument) }
}

pub fn dup(fd: u32) -> i64 {
    unsafe { syscall3(nr::DUP, u64::from(fd), 0, 0) }
}

/// `lseek(fd, offset, whence)`: 0 = SET, 1 = CUR, 2 = END.
pub fn lseek(fd: u32, offset: i64, whence: u32) -> i64 {
    unsafe { syscall3(nr::LSEEK, u64::from(fd), offset as u64, u64::from(whence)) }
}

/// `stat` into a 144-byte `struct stat`; `path` ends with a NUL byte.
pub fn stat(path: &[u8], buffer: &mut [u8; 144]) -> i64 {
    unsafe {
        syscall3(
            nr::STAT,
            path.as_ptr() as u64,
            buffer.as_mut_ptr() as u64,
            0,
        )
    }
}

pub fn getdents64(fd: u32, buffer: &mut [u8]) -> i64 {
    unsafe {
        syscall3(
            nr::GETDENTS64,
            u64::from(fd),
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
        )
    }
}

/// `path` ends with a NUL byte.
pub fn mkdir(path: &[u8], mode: u32) -> i64 {
    unsafe { syscall3(nr::MKDIR, path.as_ptr() as u64, u64::from(mode), 0) }
}

pub fn rmdir(path: &[u8]) -> i64 {
    unsafe { syscall3(nr::RMDIR, path.as_ptr() as u64, 0, 0) }
}

pub fn unlink(path: &[u8]) -> i64 {
    unsafe { syscall3(nr::UNLINK, path.as_ptr() as u64, 0, 0) }
}

pub fn rename(old: &[u8], new: &[u8]) -> i64 {
    unsafe { syscall3(nr::RENAME, old.as_ptr() as u64, new.as_ptr() as u64, 0) }
}

/// End the process at once, without flushing buffered output (`_exit`).
pub fn exit_now(code: i32) -> ! {
    unsafe { syscall3(nr::EXIT_GROUP, code as u64, 0, 0) };
    // exit never returns; if it somehow did, never run on.
    unsafe { asm!("ud2", options(nomem, nostack, noreturn)) }
}
