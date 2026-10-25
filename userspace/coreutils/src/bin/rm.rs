//! `rm PATH...`: remove files, and empty directories.

#![no_std]
#![no_main]

use libsaeos::errno::Errno;
use libsaeos::{Args, entry, fs};

entry!(main);

fn main(_args: Args) -> i32 {
    let mut status = 0;
    for path in coreutils::arguments() {
        let result = match fs::remove_file(path) {
            Err(Errno::EISDIR) => fs::remove_dir(path),
            result => result,
        };
        if let Err(error) = result {
            status = coreutils::report("rm", path, error);
        }
    }
    status
}
