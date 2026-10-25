//! Filesystem shell commands, all through the syscall-shaped `fs` API on the
//! shell process's own file table.

use crate::fs::{self, Errno, InodeKind, OpenFlags};
use crate::{print, println};

/// Handle `command` if it is a filesystem command; returns false otherwise.
pub fn execute(command: &str) -> bool {
    let (name, rest) = command.split_once(' ').unwrap_or((command, ""));
    let argument = rest.trim();
    match name {
        "ls" => report("ls", ls(if argument.is_empty() { "/" } else { argument })),
        "cat" if !argument.is_empty() => report("cat", cat(argument)),
        "stat" if !argument.is_empty() => report("stat", stat(argument)),
        "mkdir" if !argument.is_empty() => report("mkdir", fs::mkdir(argument)),
        "rm" if !argument.is_empty() => report("rm", remove(argument)),
        "write" => match argument.split_once(' ') {
            Some((path, text)) => report("write", write(path, text)),
            None => println!("Usage: write <path> <text>"),
        },
        "cat" | "stat" | "mkdir" | "rm" => println!("Usage: {} <path>", name),
        _ => return false,
    }
    true
}

fn report(command: &str, result: Result<(), Errno>) {
    if let Err(error) = result {
        println!("{}: {}", command, error);
    }
}

fn kind_label(kind: InodeKind) -> &'static str {
    match kind {
        InodeKind::File => "file",
        InodeKind::Directory => "dir",
        InodeKind::CharDevice => "chardev",
    }
}

fn ls(path: &str) -> Result<(), Errno> {
    let fd = fs::open(path, OpenFlags::READ | OpenFlags::DIRECTORY)?;
    let entries = fs::readdir(fd);
    fs::close(fd)?;
    for entry in entries? {
        let size = fs::stat(&join(path, &entry.name)).map_or(0, |stat| stat.size);
        let suffix = if entry.kind == InodeKind::Directory {
            "/"
        } else {
            ""
        };
        println!(
            "{:>8} {:<7} {}{}",
            size,
            kind_label(entry.kind),
            entry.name,
            suffix
        );
    }
    Ok(())
}

fn join(directory: &str, name: &str) -> alloc::string::String {
    let mut path = alloc::string::String::from(directory.trim_end_matches('/'));
    path.push('/');
    path.push_str(name);
    path
}

fn cat(path: &str) -> Result<(), Errno> {
    let fd = fs::open(path, OpenFlags::READ)?;
    let mut buffer = [0u8; 256];
    let result = loop {
        match fs::read(fd, &mut buffer) {
            Ok(0) => break Ok(()),
            Ok(count) => print!(
                "{}",
                alloc::string::String::from_utf8_lossy(&buffer[..count])
            ),
            Err(error) => break Err(error),
        }
    };
    fs::close(fd)?;
    result
}

fn stat(path: &str) -> Result<(), Errno> {
    let stat = fs::stat(path)?;
    println!(
        "{}: {} size={} ino={} fs={} links={}",
        path,
        kind_label(stat.kind),
        stat.size,
        stat.ino,
        stat.fs_id,
        stat.nlink
    );
    Ok(())
}

fn write(path: &str, text: &str) -> Result<(), Errno> {
    let fd = fs::open(
        path,
        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNC,
    )?;
    let result = fs::write(fd, text.as_bytes()).and_then(|_| fs::write(fd, b"\n"));
    fs::close(fd)?;
    result.map(|_| ())
}

/// `rm` removes a file or an empty directory.
fn remove(path: &str) -> Result<(), Errno> {
    match fs::unlink(path) {
        Err(Errno::EISDIR) => fs::rmdir(path),
        result => result,
    }
}
