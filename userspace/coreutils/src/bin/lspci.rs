//! `lspci [-v]`: the PCI devices the kernel found (`/proc/pci`); `-v` adds
//! their BARs.

#![no_std]
#![no_main]

use libsaeos::prelude::*;
use libsaeos::{Args, entry, fs};

entry!(main);

fn main(_args: Args) -> i32 {
    let verbose = coreutils::arguments().first() == Some(&"-v");
    match fs::read_to_string("/proc/pci") {
        Ok(text) => {
            for line in text.lines() {
                if verbose || !line.starts_with(' ') {
                    println!("{line}");
                }
            }
            0
        }
        Err(error) => coreutils::report("lspci", "/proc/pci", error),
    }
}
