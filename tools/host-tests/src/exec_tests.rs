//! ELF load plans and the initial user stack.

use crate::elf::{self, LoadPlan, PAGE_SIZE, SegmentFlags};
use crate::exec_stack::{self as stack, AT_ENTRY, AT_NULL, AT_PAGESZ, AT_RANDOM};
use crate::fs::Errno;

const USER_TOP: u64 = 0x7fff_ffff_f000;

/// A program header for the test writer.
#[derive(Clone, Copy)]
struct Phdr {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    file_size: u64,
    mem_size: u64,
}

const R: u32 = 4;
const W: u32 = 2;
const X: u32 = 1;

fn load(flags: u32, offset: u64, vaddr: u64, file_size: u64, mem_size: u64) -> Phdr {
    Phdr {
        kind: 1,
        flags,
        offset,
        vaddr,
        file_size,
        mem_size,
    }
}

/// Write a minimal ELF64 executable: header, program headers at 64, and
/// the file padded to `length`, with every byte after the headers set to a
/// position-derived pattern.
fn write_elf(entry: u64, headers: &[Phdr], length: usize) -> Vec<u8> {
    let mut file: Vec<u8> = (0..length).map(|index| (index * 13 + 5) as u8).collect();
    let header_end = 64 + headers.len() * 56;
    file[..header_end].fill(0);
    file[..4].copy_from_slice(b"\x7fELF");
    file[4] = 2; // 64-bit
    file[5] = 1; // little-endian
    file[6] = 1; // version
    file[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    file[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    file[20..24].copy_from_slice(&1u32.to_le_bytes());
    file[24..32].copy_from_slice(&entry.to_le_bytes());
    file[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    file[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    file[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    file[56..58].copy_from_slice(&(headers.len() as u16).to_le_bytes());
    for (index, header) in headers.iter().enumerate() {
        let base = 64 + index * 56;
        file[base..base + 4].copy_from_slice(&header.kind.to_le_bytes());
        file[base + 4..base + 8].copy_from_slice(&header.flags.to_le_bytes());
        file[base + 8..base + 16].copy_from_slice(&header.offset.to_le_bytes());
        file[base + 16..base + 24].copy_from_slice(&header.vaddr.to_le_bytes());
        file[base + 24..base + 32].copy_from_slice(&header.vaddr.to_le_bytes());
        file[base + 32..base + 40].copy_from_slice(&header.file_size.to_le_bytes());
        file[base + 40..base + 48].copy_from_slice(&header.mem_size.to_le_bytes());
        file[base + 48..base + 56].copy_from_slice(&PAGE_SIZE.to_le_bytes());
    }
    file
}

/// The layout `userspace/user.ld` produces: R headers+rodata, RX text, RW
/// data with a multi-page .bss.
fn typical() -> (Vec<Phdr>, Vec<u8>) {
    let headers = vec![
        Phdr {
            kind: 6,
            flags: R,
            offset: 64,
            vaddr: 0x400040,
            file_size: 4 * 56,
            mem_size: 4 * 56,
        },
        load(R, 0, 0x400000, 0x300, 0x300),
        load(R | X, 0x1000, 0x401000, 0x1234, 0x1234),
        load(R | W, 0x3000, 0x403000, 0x80, 0x3080),
    ];
    let file = write_elf(0x401010, &headers, 0x3080);
    (headers, file)
}

#[test]
fn a_typical_static_executable_plans_three_segments() {
    let (_, file) = typical();
    let plan = elf::plan(&file, USER_TOP).unwrap();
    assert_eq!(plan.entry, 0x401010);
    assert_eq!(plan.phdr_vaddr, 0x400040);
    assert_eq!(plan.phnum, 4);
    let summary: Vec<_> = plan
        .segments
        .iter()
        .map(|segment| (segment.start, segment.end, segment.flags))
        .collect();
    let flags = |read, write, execute| SegmentFlags {
        read,
        write,
        execute,
    };
    assert_eq!(
        summary,
        [
            (0x400000, 0x401000, flags(true, false, false)),
            (0x401000, 0x403000, flags(true, false, true)),
            (0x403000, 0x407000, flags(true, true, false)),
        ]
    );
    // Page contents: file bytes, then zeros for the rest of the page and
    // for .bss pages.
    let mut page = [0u8; 4096];
    let text = &plan.segments[1];
    LoadPlan::page_contents(text, &file, 0x402000, &mut page);
    assert_eq!(&page[..0x234], &file[0x2000..0x2234]);
    assert!(page[0x234..].iter().all(|&byte| byte == 0));
    let data = &plan.segments[2];
    LoadPlan::page_contents(data, &file, 0x403000, &mut page);
    assert_eq!(&page[..0x80], &file[0x3000..0x3080]);
    assert!(
        page[0x80..].iter().all(|&byte| byte == 0),
        ".bss starts zeroed"
    );
    LoadPlan::page_contents(data, &file, 0x406000, &mut page);
    assert!(page.iter().all(|&byte| byte == 0));
}

#[test]
fn headers_without_pt_phdr_are_found_in_the_first_segment() {
    let headers = vec![load(R | X, 0, 0x400000, 0x200, 0x200)];
    let file = write_elf(0x400100, &headers, 0x200);
    let plan = elf::plan(&file, USER_TOP).unwrap();
    assert_eq!(plan.phdr_vaddr, 0x400040);
}

#[test]
fn every_kind_of_damage_is_enoexec() {
    let (headers, file) = typical();
    let rejected = |file: &[u8]| elf::plan(file, USER_TOP) == Err(Errno::ENOEXEC);
    // Truncation anywhere before the last segment's end.
    let step = if cfg!(miri) { 97 } else { 1 };
    for length in (0..file.len()).step_by(step) {
        assert!(rejected(&file[..length]), "truncated to {length}");
    }
    let patched = |offset: usize, bytes: &[u8]| {
        let mut copy = file.clone();
        copy[offset..offset + bytes.len()].copy_from_slice(bytes);
        copy
    };
    assert!(rejected(&patched(0, b"\x7fELG")), "magic");
    assert!(rejected(&patched(4, &[1])), "32-bit");
    assert!(rejected(&patched(5, &[2])), "big-endian");
    assert!(rejected(&patched(16, &3u16.to_le_bytes())), "ET_DYN");
    assert!(rejected(&patched(18, &3u16.to_le_bytes())), "i386");
    assert!(rejected(&patched(54, &32u16.to_le_bytes())), "phentsize");
    assert!(
        rejected(&patched(56, &0u16.to_le_bytes())),
        "no program headers"
    );
    assert!(
        rejected(&patched(56, &1000u16.to_le_bytes())),
        "too many headers"
    );
    assert!(
        rejected(&patched(32, &u64::MAX.to_le_bytes())),
        "phoff wraps"
    );
    // Bad segments, one at a time.
    let with = |change: &dyn Fn(&mut Vec<Phdr>)| {
        let mut changed = headers.clone();
        change(&mut changed);
        write_elf(0x401010, &changed, 0x3080)
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("W+X", with(&|h| h[3].flags = R | W | X)),
        ("filesz > memsz", with(&|h| h[3].file_size = 0x4000)),
        ("past end of file", with(&|h| h[2].file_size = 0x10_0000)),
        ("misaligned offset", with(&|h| h[2].offset = 0x1004)),
        ("overlap", with(&|h| h[3].vaddr = 0x402800)),
        ("above user top", with(&|h| h[3].vaddr = USER_TOP - 0x1000)),
        (
            "vaddr wraps",
            with(&|h| {
                h[3].vaddr = u64::MAX - 0xfff;
                h[3].offset = 0x3000;
            }),
        ),
        (
            "page zero",
            with(&|h| {
                h[1].vaddr = 0;
                h[0].vaddr = 0x40;
            }),
        ),
        (
            "PT_INTERP",
            with(&|h| {
                h.push(Phdr {
                    kind: 3,
                    flags: R,
                    offset: 0,
                    vaddr: 0,
                    file_size: 0,
                    mem_size: 0,
                })
            }),
        ),
        (
            "PT_DYNAMIC",
            with(&|h| {
                h.push(Phdr {
                    kind: 2,
                    flags: R,
                    offset: 0,
                    vaddr: 0,
                    file_size: 0,
                    mem_size: 0,
                })
            }),
        ),
        ("no PT_LOAD", with(&|h| h.retain(|header| header.kind != 1))),
    ];
    for (name, file) in cases {
        assert!(rejected(&file), "{name}");
    }
    let mut entry_in_data = file.clone();
    entry_in_data[24..32].copy_from_slice(&0x403010u64.to_le_bytes());
    assert!(
        rejected(&entry_in_data),
        "entry outside an executable segment"
    );
    // Below a lower load limit (the stack guard), the same file fails.
    assert!(elf::plan(&file, 0x405000).is_err());
}

/// Read the stack back as a program's `_start` would.
fn parse(
    bytes: &[u8],
    top: u64,
    stack_pointer: u64,
) -> (Vec<String>, Vec<String>, Vec<(u64, u64)>) {
    let base = top - bytes.len() as u64;
    let word = |address: u64| {
        let offset = (address - base) as usize;
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    };
    let string = |address: u64| {
        let offset = (address - base) as usize;
        let end = bytes[offset..].iter().position(|&byte| byte == 0).unwrap();
        String::from_utf8(bytes[offset..offset + end].to_vec()).unwrap()
    };
    let argc = word(stack_pointer);
    let mut cursor = stack_pointer + 8;
    let argv: Vec<String> = (0..argc)
        .map(|index| string(word(cursor + index * 8)))
        .collect();
    cursor += argc * 8;
    assert_eq!(word(cursor), 0, "argv NULL");
    cursor += 8;
    let mut envp = Vec::new();
    while word(cursor) != 0 {
        envp.push(string(word(cursor)));
        cursor += 8;
    }
    cursor += 8;
    let mut auxv = Vec::new();
    loop {
        let (kind, value) = (word(cursor), word(cursor + 8));
        auxv.push((kind, value));
        cursor += 16;
        if kind == AT_NULL {
            break;
        }
    }
    (argv, envp, auxv)
}

#[test]
fn initial_stack_follows_the_sysv_layout() {
    let top = 0x7fff_ffff_e000;
    let random = *b"0123456789abcdef";
    let built = stack::build(
        top,
        &["hello", "x", ""],
        &["A=B", "PATH=/bin"],
        &[(AT_PAGESZ, 4096), (AT_ENTRY, 0x401010)],
        random,
        8192,
    )
    .unwrap();
    assert_eq!(built.stack_pointer % 16, 0, "rsp aligned at _start");
    assert_eq!(top - built.stack_pointer, built.bytes.len() as u64);
    let (argv, envp, auxv) = parse(&built.bytes, top, built.stack_pointer);
    assert_eq!(argv, ["hello", "x", ""]);
    assert_eq!(envp, ["A=B", "PATH=/bin"]);
    assert_eq!(auxv[0], (AT_PAGESZ, 4096));
    assert_eq!(auxv[1], (AT_ENTRY, 0x401010));
    assert_eq!(auxv.last(), Some(&(AT_NULL, 0)));
    let random_address = auxv.iter().find(|(kind, _)| *kind == AT_RANDOM).unwrap().1;
    let offset = (random_address - (top - built.bytes.len() as u64)) as usize;
    assert_eq!(&built.bytes[offset..offset + 16], &random);
    // Everything lies below the top.
    assert!(random_address + 16 <= top);
}

#[test]
fn initial_stack_edge_cases() {
    let top = 0x7fff_ffff_e000;
    let empty = stack::build(top, &[], &[], &[], [0; 16], 4096).unwrap();
    assert_eq!(empty.stack_pointer % 16, 0);
    let (argv, envp, auxv) = parse(&empty.bytes, top, empty.stack_pointer);
    assert!(argv.is_empty() && envp.is_empty());
    assert_eq!(auxv.len(), 2, "AT_RANDOM and AT_NULL");
    // Too big for the limit.
    let long = "y".repeat(5000);
    assert_eq!(
        stack::build(top, &[&long], &[], &[], [0; 16], 4096).err(),
        Some(Errno::E2BIG)
    );
    // Alignment holds for every argument length.
    for length in 0..40 {
        let argument = "a".repeat(length);
        let built = stack::build(top, &[&argument], &[], &[], [0; 16], 4096).unwrap();
        assert_eq!(built.stack_pointer % 16, 0, "length {length}");
        assert_eq!(parse(&built.bytes, top, built.stack_pointer).0, [argument]);
    }
    // A top too low to hold anything is E2BIG, not an underflow.
    assert_eq!(
        stack::build(8, &["abc"], &[], &[], [0; 16], 4096).err(),
        Some(Errno::E2BIG)
    );
}
