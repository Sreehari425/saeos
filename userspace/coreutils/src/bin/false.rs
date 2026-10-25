//! `false`: exit with status 1.

#![no_std]
#![no_main]

use libsaeos::{Args, entry};

entry!(main);

fn main(_args: Args) -> i32 {
    1
}
