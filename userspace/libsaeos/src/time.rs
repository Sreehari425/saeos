//! Clocks and sleeping, in nanoseconds: [`now`] is monotonic time since
//! boot, the clock to measure and pace with; [`realtime`] is the wall
//! clock, when the kernel knows it. Sleeps last at least as long as asked.

use crate::errno::{Result, check};
use crate::raw::{nr, syscall3};

pub const NANOS_PER_SECOND: u64 = 1_000_000_000;
pub const NANOS_PER_MILLISECOND: u64 = 1_000_000;

const CLOCK_REALTIME: u64 = 0;
const CLOCK_MONOTONIC: u64 = 1;
const TIMER_ABSTIME: u64 = 1;

/// `struct timespec`.
#[repr(C)]
#[derive(Default)]
struct Timespec {
    seconds: i64,
    nanoseconds: i64,
}

impl Timespec {
    fn from_nanos(nanoseconds: u64) -> Self {
        Self {
            seconds: (nanoseconds / NANOS_PER_SECOND) as i64,
            nanoseconds: (nanoseconds % NANOS_PER_SECOND) as i64,
        }
    }

    fn nanos(&self) -> u64 {
        self.seconds as u64 * NANOS_PER_SECOND + self.nanoseconds as u64
    }
}

fn clock(id: u64) -> Result<u64> {
    let mut time = Timespec::default();
    check(unsafe { syscall3(nr::CLOCK_GETTIME, id, &raw mut time as u64, 0) })?;
    Ok(time.nanos())
}

/// Nanoseconds since boot. Never goes backwards.
pub fn now() -> u64 {
    clock(CLOCK_MONOTONIC).unwrap_or(0)
}

/// Nanoseconds since the Unix epoch (`EINVAL` if the kernel has no wall
/// clock).
pub fn realtime() -> Result<u64> {
    clock(CLOCK_REALTIME)
}

/// Sleep for at least `nanoseconds`.
pub fn sleep(nanoseconds: u64) {
    let request = Timespec::from_nanos(nanoseconds);
    unsafe { syscall3(nr::NANOSLEEP, &raw const request as u64, 0, 0) };
}

/// Sleep until [`now`] reaches `deadline`: for a steady pace (frames, audio)
/// that does not drift by the time spent between sleeps.
pub fn sleep_until(deadline: u64) {
    let request = Timespec::from_nanos(deadline);
    unsafe {
        crate::raw::syscall6(
            nr::CLOCK_NANOSLEEP,
            [
                CLOCK_MONOTONIC,
                TIMER_ABSTIME,
                &raw const request as u64,
                0,
                0,
                0,
            ],
        )
    };
}
