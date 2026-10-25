//! `mv FROM TO`

#![no_std]
#![no_main]

use libsaeos::prelude::*;
use libsaeos::{Args, entry, fs};

entry!(main);

fn main(_args: Args) -> i32 {
    let arguments = coreutils::arguments();
    let [from, to] = arguments[..] else {
        eprintln!("usage: mv FROM TO");
        return 2;
    };
    match fs::rename(from, to) {
        Ok(()) => 0,
        Err(error) => coreutils::report("mv", from, error),
    }
}
