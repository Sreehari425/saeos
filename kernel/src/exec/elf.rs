//! ELF64 executables: validation and a load plan.
//!
//! Only what SaeOS can run is accepted: little-endian x86_64 `ET_EXEC`
//! files with `PT_LOAD` segments that are in bounds, below `USER_TOP`,
//! page-congruent, non-overlapping, and never writable and executable at
//! once. Dynamic executables (`PT_INTERP`, `PT_DYNAMIC`) are rejected; there
//! is no dynamic linker. Every check is an error (`ENOEXEC`), never a panic.

use crate::fs::Errno;
use alloc::vec::Vec;

pub const PAGE_SIZE: u64 = 4096;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
/// Far more than any real static executable has.
const MAX_PROGRAM_HEADERS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentFlags {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

/// One `PT_LOAD` segment, rounded out to whole pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// First page of the segment.
    pub start: u64,
    /// One past the last page (page-aligned).
    pub end: u64,
    pub flags: SegmentFlags,
    /// The segment's file bytes go at `vaddr`, `file_offset..+file_size`;
    /// the rest up to `vaddr + mem_size` (`.bss`) is zero.
    pub vaddr: u64,
    pub file_offset: u64,
    pub file_size: u64,
    pub mem_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPlan {
    pub entry: u64,
    /// Where the program headers are in the loaded image (`AT_PHDR`).
    pub phdr_vaddr: u64,
    pub phnum: u64,
    pub segments: Vec<Segment>,
}

fn u16_at(file: &[u8], offset: usize) -> Result<u16, Errno> {
    file.get(offset..offset + 2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or(Errno::ENOEXEC)
}

fn u32_at(file: &[u8], offset: usize) -> Result<u32, Errno> {
    file.get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        .ok_or(Errno::ENOEXEC)
}

fn u64_at(file: &[u8], offset: usize) -> Result<u64, Errno> {
    file.get(offset..offset + 8)
        .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
        .ok_or(Errno::ENOEXEC)
}

/// Validate `file` and describe how to load it below `user_top`.
pub fn plan(file: &[u8], user_top: u64) -> Result<LoadPlan, Errno> {
    let header = file.get(..HEADER_SIZE).ok_or(Errno::ENOEXEC)?;
    // ELF magic, 64-bit, little-endian, version 1.
    if header[..4] != *b"\x7fELF" || header[4] != 2 || header[5] != 1 || header[6] != 1 {
        return Err(Errno::ENOEXEC);
    }
    if u16_at(file, 16)? != ET_EXEC || u16_at(file, 18)? != EM_X86_64 || u32_at(file, 20)? != 1 {
        return Err(Errno::ENOEXEC);
    }
    let entry = u64_at(file, 24)?;
    let phoff = u64_at(file, 32)?;
    let phentsize = usize::from(u16_at(file, 54)?);
    let phnum = usize::from(u16_at(file, 56)?);
    if phentsize != PROGRAM_HEADER_SIZE || phnum == 0 || phnum > MAX_PROGRAM_HEADERS {
        return Err(Errno::ENOEXEC);
    }
    let table_start = usize::try_from(phoff).map_err(|_| Errno::ENOEXEC)?;
    let table_end = table_start
        .checked_add(phnum * PROGRAM_HEADER_SIZE)
        .ok_or(Errno::ENOEXEC)?;
    if table_end > file.len() {
        return Err(Errno::ENOEXEC);
    }

    let mut segments: Vec<Segment> = Vec::new();
    let mut phdr_vaddr = None;
    for index in 0..phnum {
        let base = table_start + index * PROGRAM_HEADER_SIZE;
        let kind = u32_at(file, base)?;
        let flags = u32_at(file, base + 4)?;
        let offset = u64_at(file, base + 8)?;
        let vaddr = u64_at(file, base + 16)?;
        let file_size = u64_at(file, base + 32)?;
        let mem_size = u64_at(file, base + 40)?;
        match kind {
            PT_INTERP | PT_DYNAMIC => return Err(Errno::ENOEXEC),
            PT_PHDR => phdr_vaddr = Some(vaddr),
            PT_LOAD => {}
            _ => continue,
        }
        if kind != PT_LOAD || mem_size == 0 {
            continue;
        }
        let flags = SegmentFlags {
            read: flags & PF_R != 0,
            write: flags & PF_W != 0,
            execute: flags & PF_X != 0,
        };
        let file_end = offset.checked_add(file_size).ok_or(Errno::ENOEXEC)?;
        let mem_end = vaddr.checked_add(mem_size).ok_or(Errno::ENOEXEC)?;
        if file_size > mem_size
            || file_end > file.len() as u64
            || vaddr % PAGE_SIZE != offset % PAGE_SIZE
            || (flags.write && flags.execute)
            || vaddr < PAGE_SIZE
        {
            return Err(Errno::ENOEXEC);
        }
        let start = vaddr & !(PAGE_SIZE - 1);
        let end = mem_end
            .checked_next_multiple_of(PAGE_SIZE)
            .ok_or(Errno::ENOEXEC)?;
        if end > user_top
            || segments
                .iter()
                .any(|other| start < other.end && other.start < end)
        {
            return Err(Errno::ENOEXEC);
        }
        segments.push(Segment {
            start,
            end,
            flags,
            vaddr,
            file_offset: offset,
            file_size,
            mem_size,
        });
    }
    if segments.is_empty() {
        return Err(Errno::ENOEXEC);
    }
    let executable = |address: u64| {
        segments.iter().any(|segment| {
            segment.flags.execute
                && segment.vaddr <= address
                && address < segment.vaddr + segment.mem_size
        })
    };
    if !executable(entry) {
        return Err(Errno::ENOEXEC);
    }
    // Without PT_PHDR, the headers are visible if a segment maps their
    // file bytes (static executables normally load them in the first one).
    let phdr_vaddr = phdr_vaddr
        .or_else(|| {
            segments
                .iter()
                .find(|segment| {
                    segment.file_offset <= phoff
                        && phoff + (phnum * PROGRAM_HEADER_SIZE) as u64
                            <= segment.file_offset + segment.file_size
                })
                .map(|segment| segment.vaddr + (phoff - segment.file_offset))
        })
        .unwrap_or(0);
    Ok(LoadPlan {
        entry,
        phdr_vaddr,
        phnum: phnum as u64,
        segments,
    })
}

impl LoadPlan {
    /// The bytes of page `page` (a page-aligned address inside `segment`)
    /// as loaded: file bytes where the segment has them, zero elsewhere.
    pub fn page_contents(segment: &Segment, file: &[u8], page: u64, out: &mut [u8; 4096]) {
        out.fill(0);
        let data_start = segment.vaddr;
        let data_end = segment.vaddr + segment.file_size;
        let from = page.max(data_start);
        let to = (page + PAGE_SIZE).min(data_end);
        if from < to {
            let source = (segment.file_offset + (from - segment.vaddr)) as usize;
            let destination = (from - page) as usize;
            let length = (to - from) as usize;
            out[destination..destination + length].copy_from_slice(&file[source..source + length]);
        }
    }
}
