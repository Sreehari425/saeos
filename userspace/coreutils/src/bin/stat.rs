//! `stat PATH...`: type, size and inode number.

#![no_std]
#![no_main]

use libsaeos::fs::{self, FileType};
use libsaeos::prelude::*;
use libsaeos::{Args, entry};

entry!(main);

fn main(_args: Args) -> i32 {
    let mut status = 0;
    for path in coreutils::arguments() {
        match fs::metadata(path) {
            Ok(metadata) => {
                let kind = match metadata.file_type {
                    FileType::File => "regular file",
                    FileType::Directory => "directory",
                    FileType::CharDevice => "character device",
                    FileType::Other => "other",
                };
                println!(
                    "{path}: {kind}, {} bytes, inode {}",
                    metadata.size, metadata.inode
                );
            }
            Err(error) => status = coreutils::report("stat", path, error),
        }
    }
    status
}
