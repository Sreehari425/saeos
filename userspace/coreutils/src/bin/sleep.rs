//! `sleep SECONDS`: wait, then exit. `SECONDS` may have a fraction
//! (`sleep 0.25`).

#![no_std]
#![no_main]

extern crate alloc;

use libsaeos::prelude::*;
use libsaeos::time::{self, NANOS_PER_SECOND};
use libsaeos::{Args, entry};

entry!(main);

/// `"1.5"` as nanoseconds, exactly (no floating point).
fn parse(text: &str) -> Option<u64> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if (whole.is_empty() && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let seconds: u64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let mut nanoseconds = 0;
    let mut scale = NANOS_PER_SECOND / 10;
    for digit in fraction.bytes().take(9) {
        nanoseconds += u64::from(digit - b'0') * scale;
        scale /= 10;
    }
    seconds
        .checked_mul(NANOS_PER_SECOND)?
        .checked_add(nanoseconds)
}

fn main(_args: Args) -> i32 {
    let arguments = coreutils::arguments();
    let [seconds] = arguments.as_slice() else {
        eprintln!("usage: sleep SECONDS");
        return 1;
    };
    let Some(duration) = parse(seconds) else {
        eprintln!("sleep: invalid time '{seconds}'");
        return 1;
    };
    time::sleep(duration);
    0
}
