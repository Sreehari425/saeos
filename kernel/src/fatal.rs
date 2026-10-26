//! Last-resort reporting for panics and fatal CPU exceptions.
//!
//! A fault can arrive while the faulting code holds the console or serial
//! lock (for example, halfway through a `println!`). Waiting for that lock
//! would hang the report forever. Once [`begin`] marks the system as
//! panicking, the print paths take their locks with `force_lock` instead:
//! with interrupts off on a single CPU, the code that held them never runs
//! again, so stealing them is safe, and at worst the screen shows a
//! half-updated line.

use crate::arch::x86_64::{cpu, qemu};
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

static PANICKING: AtomicBool = AtomicBool::new(false);

/// Whether the system is in the fatal path; print paths then ignore locks.
pub fn panicking() -> bool {
    PANICKING.load(Ordering::Acquire)
}

/// Enter the fatal path with interrupts off. Returns false if the system was
/// already panicking (a fault inside the fatal path itself).
pub fn begin() -> bool {
    cpu::cli();
    !PANICKING.swap(true, Ordering::AcqRel)
}

/// Report a fatal condition on every console and stop. `passed` is the QEMU
/// test verdict; it is true only where a test expects this exact fault.
pub fn report(message: fmt::Arguments, passed: bool) -> ! {
    if !begin() {
        // The report itself faulted: avoid all locks and formatting.
        raw_serial("\n[FATAL: fault while reporting a fault]\n");
        halt(false);
    }
    // A program may own the screen (/dev/fb0): take it back first.
    crate::drivers::fb::reclaim_for_panic();
    crate::drivers::console::reclaim_screen_for_panic();
    crate::drivers::console::set_color(
        crate::drivers::vga::Color::LightRed,
        crate::drivers::vga::Color::Black,
    );
    crate::println!("\n{}", message);
    print_backtrace();
    halt(passed)
}

/// Print the call chain leading here. Addresses are kernel virtual
/// addresses; `scripts/symbolize.sh` turns them into function names.
fn print_backtrace() {
    crate::println!(
        "Backtrace (image={}; scripts/symbolize.sh):",
        crate::backtrace::IMAGE
    );
    crate::backtrace::walk_current(|depth, address| {
        crate::println!("  #{} {:#x}", depth, address);
    });
}

/// End the run (QEMU test builds) or halt forever.
pub fn halt(passed: bool) -> ! {
    qemu::exit_if_enabled(passed);
    loop {
        cpu::cli();
        cpu::hlt();
    }
}

/// Write straight to COM1 without the serial driver or its lock.
pub fn raw_serial(message: &str) {
    const COM1: u16 = 0x3f8;
    const LINE_STATUS: u16 = COM1 + 5;
    const TRANSMIT_EMPTY: u8 = 1 << 5;
    for byte in message.bytes() {
        let mut spins = 0u32;
        while unsafe { cpu::inb(LINE_STATUS) } & TRANSMIT_EMPTY == 0 && spins < 100_000 {
            spins += 1;
            core::hint::spin_loop();
        }
        unsafe { cpu::outb(COM1, byte) };
    }
}
