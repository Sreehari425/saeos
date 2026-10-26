//! Shared by the tools in `src/bin`.

#![no_std]

extern crate alloc;

pub mod bav;
pub mod wav;

use libsaeos::prelude::*;

/// Report `error` for `path` like the Unix tools do and return exit code 1.
pub fn report(tool: &str, path: &str, error: Errno) -> i32 {
    eprintln!("{tool}: {path}: {error}");
    1
}

/// Read until `buffer` is full or the file ends; returns the bytes read.
pub fn read_full(file: &mut libsaeos::io::File, buffer: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    Ok(filled)
}

/// The arguments after the program name.
pub fn arguments() -> Vec<&'static str> {
    libsaeos::env::args().skip(1).collect()
}
