//! `ls [-l] [PATH...]`: list directories (sorted), or name files.

#![no_std]
#![no_main]

extern crate alloc;

use libsaeos::fs::{self, FileType};
use libsaeos::prelude::*;
use libsaeos::{Args, entry};

entry!(main);

fn kind(file_type: FileType) -> &'static str {
    match file_type {
        FileType::Directory => "dir",
        FileType::File => "file",
        FileType::CharDevice => "chardev",
        FileType::Other => "other",
    }
}

fn show(name: &str, path: &str, long: bool) {
    if !long {
        println!("{name}");
        return;
    }
    match fs::metadata(path) {
        Ok(metadata) => println!(
            "{:<7} {:>8} {name}",
            kind(metadata.file_type),
            metadata.size
        ),
        Err(_) => println!("?       {:>8} {name}", "?"),
    }
}

fn main(_args: Args) -> i32 {
    let mut arguments = coreutils::arguments();
    let long = arguments.first() == Some(&"-l");
    if long {
        arguments.remove(0);
    }
    if arguments.is_empty() {
        arguments.push(".");
    }
    let mut status = 0;
    let headings = arguments.len() > 1;
    for path in &arguments {
        match fs::metadata(path) {
            Ok(metadata) if metadata.file_type == FileType::Directory => {
                let mut entries = match fs::read_dir(path) {
                    Ok(entries) => entries,
                    Err(error) => {
                        status = coreutils::report("ls", path, error);
                        continue;
                    }
                };
                entries.sort_by(|a, b| a.name.cmp(&b.name));
                if headings {
                    println!("{path}:");
                }
                for entry in entries {
                    show(
                        &entry.name,
                        &format!("{}/{}", path.trim_end_matches('/'), entry.name),
                        long,
                    );
                }
            }
            Ok(_) => show(path, path, long),
            Err(error) => status = coreutils::report("ls", path, error),
        }
    }
    status
}
