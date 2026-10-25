//! The cpio "newc" format (`070701`), as read by Linux and by the SaeOS
//! kernel (`kernel/src/fs/initramfs.rs`).
//!
//! ```text
//! entry   = header(110 bytes) name NUL pad4 data pad4
//! header  = "070701" + 13 fields of 8 hex digits:
//!           ino mode uid gid nlink mtime filesize
//!           devmajor devminor rdevmajor rdevminor namesize check
//! archive = entry* "TRAILER!!!" entry
//! ```
//!
//! Padding aligns to 4 bytes counted from the start of the entry's header.

pub const MAGIC: &[u8; 6] = b"070701";
pub const HEADER_LEN: usize = 110;
pub const TRAILER: &str = "TRAILER!!!";

pub const S_IFMT: u32 = 0o170000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;

/// One archive member. `path` is relative (`etc/hello`), as Linux writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub path: String,
    pub mode: u32,
    pub data: Vec<u8>,
}

/// Append one entry. Any mode is accepted, so tests can write types the
/// kernel must reject.
pub fn push_entry(archive: &mut Vec<u8>, ino: u32, mode: u32, path: &str, data: &[u8]) {
    let start = archive.len();
    let nlink = if mode & S_IFMT == S_IFDIR { 2 } else { 1 };
    let fields = [
        ino,
        mode,
        0, // uid
        0, // gid
        nlink,
        0, // mtime: fixed, so builds are reproducible
        u32::try_from(data.len()).expect("cpio newc files are limited to 4 GiB"),
        0,
        0,
        0,
        0,
        u32::try_from(path.len() + 1).expect("path length"),
        0, // check: only used by the "070702" variant
    ];
    archive.extend_from_slice(MAGIC);
    for field in fields {
        archive.extend_from_slice(format!("{field:08x}").as_bytes());
    }
    archive.extend_from_slice(path.as_bytes());
    archive.push(0);
    pad(archive, start);
    archive.extend_from_slice(data);
    pad(archive, start);
}

fn pad(archive: &mut Vec<u8>, start: usize) {
    let length = archive.len() - start;
    archive.resize(start + length.next_multiple_of(4), 0);
}

/// Write `members` in order, numbering inodes from 1, then the trailer.
pub fn write(members: &[Member]) -> Vec<u8> {
    let mut archive = Vec::new();
    for (member, ino) in members.iter().zip(1u32..) {
        push_entry(&mut archive, ino, member.mode, &member.path, &member.data);
    }
    push_entry(&mut archive, 0, 0, TRAILER, &[]);
    archive
}

/// Read back an archive written by this tool (for `mkinitramfs list` and
/// tests). Stops at the first trailer.
pub fn read(archive: &[u8]) -> Result<Vec<Member>, String> {
    let mut members = Vec::new();
    let mut offset = 0;
    loop {
        let header = archive
            .get(offset..offset + HEADER_LEN)
            .ok_or("truncated header")?;
        if &header[..6] != MAGIC {
            return Err(format!("bad magic at offset {offset}"));
        }
        let field = |index: usize| -> Result<usize, String> {
            let text = std::str::from_utf8(&header[6 + index * 8..14 + index * 8])
                .map_err(|_| "non-ASCII header field")?;
            usize::from_str_radix(text, 16).map_err(|_| format!("bad hex field {text:?}"))
        };
        let (mode, size, name_size) = (field(1)?, field(6)?, field(11)?);
        let name_start = offset + HEADER_LEN;
        let name = archive
            .get(name_start..name_start + name_size.saturating_sub(1))
            .ok_or("truncated name")?;
        let path = String::from_utf8(name.to_vec()).map_err(|_| "non-UTF-8 name")?;
        let data_start = offset + (HEADER_LEN + name_size).next_multiple_of(4);
        let data = archive
            .get(data_start..data_start + size)
            .ok_or("truncated data")?;
        if path == TRAILER {
            return Ok(members);
        }
        members.push(Member {
            path,
            mode: mode as u32,
            data: data.to_vec(),
        });
        offset = data_start + size.next_multiple_of(4);
    }
}
