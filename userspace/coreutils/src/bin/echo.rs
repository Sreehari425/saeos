//! `echo [-n] ARGS...`: print the arguments.

#![no_std]
#![no_main]

extern crate alloc;

use libsaeos::prelude::*;
use libsaeos::{Args, entry};

entry!(main);

fn main(_args: Args) -> i32 {
    let mut arguments = coreutils::arguments();
    let newline = arguments.first() != Some(&"-n");
    if !newline {
        arguments.remove(0);
    }
    print!("{}", arguments.join(" "));
    if newline {
        println!();
    }
    0
}
