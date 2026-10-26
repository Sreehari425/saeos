//! libsaeos: the SaeOS C library, written in Rust.
//!
//! ```text
//! programs ──▶ prelude, io (print!, File, stdin), fs, env, process
//!              c: the C interface (malloc, open, strlen, errno, ...)
//!              heap + alloc: the allocator (brk heap, mmap for large)
//!              start + tls: _start, argv/envp, thread-local storage
//!              time: clocks and sleeping; sound: /dev/dsp; fb: /dev/fb0
//!              raw: the syscalls themselves (Linux x86_64 numbers)
//! ```
//!
//! Programs build for `userspace/x86_64-saeos.json` (`make userspace`): a
//! static, non-PIE target with SSE (hardware floating point; the kernel
//! saves each process's FPU state), linked with `userspace/user.ld`.
//! `heap`, `lines` and `startinfo` are pure and tested in the host model.

#![no_std]
#![feature(thread_local)]

extern crate alloc;

mod allocator;
pub mod c;
pub mod env;
pub mod errno;
pub mod fb;
pub mod fs;
pub mod heap;
pub mod io;
pub mod lines;
pub mod process;
pub mod raw;
pub mod sound;
mod start;
pub mod startinfo;
pub mod time;
mod tls;

// The raw layer stays at the root for programs written against it.
pub use raw::*;

pub use errno::{Errno, Result};
pub use process::exit;
pub use start::Args;

/// What most programs want in scope.
pub mod prelude {
    pub use crate::errno::{Errno, Result};
    pub use crate::{eprint, eprintln, print, println};
    pub use alloc::boxed::Box;
    pub use alloc::format;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec;
    pub use alloc::vec::Vec;
}
