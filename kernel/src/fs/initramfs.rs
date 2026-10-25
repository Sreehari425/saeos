//! The initial RAM filesystem: a cpio "newc" archive (Linux's initramfs
//! format, written by `tools/mkinitramfs`) unpacked into ramfs at boot.
//!
//! ```text
//! entry   = header(110 bytes) name NUL pad4 data pad4
//! header  = "070701" + 13 fields of 8 hex digits:
//!           ino mode uid gid nlink mtime filesize
//!           devmajor devminor rdevmajor rdevminor namesize check
//! archive = entry* "TRAILER!!!" entry, then optionally zero padding and
//!           further archives (later entries override earlier ones)
//! ```
//!
//! The parser trusts nothing: every field, size and path is checked and a
//! bad archive is an error, never a panic or an out-of-bounds read. Only
//! regular files and directories are supported; hard links are unpacked as
//! independent files.

use super::errno::Errno;
use super::inode::{Inode, InodeKind};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

const MAGIC: &[u8; 6] = b"070701";
const HEADER_LEN: usize = 110;
const TRAILER: &str = "TRAILER!!!";
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpioError {
    /// The archive ends inside a header, name, file body or padding, or
    /// has no trailer.
    Truncated,
    BadMagic,
    /// A header field is not eight hex digits.
    BadNumber,
    BadPath,
    /// A file type other than regular file or directory (the full mode).
    UnsupportedType(u32),
    /// The filesystem rejected an entry.
    Filesystem(Errno),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'a> {
    /// Relative path with no `.`/`..` components, e.g. `etc/hello`.
    pub path: String,
    pub kind: EntryKind,
    pub data: &'a [u8],
}

fn hex(field: &[u8]) -> Result<usize, CpioError> {
    field.iter().try_fold(0usize, |value, &byte| {
        let digit = (byte as char).to_digit(16).ok_or(CpioError::BadNumber)?;
        Ok(value << 4 | digit as usize)
    })
}

/// Reduce a member name to safe relative components; `..` is rejected.
fn clean_path(name: &str) -> Result<String, CpioError> {
    let mut path = String::new();
    for component in name.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(CpioError::BadPath),
            component => {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(component);
            }
        }
    }
    Ok(path)
}

/// `archive[start..start + length]`, or `Truncated`.
fn slice(archive: &[u8], start: usize, length: usize) -> Result<&[u8], CpioError> {
    start
        .checked_add(length)
        .and_then(|end| archive.get(start..end))
        .ok_or(CpioError::Truncated)
}

/// Parse every entry of every concatenated archive, in order.
pub fn parse(archive: &[u8]) -> Result<Vec<Entry<'_>>, CpioError> {
    let mut entries = Vec::new();
    let mut offset = 0;
    loop {
        let header = slice(archive, offset, HEADER_LEN)?;
        if &header[..6] != MAGIC {
            return Err(CpioError::BadMagic);
        }
        let mut fields = [0usize; 13];
        for (index, field) in fields.iter_mut().enumerate() {
            *field = hex(&header[6 + index * 8..14 + index * 8])?;
        }
        let (mode, size, name_size) = (fields[1] as u32, fields[6], fields[11]);
        // The name is NUL-terminated and padded so the data starts 4-aligned
        // relative to the header.
        let name = slice(archive, offset + HEADER_LEN, name_size)?;
        let Some((&0, name)) = name.split_last() else {
            return Err(CpioError::BadPath);
        };
        if name.is_empty() || name.contains(&0) {
            return Err(CpioError::BadPath);
        }
        let name = core::str::from_utf8(name).map_err(|_| CpioError::BadPath)?;
        let data_start = offset + (HEADER_LEN + name_size).next_multiple_of(4);
        let padded_size = size
            .checked_next_multiple_of(4)
            .ok_or(CpioError::Truncated)?;
        slice(archive, data_start, padded_size)?;
        let data = &archive[data_start..data_start + size];
        offset = data_start + padded_size;

        if name == TRAILER {
            // Zero padding may separate concatenated archives.
            while archive.get(offset) == Some(&0) {
                offset += 1;
            }
            if offset == archive.len() {
                return Ok(entries);
            }
            continue;
        }
        let path = clean_path(name)?;
        let kind = match mode & S_IFMT {
            S_IFREG if !path.is_empty() => EntryKind::File,
            S_IFREG => return Err(CpioError::BadPath),
            S_IFDIR if size != 0 => return Err(CpioError::BadNumber),
            // `.` (as written by `find . | cpio`) is the root itself.
            S_IFDIR if path.is_empty() => continue,
            S_IFDIR => EntryKind::Directory,
            _ => return Err(CpioError::UnsupportedType(mode)),
        };
        entries.push(Entry { path, kind, data });
    }
}

/// Unpack `archive` below `root`, creating parent directories as needed.
/// A file that already exists (from an earlier archive) is replaced; an
/// existing directory is kept. Returns `(files, bytes)` unpacked.
pub fn unpack(root: &Arc<dyn Inode>, archive: &[u8]) -> Result<(usize, u64), CpioError> {
    let entries = parse(archive)?;
    let (mut files, mut bytes) = (0, 0u64);
    for entry in &entries {
        let mut directory = root.clone();
        let mut components = entry.path.split('/').peekable();
        while let Some(component) = components.next() {
            let last = components.peek().is_none();
            if last && entry.kind == EntryKind::File {
                let file = match directory.lookup(component) {
                    Ok(existing) if existing.kind() == InodeKind::File => {
                        existing.truncate(0).map_err(CpioError::Filesystem)?;
                        existing
                    }
                    Ok(_) => return Err(CpioError::Filesystem(Errno::EISDIR)),
                    Err(Errno::ENOENT) => directory
                        .create(component, InodeKind::File)
                        .map_err(CpioError::Filesystem)?,
                    Err(error) => return Err(CpioError::Filesystem(error)),
                };
                file.write_at(0, entry.data)
                    .map_err(CpioError::Filesystem)?;
                files += 1;
                bytes += entry.data.len() as u64;
            } else {
                directory = match directory.lookup(component) {
                    Ok(existing) if existing.kind() == InodeKind::Directory => existing,
                    Ok(_) => return Err(CpioError::Filesystem(Errno::ENOTDIR)),
                    Err(Errno::ENOENT) => directory
                        .create(component, InodeKind::Directory)
                        .map_err(CpioError::Filesystem)?,
                    Err(error) => return Err(CpioError::Filesystem(error)),
                };
            }
        }
    }
    Ok((files, bytes))
}
