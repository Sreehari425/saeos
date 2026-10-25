//! The first SaeOS user program. Built and loaded by `exec::prepare` in
//! the boot test; it runs once the kernel can enter ring 3.

#![no_std]
#![no_main]

use saeos_rt::{Args, entry, write};

entry!(main);

/// Large enough to span several pages of `.bss`, which the loader must
/// zero (the boot test checks it does).
static mut SCRATCH: [u8; 3 * 4096] = [0; 3 * 4096];

/// Lands in `.data`.
static mut GREETING: [u8; 21] = *b"hello from userspace\n";

fn main(args: Args) -> i32 {
    let scratch = &raw mut SCRATCH;
    // Touch .bss so it is not optimised away.
    unsafe { core::ptr::write_volatile(scratch.cast::<u8>(), args.count() as u8) };
    let greeting = unsafe { (&raw const GREETING).read() };
    if write(1, &greeting) < 0 {
        return 1;
    }
    0
}
