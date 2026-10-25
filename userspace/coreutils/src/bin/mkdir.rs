//! `mkdir DIR...`

#![no_std]
#![no_main]

use libsaeos::{Args, entry, fs};

entry!(main);

fn main(_args: Args) -> i32 {
    let mut status = 0;
    for path in coreutils::arguments() {
        if let Err(error) = fs::create_dir(path) {
            status = coreutils::report("mkdir", path, error);
        }
    }
    status
}
