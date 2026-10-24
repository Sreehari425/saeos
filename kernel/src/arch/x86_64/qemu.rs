//! QEMU's `isa-debug-exit` device, used to end automated test runs.
//!
//! Started with `-device isa-debug-exit,iobase=0xf4,iosize=0x04`, QEMU exits
//! with status `(value << 1) | 1` when the guest writes `value` to the port.
//! The port is only touched in builds with the `qemu-exit` feature, so real
//! hardware never sees the write.

pub const DEBUG_EXIT_PORT: u16 = 0xf4;
/// Host exit status 33.
pub const PASS: u32 = 0x10;
/// Host exit status 35.
pub const FAIL: u32 = 0x11;

/// End the QEMU run with a verdict. Halts if the device is absent.
#[cfg(feature = "qemu-exit")]
pub fn exit(passed: bool) -> ! {
    crate::serial_println!("qemu-exit: {}", if passed { "PASS" } else { "FAIL" });
    unsafe { super::cpu::outl(DEBUG_EXIT_PORT, if passed { PASS } else { FAIL }) };
    loop {
        super::cpu::cli();
        super::cpu::hlt();
    }
}

/// End the run if this is a `qemu-exit` build; otherwise do nothing so the
/// caller's normal halt or report path continues.
pub fn exit_if_enabled(passed: bool) {
    #[cfg(feature = "qemu-exit")]
    exit(passed);
    #[cfg(not(feature = "qemu-exit"))]
    let _ = passed;
}
