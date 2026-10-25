//! The first SaeOS user program.

#![no_std]
#![no_main]

use libsaeos::prelude::*;
use libsaeos::{Args, entry, env};

entry!(main);

fn main(_args: Args) -> i32 {
    let name = env::args().next().unwrap_or("hello");
    println!(
        "hello from userspace ({name}, pid {})",
        libsaeos::process::id()
    );
    0
}
