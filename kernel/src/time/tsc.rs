//! Time Stamp Counter (TSC) calibration and high-resolution timing.

use crate::arch::x86_64::cpu;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Calibrated TSC frequency in Hertz (cycles per second).
static TSC_HZ: AtomicU64 = AtomicU64::new(0);

/// Flag indicating if the CPU provides an Invariant TSC.
static INVARIANT_TSC: AtomicBool = AtomicBool::new(false);

/// Calibrate the TSC frequency using PIT Channel 2 or Channel 0.
///
/// We run a calibration interval of ~10 milliseconds against the PIT.
/// PIT frequency is 1,193,182 Hz. Over 10ms (11,932 PIT counts), we measure
/// the delta in `rdtsc()`.
pub fn init() {
    let invariant = cpu::has_invariant_tsc();
    INVARIANT_TSC.store(invariant, Ordering::Relaxed);

    if !cpu::has_rdtsc() {
        return;
    }

    // Measure TSC ticks over ~10ms using PIT Channel 2 one-shot countdown.
    // Port 0x61: System Control Port B (gate 2 is bit 0, speaker out is bit 1)
    // Port 0x42: PIT Channel 2 data
    // Port 0x43: PIT Command register
    unsafe {
        // Prepare PIT Channel 2: Mode 0 (interrupt on terminal count), binary, low/high byte.
        // Command byte: 10 (Channel 2) 11 (Access mode: low then high) 000 (Mode 0) 0 (Binary) -> 0b10110000 = 0xB0.
        // 10ms = 11,932 ticks (1,193,182 Hz / 100)
        const CALIBRATE_TICKS: u16 = 11_932;

        // Save original port 0x61 state
        let port61_orig = cpu::inb(0x61);

        // Turn off speaker (bit 1) and disable gate 2 (bit 0)
        cpu::outb(0x61, port61_orig & 0xFC);

        // Configure Channel 2
        cpu::outb(0x43, 0xB0);
        cpu::outb(0x42, (CALIBRATE_TICKS & 0xFF) as u8);
        cpu::outb(0x42, (CALIBRATE_TICKS >> 8) as u8);

        // Enable gate 2 (bit 0) to start countdown
        let port61_start = cpu::inb(0x61);
        cpu::outb(0x61, (port61_start & 0xFD) | 0x01);

        let tsc_start = cpu::rdtsc();

        // Wait until OUT2 (bit 5 of port 0x61) becomes high (terminal count reached)
        // With a safety timeout loop in case running in an emulation without Channel 2
        let mut loop_count = 0u32;
        while (cpu::inb(0x61) & 0x20) == 0 {
            loop_count += 1;
            if loop_count > 10_000_000 {
                // Channel 2 not functional; fallback default 2.0 GHz estimate
                break;
            }
            core::hint::spin_loop();
        }

        let tsc_end = cpu::rdtsc();

        // Reset port 0x61
        cpu::outb(0x61, port61_orig & 0xFC);

        if loop_count <= 10_000_000 && tsc_end > tsc_start {
            let delta = tsc_end - tsc_start;
            // 11,932 PIT ticks = 11,932 / 1,193,182 seconds ~= 0.009999983 seconds (10ms)
            // Frequency (Hz) = delta * 1,193,182 / 11,932
            let hz = delta.saturating_mul(1_193_182) / (CALIBRATE_TICKS as u64);
            TSC_HZ.store(hz, Ordering::Relaxed);
        } else {
            // Fallback: assume 2.0 GHz if PIT Channel 2 is missing/stuck
            TSC_HZ.store(2_000_000_000, Ordering::Relaxed);
        }
    }
}

/// Returns the calibrated TSC frequency in Hz (cycles per second), or 0 if uncalibrated.
#[inline]
pub fn frequency_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

/// Returns whether the CPU supports Invariant TSC.
#[inline]
pub fn is_invariant() -> bool {
    INVARIANT_TSC.load(Ordering::Relaxed)
}

/// Returns the current raw cycle count from the CPU Time Stamp Counter.
#[inline]
pub fn read() -> u64 {
    cpu::rdtsc()
}

/// Converts TSC cycle delta to nanoseconds.
#[inline]
pub fn cycles_to_nanos(cycles: u64) -> u64 {
    let hz = frequency_hz();
    if hz == 0 {
        return 0;
    }
    ((cycles as u128 * 1_000_000_000) / (hz as u128)) as u64
}

/// Converts TSC cycle delta to microseconds.
#[inline]
pub fn cycles_to_micros(cycles: u64) -> u64 {
    let hz = frequency_hz();
    if hz == 0 {
        return 0;
    }
    ((cycles as u128 * 1_000_000) / (hz as u128)) as u64
}

/// Busy-wait delay for the specified number of microseconds using the calibrated TSC.
pub fn delay_us(micros: u64) {
    let hz = frequency_hz();
    if hz == 0 {
        for _ in 0..(micros * 1000) {
            core::hint::spin_loop();
        }
        return;
    }

    let cycles = ((micros as u128 * hz as u128) / 1_000_000) as u64;
    let start = read();
    while read().saturating_sub(start) < cycles {
        core::hint::spin_loop();
    }
}
