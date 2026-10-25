//! The initial RAM filesystem: a POSIX ustar archive unpacked into ramfs at
//! boot. The parser trusts nothing: every header is checked (magic,
//! checksum, octal fields, sizes, paths) and a bad archive is an error,
//! never a panic or an out-of-bounds read.

use super::errno::Errno;
use super::inode::{Inode, InodeKind};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

const BLOCK: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarError {
    /// The archive ends inside a header or file body.
    Truncated,
    BadMagic,
    BadChecksum,
    BadNumber,
    BadPath,
    UnsupportedType(u8),
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

/// Parse an octal field (NUL- or space-terminated, optional leading spaces).
fn octal(field: &[u8]) -> Result<u64, TarError> {
    let mut value: u64 = 0;
    let mut digits = 0;
    for &byte in field {
        match byte {
            b'0'..=b'7' => {
                value = value
                    .checked_mul(8)
                    .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                    .ok_or(TarError::BadNumber)?;
                digits += 1;
            }
            b' ' if digits == 0 => {}
            0 | b' ' => break,
            _ => return Err(TarError::BadNumber),
        }
    }
    if digits == 0 {
        return Err(TarError::BadNumber);
    }
    Ok(value)
}

/// A NUL-terminated text field.
fn text(field: &[u8]) -> Result<&str, TarError> {
    let length = field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len());
    core::str::from_utf8(&field[..length]).map_err(|_| TarError::BadPath)
}

/// Join ustar `prefix` and `name` and reduce to safe relative components.
fn clean_path(prefix: &str, name: &str) -> Result<String, TarError> {
    let mut path = String::new();
    for component in prefix.split('/').chain(name.split('/')) {
        match component {
            "" | "." => {}
            ".." => return Err(TarError::BadPath),
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

/// Parse every entry. The archive ends at the first all-zero header.
pub fn parse(archive: &[u8]) -> Result<Vec<Entry<'_>>, TarError> {
    let mut entries = Vec::new();
    let mut offset = 0;
    loop {
        let header = archive
            .get(offset..offset + BLOCK)
            .ok_or(TarError::Truncated)?;
        if header.iter().all(|&byte| byte == 0) {
            return Ok(entries);
        }
        if &header[257..263] != b"ustar\0" {
            return Err(TarError::BadMagic);
        }
        // The checksum covers the header with its own field read as spaces.
        let stored = octal(&header[148..156])?;
        let computed: u64 = header
            .iter()
            .enumerate()
            .map(|(index, &byte)| {
                if (148..156).contains(&index) {
                    u64::from(b' ')
                } else {
                    u64::from(byte)
                }
            })
            .sum();
        if stored != computed {
            return Err(TarError::BadChecksum);
        }
        let size = usize::try_from(octal(&header[124..136])?).map_err(|_| TarError::BadNumber)?;
        let path = clean_path(text(&header[345..500])?, text(&header[0..100])?)?;
        if path.is_empty() {
            return Err(TarError::BadPath);
        }
        let kind = match header[156] {
            b'0' | 0 => EntryKind::File,
            b'5' => EntryKind::Directory,
            other => return Err(TarError::UnsupportedType(other)),
        };
        let data_start = offset + BLOCK;
        let data_end = data_start.checked_add(size).ok_or(TarError::Truncated)?;
        let data = archive
            .get(data_start..data_end)
            .ok_or(TarError::Truncated)?;
        if kind == EntryKind::Directory && size != 0 {
            return Err(TarError::BadNumber);
        }
        entries.push(Entry { path, kind, data });
        offset = data_start + size.div_ceil(BLOCK) * BLOCK;
    }
}

/// Unpack `archive` below `root`, creating parent directories as needed.
/// Returns `(files, bytes)` unpacked.
pub fn unpack(root: &Arc<dyn Inode>, archive: &[u8]) -> Result<(usize, u64), TarError> {
    let entries = parse(archive)?;
    let (mut files, mut bytes) = (0, 0u64);
    for entry in &entries {
        let mut directory = root.clone();
        let mut components = entry.path.split('/').peekable();
        while let Some(component) = components.next() {
            let last = components.peek().is_none();
            if last && entry.kind == EntryKind::File {
                let file = directory
                    .create(component, InodeKind::File)
                    .map_err(TarError::Filesystem)?;
                file.write_at(0, entry.data).map_err(TarError::Filesystem)?;
                files += 1;
                bytes += entry.data.len() as u64;
            } else {
                directory = match directory.lookup(component) {
                    Ok(existing) if existing.kind() == InodeKind::Directory => existing,
                    Ok(_) => return Err(TarError::Filesystem(Errno::ENOTDIR)),
                    Err(Errno::ENOENT) => directory
                        .create(component, InodeKind::Directory)
                        .map_err(TarError::Filesystem)?,
                    Err(error) => return Err(TarError::Filesystem(error)),
                };
            }
        }
    }
    Ok((files, bytes))
}
