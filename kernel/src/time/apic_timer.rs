//! Local APIC Timer implementation for high-precision periodic/oneshot ticks.

use crate::arch::x86_64::interrupt_controller;
use crate::arch::x86_64::pic;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Stored ticks per millisecond for the Local APIC Timer.
static APIC_TICKS_PER_MS: AtomicU32 = AtomicU32::new(0);

/// Whether the Local APIC Timer is currently active and driving system ticks.
static APIC_TIMER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Divider 16 representation for APIC Timer Divisor Register (0b0011).
const APIC_TIMER_DIV_16: u32 = 0x03;

/// Calibrates and starts the Local APIC Timer.
///
/// Calibration is performed by running the APIC timer countdown with divisor 16
/// while waiting for a known delay of ~10ms using TSC (if available) or PIT.
pub fn init() -> Result<(), &'static str> {
    let ctrl = interrupt_controller::CONTROLLER.lock();
    if ctrl.mode != interrupt_controller::ControllerKind::Apic {
        return Err("APIC is not the active interrupt controller");
    }

    // 1. Tell LAPIC to use Divisor 16
    ctrl.lapic.set_timer_divisor(APIC_TIMER_DIV_16);

    // 2. Set timer in one-shot mode masked (LVT Timer: masked, vector 0xFF)
    ctrl.lapic.set_lvt_timer(0xFF, false, true);

    // 3. Set initial count to maximum (0xFFFF_FFFF) to measure decrement
    ctrl.lapic.set_timer_initial_count(0xFFFF_FFFF);

    // Release lock before sleeping to avoid deadlocks
    drop(ctrl);

    // 4. Wait 10 milliseconds using TSC delay or busy spin
    #[cfg(feature = "tsc")]
    crate::time::tsc::delay_us(10_000);
    #[cfg(not(feature = "tsc"))]
    {
        // Simple spin-loop fallback for ~10ms
        for _ in 0..5_000_000 {
            core::hint::spin_loop();
        }
    }

    // 5. Read elapsed counts
    let mut ctrl = interrupt_controller::CONTROLLER.lock();
    let remaining = ctrl.lapic.read_timer_current_count();
    let elapsed = 0xFFFF_FFFFu32.saturating_sub(remaining);

    // Stop timer
    ctrl.lapic.mask_timer();

    if elapsed == 0 {
        return Err("APIC timer counter did not decrement");
    }

    // Elapsed in 10 ms -> ticks per ms = elapsed / 10
    let ticks_per_ms = elapsed / 10;
    APIC_TICKS_PER_MS.store(ticks_per_ms, Ordering::Relaxed);

    // 6. Program Local APIC Timer for Periodic mode at 1000 Hz (1ms per tick)
    // Vector 0x20 matches PIC_1_OFFSET (timer_interrupt_handler in IDT)
    ctrl.lapic.set_timer_divisor(APIC_TIMER_DIV_16);
    ctrl.lapic.set_lvt_timer(pic::PIC_1_OFFSET, true, false);
    ctrl.lapic.set_timer_initial_count(ticks_per_ms);

    APIC_TIMER_ACTIVE.store(true, Ordering::Relaxed);

    // Mask the IOAPIC IRQ0 / PIT redirection entry so the PIT no longer
    // delivers Vector 0x20 interrupts alongside the LAPIC timer.
    // Without this both sources fire into on_timer_interrupt() → TICKS
    // accumulates at 2× the intended rate, making sleep run ~2× too fast.
    ctrl.mask_ioapic_timer();

    Ok(())
}

/// Returns whether the APIC Timer is currently driving system ticks.
#[inline]
pub fn is_active() -> bool {
    APIC_TIMER_ACTIVE.load(Ordering::Relaxed)
}

/// Returns the calibrated APIC timer ticks per millisecond.
#[inline]
pub fn ticks_per_ms() -> u32 {
    APIC_TICKS_PER_MS.load(Ordering::Relaxed)
}

/// Stops and masks the Local APIC Timer.
pub fn stop() {
    let ctrl = interrupt_controller::CONTROLLER.lock();
    ctrl.lapic.mask_timer();
    APIC_TIMER_ACTIVE.store(false, Ordering::Relaxed);
}
