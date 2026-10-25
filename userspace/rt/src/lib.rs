//! The SaeOS user runtime: program entry, raw syscalls (Linux x86_64
//! convention), and a panic handler. No allocation, no libc.
//!
//! ```text
//! kernel exec -> _start (rsp = argc, argv..., NULL, envp..., NULL, auxv...)
//!             -> start(stack) -> saeos_main(Args) -> exit(code)
//! ```

#![no_std]

use core::arch::{asm, naked_asm};

pub mod nr {
    pub const READ: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const OPEN: u64 = 2;
    pub const CLOSE: u64 = 3;
    pub const GETPID: u64 = 39;
    pub const EXIT: u64 = 60;
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

pub fn exit(code: i32) -> ! {
    unsafe { syscall3(nr::EXIT, code as u64, 0, 0) };
    // exit never returns; if it somehow did, never run on.
    unsafe { asm!("ud2", options(nomem, nostack, noreturn)) }
}

/// The program's arguments, from the initial stack.
#[derive(Clone, Copy)]
pub struct Args {
    count: usize,
    vector: *const *const u8,
}

impl Args {
    pub fn count(&self) -> usize {
        self.count
    }

    /// Argument `index` without its NUL.
    pub fn get(&self, index: usize) -> Option<&'static [u8]> {
        if index >= self.count {
            return None;
        }
        unsafe {
            let start = *self.vector.add(index);
            let mut length = 0;
            while *start.add(length) != 0 {
                length += 1;
            }
            Some(core::slice::from_raw_parts(start, length))
        }
    }
}

unsafe extern "Rust" {
    /// Defined by [`entry!`].
    fn saeos_main(args: Args) -> i32;
}

/// Declare the program's `main(Args) -> i32`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        fn saeos_main(args: $crate::Args) -> i32 {
            $main(args)
        }
    };
}

/// The ELF entry point. The kernel starts us with rsp at argc and nothing
/// else set up; align the stack as the SysV ABI requires for a call.
///
/// # Safety
/// Only the kernel may enter here, with the initial stack `exec` builds.
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _start() -> ! {
    naked_asm!(
        "mov rdi, rsp",
        "and rsp, -16",
        "xor ebp, ebp",
        "call {start}",
        "ud2",
        start = sym start,
    )
}

unsafe extern "C" fn start(stack: *const u64) -> ! {
    let args = unsafe {
        Args {
            count: *stack as usize,
            vector: stack.add(1) as *const *const u8,
        }
    };
    exit(unsafe { saeos_main(args) })
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    write(2, b"panic\n");
    exit(101)
}
