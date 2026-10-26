//! `fbinfo`: the framebuffer's mode (like Linux's `fbset -i`), from
//! `/proc/fb`.

#![no_std]
#![no_main]

extern crate alloc;

use libsaeos::prelude::*;
use libsaeos::{Args, entry};

entry!(main);

fn main(_args: Args) -> i32 {
    match libsaeos::fs::read_to_string("/proc/fb") {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(error) => coreutils::report("fbinfo", "/proc/fb", error),
    }
}
