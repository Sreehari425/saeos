//! Clocks and sleeping: `clock_gettime`, `nanosleep`, `clock_nanosleep`.
//!
//! | clock                  | source                                        |
//! |------------------------|-----------------------------------------------|
//! | `CLOCK_MONOTONIC` (1)  | nanoseconds since boot (TSC, else timer ticks)|
//! | `CLOCK_BOOTTIME` (7)   | the same: SaeOS never suspends                |
//! | `CLOCK_REALTIME` (0)   | the RTC at boot plus monotonic time; `EINVAL` without an RTC |
//!
//! A sleep lasts *at least* as long as asked (the kernel rounds up to the
//! next timer tick) and, with no signals yet, is never cut short, so the
//! `rem` argument is never written. A deadline already past returns at once.
//! Pure: time comes from [`ProcessOps`], so the host model tests it.

use super::{Context, ProcessOps, UserMemory};
use crate::fs::Errno;

pub const CLOCK_REALTIME: u64 = 0;
pub const CLOCK_MONOTONIC: u64 = 1;
pub const CLOCK_BOOTTIME: u64 = 7;
/// `clock_nanosleep` flag: `request` is an absolute time on the clock.
pub const TIMER_ABSTIME: u64 = 1;

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// `struct timespec { i64 tv_sec; i64 tv_nsec; }`.
pub fn encode_timespec(nanoseconds: u64) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&(nanoseconds / NANOS_PER_SECOND).to_le_bytes());
    bytes[8..].copy_from_slice(&(nanoseconds % NANOS_PER_SECOND).to_le_bytes());
    bytes
}

/// A `struct timespec` as nanoseconds; `EINVAL` for a negative time or
/// `tv_nsec` outside `0..1e9`, as Linux.
pub fn decode_timespec(bytes: &[u8; 16]) -> Result<u64, Errno> {
    let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
    let nanoseconds = i64::from_le_bytes(bytes[8..].try_into().unwrap());
    if seconds < 0 || !(0..NANOS_PER_SECOND as i64).contains(&nanoseconds) {
        return Err(Errno::EINVAL);
    }
    Ok((seconds as u64)
        .saturating_mul(NANOS_PER_SECOND)
        .saturating_add(nanoseconds as u64))
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Seconds since the Unix epoch for a UTC date and time.
pub fn unix_seconds(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    days_from_civil(year, month, day) * 86_400
        + i64::from(hour) * 3600
        + i64::from(minute) * 60
        + i64::from(second)
}

/// The clock's current time.
fn now(process: &dyn ProcessOps, clock: u64) -> Result<u64, Errno> {
    match clock {
        CLOCK_MONOTONIC | CLOCK_BOOTTIME => Ok(process.monotonic_ns()),
        CLOCK_REALTIME => process.realtime_ns().ok_or(Errno::EINVAL),
        _ => Err(Errno::EINVAL),
    }
}

fn read_timespec<M: UserMemory>(context: &Context<M>, address: u64) -> Result<u64, Errno> {
    let mut bytes = [0u8; 16];
    context.memory.read(address, &mut bytes)?;
    decode_timespec(&bytes)
}

pub fn clock_gettime<M: UserMemory>(
    context: &Context<M>,
    clock: u64,
    address: u64,
) -> Result<u64, Errno> {
    let time = now(context.process, clock)?;
    context.memory.write(address, &encode_timespec(time))?;
    Ok(0)
}

pub fn nanosleep<M: UserMemory>(context: &Context<M>, request: u64) -> Result<u64, Errno> {
    let duration = read_timespec(context, request)?;
    let deadline = context.process.monotonic_ns().saturating_add(duration);
    context.process.sleep_until_ns(deadline);
    Ok(0)
}

pub fn clock_nanosleep<M: UserMemory>(
    context: &Context<M>,
    clock: u64,
    flags: u64,
    request: u64,
) -> Result<u64, Errno> {
    if flags & !TIMER_ABSTIME != 0 {
        return Err(Errno::EINVAL);
    }
    let value = read_timespec(context, request)?;
    let clock_now = now(context.process, clock)?;
    let monotonic_now = context.process.monotonic_ns();
    let remaining = if flags & TIMER_ABSTIME != 0 {
        value.saturating_sub(clock_now)
    } else {
        value
    };
    context
        .process
        .sleep_until_ns(monotonic_now.saturating_add(remaining));
    Ok(0)
}
