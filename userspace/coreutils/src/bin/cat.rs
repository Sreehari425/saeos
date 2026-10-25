//! `cat [FILE...]`: copy files (or stdin) to stdout.

#![no_std]
#![no_main]

extern crate alloc;

use libsaeos::io::{self, File};
use libsaeos::prelude::*;
use libsaeos::{Args, entry};

entry!(main);

fn copy(mut read: impl FnMut(&mut [u8]) -> Result<usize>) -> Result<()> {
    let mut buffer = [0u8; 4096];
    loop {
        match read(&mut buffer)? {
            0 => return Ok(()),
            count => io::write_all(1, &buffer[..count])?,
        }
    }
}

fn main(_args: Args) -> i32 {
    let files = coreutils::arguments();
    if files.is_empty() {
        let stdin = |buffer: &mut [u8]| {
            libsaeos::errno::check(libsaeos::raw::read(0, buffer)).map(|count| count as usize)
        };
        return if copy(stdin).is_ok() { 0 } else { 1 };
    }
    let mut status = 0;
    for path in files {
        let result = File::open(path).and_then(|mut file| copy(|buffer| file.read(buffer)));
        if let Err(error) = result {
            status = coreutils::report("cat", path, error);
        }
    }
    status
}
