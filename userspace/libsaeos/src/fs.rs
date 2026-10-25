//! Files and directories by path.

use crate::errno::{Errno, Result, check};
use crate::io::{File, c_path};
use crate::raw;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    CharDevice,
    Other,
}

impl FileType {
    fn from_mode(mode: u32) -> Self {
        match mode & 0o170000 {
            0o100000 => Self::File,
            0o040000 => Self::Directory,
            0o020000 => Self::CharDevice,
            _ => Self::Other,
        }
    }

    fn from_dirent(kind: u8) -> Self {
        match kind {
            8 => Self::File,
            4 => Self::Directory,
            2 => Self::CharDevice,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub file_type: FileType,
    pub size: u64,
    pub inode: u64,
}

pub fn metadata(path: &str) -> Result<Metadata> {
    let mut stat = [0u8; 144];
    check(raw::stat(&c_path(path)?, &mut stat))?;
    let field = |offset: usize| u64::from_le_bytes(stat[offset..offset + 8].try_into().unwrap());
    Ok(Metadata {
        file_type: FileType::from_mode(field(24) as u32),
        size: field(48),
        inode: field(8),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub file_type: FileType,
}

/// Every entry of a directory (without `.` and `..`), in the order the
/// filesystem lists them.
pub fn read_dir(path: &str) -> Result<Vec<DirEntry>> {
    const O_DIRECTORY: u64 = 0o200000;
    let fd = check(raw::open(&c_path(path)?, raw::O_RDONLY | O_DIRECTORY))? as u32;
    let mut entries = Vec::new();
    let mut buffer = [0u8; 4096];
    let result = loop {
        let count = match check(raw::getdents64(fd, &mut buffer)) {
            Ok(0) => break Ok(entries),
            Ok(count) => count as usize,
            Err(error) => break Err(error),
        };
        // struct linux_dirent64: ino, off, reclen (u16), type (u8), name.
        let mut offset = 0;
        while offset + 19 <= count {
            let record = &buffer[offset..count];
            let length = usize::from(u16::from_le_bytes([record[16], record[17]]));
            if length < 19 || length > record.len() {
                break;
            }
            let name = &record[19..length];
            let name = &name[..name
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(name.len())];
            entries.push(DirEntry {
                name: String::from_utf8_lossy(name).into_owned(),
                file_type: FileType::from_dirent(record[18]),
            });
            offset += length;
        }
    };
    raw::close(fd);
    result
}

pub fn read(path: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn read_to_string(path: &str) -> Result<String> {
    String::from_utf8(read(path)?).map_err(|_| Errno::EINVAL)
}

pub fn write(path: &str, bytes: &[u8]) -> Result<()> {
    File::create(path)?.write_all(bytes)
}

pub fn create_dir(path: &str) -> Result<()> {
    check(raw::mkdir(&c_path(path)?, 0o755)).map(|_| ())
}

pub fn remove_file(path: &str) -> Result<()> {
    check(raw::unlink(&c_path(path)?)).map(|_| ())
}

pub fn remove_dir(path: &str) -> Result<()> {
    check(raw::rmdir(&c_path(path)?)).map(|_| ())
}

pub fn rename(old: &str, new: &str) -> Result<()> {
    check(raw::rename(&c_path(old)?, &c_path(new)?)).map(|_| ())
}
