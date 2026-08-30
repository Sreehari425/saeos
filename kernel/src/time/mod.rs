//! Kernel time services.
//!
//! PIT ticks provide monotonic deadlines. CMOS RTC reads provide wall-clock
//! information only; they are never used for elapsed-time calculations.

#[cfg(feature = "pit")]
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "pit")]
use crate::arch::x86_64::cpu;

#[cfg(feature = "pit")]
const PIT_CHANNEL_0: u16 = 0x40;
#[cfg(feature = "pit")]
const PIT_COMMAND: u16 = 0x43;
#[cfg(feature = "pit")]
const PIT_BASE_HZ: u32 = 1_193_182;
#[cfg(feature = "pit")]
pub const TICK_HZ: u32 = 1_000;
#[cfg(feature = "pit")]
const PIT_DIVISOR: u16 = (PIT_BASE_HZ / TICK_HZ) as u16;

#[cfg(feature = "pit")]
static TICKS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepError {
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtcTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

pub fn init() {
    #[cfg(feature = "pit")]
    unsafe {
        // Channel 0, low byte then high byte, mode 2 rate generator, binary
        // mode. Mode 2 gives the IOAPIC one terminal-count pulse per period.
        cpu::outb(PIT_COMMAND, 0x34);
        cpu::outb(PIT_CHANNEL_0, PIT_DIVISOR as u8);
        cpu::outb(PIT_CHANNEL_0, (PIT_DIVISOR >> 8) as u8);
    }
}

pub fn on_timer_interrupt() {
    #[cfg(feature = "pit")]
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn uptime_ms() -> Option<u64> {
    #[cfg(feature = "pit")]
    {
        Some(TICKS.load(Ordering::Relaxed).saturating_mul(1_000) / TICK_HZ as u64)
    }
    #[cfg(not(feature = "pit"))]
    None
}

pub fn sleep_ms(milliseconds: u64) -> Result<(), SleepError> {
    #[cfg(feature = "pit")]
    {
        let Some(start) = uptime_ms() else {
            return Err(SleepError::Disabled);
        };
        let deadline = start.saturating_add(milliseconds);
        while uptime_ms().is_some_and(|now| now < deadline) {
            if cpu::interrupts_enabled() {
                cpu::hlt();
            } else {
                cpu::sti();
                cpu::hlt();
            }
        }
        Ok(())
    }
    #[cfg(not(feature = "pit"))]
    {
        let _ = milliseconds;
        Err(SleepError::Disabled)
    }
}

#[cfg(feature = "rtc")]
mod rtc {
    use super::RtcTime;
    use crate::arch::x86_64::cpu;

    const CMOS_INDEX: u16 = 0x70;
    const CMOS_DATA: u16 = 0x71;
    const STATUS_A: u8 = 0x0A;
    const STATUS_B: u8 = 0x0B;
    const STATUS_A_UIP: u8 = 1 << 7;
    const STATUS_B_24_HOUR: u8 = 1 << 1;
    const STATUS_B_BINARY: u8 = 1 << 2;

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct RawRtc {
        second: u8,
        minute: u8,
        hour: u8,
        day: u8,
        month: u8,
        year: u8,
        status_b: u8,
    }

    unsafe fn read_register(register: u8) -> u8 {
        unsafe {
            // Keep NMI disabled while selecting and reading a CMOS register.
            cpu::outb(CMOS_INDEX, register | 0x80);
            cpu::inb(CMOS_DATA)
        }
    }

    fn wait_for_update() -> bool {
        for _ in 0..100_000 {
            if unsafe { read_register(STATUS_A) } & STATUS_A_UIP == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    fn read_raw() -> Option<RawRtc> {
        if !wait_for_update() {
            unsafe { cpu::outb(CMOS_INDEX, 0) };
            return None;
        }
        let raw = RawRtc {
            second: unsafe { read_register(0x00) },
            minute: unsafe { read_register(0x02) },
            hour: unsafe { read_register(0x04) },
            day: unsafe { read_register(0x07) },
            month: unsafe { read_register(0x08) },
            year: unsafe { read_register(0x09) },
            status_b: unsafe { read_register(STATUS_B) },
        };
        unsafe { cpu::outb(CMOS_INDEX, 0) };
        Some(raw)
    }

    fn bcd(value: u8) -> u8 {
        (value & 0x0F) + ((value >> 4) * 10)
    }

    fn convert(raw: RawRtc) -> RtcTime {
        let binary = raw.status_b & STATUS_B_BINARY != 0;
        let decode = |value| if binary { value } else { bcd(value) };
        let mut hour = decode(raw.hour & 0x7F);
        if raw.status_b & STATUS_B_24_HOUR == 0 && raw.hour & 0x80 != 0 {
            hour = if hour == 12 { 0 } else { hour + 12 };
        }
        RtcTime {
            year: 2000 + decode(raw.year) as u16,
            month: decode(raw.month),
            day: decode(raw.day),
            hour,
            minute: decode(raw.minute),
            second: decode(raw.second),
        }
    }

    pub fn read() -> Option<RtcTime> {
        for _ in 0..4 {
            let first = read_raw()?;
            let second = read_raw()?;
            if first == second {
                return Some(convert(first));
            }
        }
        None
    }
}

#[cfg(feature = "rtc")]
pub fn read_rtc() -> Option<RtcTime> {
    rtc::read()
}

#[cfg(not(feature = "rtc"))]
pub fn read_rtc() -> Option<RtcTime> {
    None
}
