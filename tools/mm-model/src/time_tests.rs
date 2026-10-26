//! Clocks and sleeps through the syscall table, against a fake clock that
//! a sleep advances to its deadline.

use crate::fs::Errno;
use crate::syscall::numbers::{CLOCK_GETTIME, CLOCK_NANOSLEEP, NANOSLEEP};
use crate::syscall::time::{
    CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_REALTIME, TIMER_ABSTIME, days_from_civil,
    decode_timespec, encode_timespec, unix_seconds,
};
use crate::syscall_tests::{BASE, World, errno};

const SECOND: u64 = 1_000_000_000;

fn timespec(seconds: i64, nanoseconds: i64) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&seconds.to_le_bytes());
    bytes[8..].copy_from_slice(&nanoseconds.to_le_bytes());
    bytes
}

fn read_timespec(world: &World, offset: u64) -> (i64, i64) {
    let bytes = world.user(offset, 16);
    (
        i64::from_le_bytes(bytes[..8].try_into().unwrap()),
        i64::from_le_bytes(bytes[8..].try_into().unwrap()),
    )
}

#[test]
fn civil_dates() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(1969, 12, 31), -1);
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    // 2000 is a leap year, 1900 and 2100 are not.
    assert_eq!(
        days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28),
        2
    );
    assert_eq!(
        days_from_civil(2100, 3, 1) - days_from_civil(2100, 2, 28),
        1
    );
    assert_eq!(unix_seconds(2038, 1, 19, 3, 14, 8), 1 << 31);
    assert_eq!(unix_seconds(2026, 10, 26, 0, 0, 0), 1_792_972_800);
}

#[test]
fn timespec_encoding() {
    assert_eq!(encode_timespec(3 * SECOND + 7), timespec(3, 7));
    assert_eq!(
        decode_timespec(&timespec(2, 999_999_999)),
        Ok(3 * SECOND - 1)
    );
    assert_eq!(
        decode_timespec(&timespec(0, SECOND as i64)),
        Err(Errno::EINVAL)
    );
    assert_eq!(decode_timespec(&timespec(0, -1)), Err(Errno::EINVAL));
    assert_eq!(decode_timespec(&timespec(-1, 0)), Err(Errno::EINVAL));
}

#[test]
fn clock_gettime_reads_each_clock() {
    let world = World::new();
    world.process.clock.set(5 * SECOND + 42);
    for clock in [CLOCK_MONOTONIC, CLOCK_BOOTTIME] {
        assert_eq!(world.call(CLOCK_GETTIME, &[clock, BASE + 100]), 0);
        assert_eq!(read_timespec(&world, 100), (5, 42));
    }
    // No wall clock: CLOCK_REALTIME is not available.
    assert_eq!(
        world.call(CLOCK_GETTIME, &[CLOCK_REALTIME, BASE + 100]),
        errno(Errno::EINVAL)
    );
    world.process.epoch.set(Some(1_000 * SECOND));
    assert_eq!(world.call(CLOCK_GETTIME, &[CLOCK_REALTIME, BASE + 100]), 0);
    assert_eq!(read_timespec(&world, 100), (1_005, 42));
    assert_eq!(
        world.call(CLOCK_GETTIME, &[99, BASE + 100]),
        errno(Errno::EINVAL)
    );
    assert_eq!(
        world.call(CLOCK_GETTIME, &[CLOCK_MONOTONIC, 0x10]),
        errno(Errno::EFAULT)
    );
}

#[test]
fn nanosleep_sleeps_the_whole_duration() {
    let world = World::new();
    world.process.clock.set(10 * SECOND);
    let request = world.put(0, &timespec(1, 500_000_000));
    assert_eq!(world.call(NANOSLEEP, &[request, 0]), 0);
    assert_eq!(*world.process.sleeps.borrow(), [11 * SECOND + SECOND / 2]);
    // rem is never written: a sleep is never cut short.
    let remaining = world.put(100, &[0xaa; 16]);
    assert_eq!(world.call(NANOSLEEP, &[request, remaining]), 0);
    assert_eq!(world.user(100, 16), [0xaa; 16]);

    let bad = world.put(200, &timespec(0, SECOND as i64));
    assert_eq!(world.call(NANOSLEEP, &[bad, 0]), errno(Errno::EINVAL));
    assert_eq!(world.call(NANOSLEEP, &[0x10, 0]), errno(Errno::EFAULT));
    assert_eq!(world.process.sleeps.borrow().len(), 2, "no sleep on errors");
}

#[test]
fn clock_nanosleep_absolute_and_relative() {
    let world = World::new();
    world.process.clock.set(10 * SECOND);
    // Absolute: until monotonic time 12 s.
    let deadline = world.put(0, &timespec(12, 0));
    assert_eq!(
        world.call(
            CLOCK_NANOSLEEP,
            &[CLOCK_MONOTONIC, TIMER_ABSTIME, deadline, 0]
        ),
        0
    );
    assert_eq!(world.process.clock.get(), 12 * SECOND);
    // A deadline already past returns at once.
    assert_eq!(
        world.call(
            CLOCK_NANOSLEEP,
            &[CLOCK_MONOTONIC, TIMER_ABSTIME, deadline, 0]
        ),
        0
    );
    assert_eq!(world.process.clock.get(), 12 * SECOND);
    // Relative.
    let quarter = world.put(100, &timespec(0, 250_000_000));
    assert_eq!(
        world.call(CLOCK_NANOSLEEP, &[CLOCK_MONOTONIC, 0, quarter, 0]),
        0
    );
    assert_eq!(world.process.clock.get(), 12 * SECOND + SECOND / 4);
    // Absolute on the wall clock: converted to a monotonic deadline.
    world.process.epoch.set(Some(1_000 * SECOND));
    let wall = world.put(200, &timespec(1_013, 0));
    assert_eq!(
        world.call(CLOCK_NANOSLEEP, &[CLOCK_REALTIME, TIMER_ABSTIME, wall, 0]),
        0
    );
    assert_eq!(world.process.clock.get(), 13 * SECOND);

    assert_eq!(
        world.call(CLOCK_NANOSLEEP, &[CLOCK_MONOTONIC, 2, quarter, 0]),
        errno(Errno::EINVAL),
        "unknown flags"
    );
    assert_eq!(
        world.call(CLOCK_NANOSLEEP, &[42, 0, quarter, 0]),
        errno(Errno::EINVAL),
        "unknown clock"
    );
}
