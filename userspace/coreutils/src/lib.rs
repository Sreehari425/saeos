//! Shared by the tools in `src/bin`.

#![no_std]

extern crate alloc;

use libsaeos::prelude::*;

/// Report `error` for `path` like the Unix tools do and return exit code 1.
pub fn report(tool: &str, path: &str, error: Errno) -> i32 {
    eprintln!("{tool}: {path}: {error}");
    1
}

/// The arguments after the program name.
pub fn arguments() -> Vec<&'static str> {
    libsaeos::env::args().skip(1).collect()
}
