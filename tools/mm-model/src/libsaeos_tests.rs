//! libsaeos's pure parts: the allocator core, the line reader, and the
//! initial-stack parser (checked against the kernel's stack builder).

use crate::user_heap::{Heap, LARGE, Source, class};
use crate::user_lines::LineReader;
use crate::user_startinfo::{AT_ENTRY, AT_PAGESZ, StartInfo, c_string, entry};
use std::alloc::Layout;
use std::collections::HashMap;

/// A fake heap: one big region grown like `brk`, and separate mappings
/// tracked so leaks show.
struct FakeSource {
    region: *mut u8,
    size: usize,
    used: usize,
    mappings: HashMap<usize, usize>,
}

const REGION: usize = 64 << 20;

impl FakeSource {
    fn new() -> Self {
        let layout = Layout::from_size_align(REGION, 4096).unwrap();
        Self {
            region: unsafe { std::alloc::alloc(layout) },
            size: REGION,
            used: 0,
            mappings: HashMap::new(),
        }
    }
}

impl Drop for FakeSource {
    fn drop(&mut self) {
        unsafe { std::alloc::dealloc(self.region, Layout::from_size_align(REGION, 4096).unwrap()) };
        for (&address, &bytes) in &self.mappings {
            unsafe {
                std::alloc::dealloc(
                    address as *mut u8,
                    Layout::from_size_align(bytes, 4096).unwrap(),
                )
            };
        }
    }
}

impl Source for FakeSource {
    fn grow(&mut self, bytes: usize) -> Option<*mut u8> {
        if self.used + bytes > self.size {
            return None;
        }
        let start = unsafe { self.region.add(self.used) };
        self.used += bytes;
        Some(start)
    }

    fn map(&mut self, bytes: usize) -> Option<*mut u8> {
        assert_eq!(bytes % 4096, 0);
        let block = unsafe { std::alloc::alloc(Layout::from_size_align(bytes, 4096).unwrap()) };
        self.mappings.insert(block as usize, bytes);
        Some(block)
    }

    fn unmap(&mut self, address: *mut u8, bytes: usize) {
        assert_eq!(
            self.mappings.remove(&(address as usize)),
            Some(bytes),
            "unmap matches map"
        );
        unsafe { std::alloc::dealloc(address, Layout::from_size_align(bytes, 4096).unwrap()) };
    }
}

fn layout(size: usize, align: usize) -> Layout {
    Layout::from_size_align(size, align).unwrap()
}

#[test]
fn size_classes_cover_size_and_alignment() {
    assert_eq!(class(layout(1, 1)), Some(0), "16-byte minimum");
    assert_eq!(class(layout(16, 8)), Some(0));
    assert_eq!(class(layout(17, 8)), Some(1));
    assert_eq!(class(layout(8, 256)), Some(4), "alignment picks the class");
    assert_eq!(class(layout(LARGE, 8)), Some(12));
    assert_eq!(class(layout(LARGE + 1, 8)), None, "own mapping");
}

#[test]
fn blocks_are_aligned_reused_and_large_ones_unmapped() {
    let mut heap = Heap::new(FakeSource::new());
    let small = layout(24, 8);
    let first = heap.alloc(small);
    let second = heap.alloc(small);
    assert!(!first.is_null() && first != second);
    assert_eq!(
        first as usize % 32,
        0,
        "a 32-byte class block is 32-aligned"
    );
    unsafe { heap.dealloc(first, small) };
    assert_eq!(heap.alloc(small), first, "freed blocks are reused first");
    let aligned = heap.alloc(layout(10, 4096));
    assert_eq!(aligned as usize % 4096, 0);
    let big = layout(200_000, 16);
    let mapped = heap.alloc(big);
    assert!(!mapped.is_null());
    assert_eq!(heap.source().mappings.len(), 1);
    unsafe { heap.dealloc(mapped, big) };
    assert!(
        heap.source().mappings.is_empty(),
        "large blocks are unmapped on free"
    );
    assert!(
        heap.alloc(layout(LARGE * 2, 8192)).is_null(),
        "alignment beyond a page is refused"
    );
    // realloc within a class stays put; across classes it moves and copies.
    let block = heap.alloc(layout(40, 8));
    unsafe {
        block.write_bytes(0x5a, 40);
        assert_eq!(heap.realloc(block, layout(40, 8), 60), block);
        let moved = heap.realloc(block, layout(60, 8), 1000);
        assert_ne!(moved, block);
        assert!((0..40).all(|index| *moved.add(index) == 0x5a));
    }
}

#[test]
fn random_allocations_never_overlap_or_corrupt() {
    let rounds = if cfg!(miri) { 120 } else { 20_000 };
    // Under Miri every byte written is slow: keep "large" just past LARGE.
    let large_limit = if cfg!(miri) { 70_000 } else { 300_000 };
    let mut heap = Heap::new(FakeSource::new());
    let mut live: Vec<(*mut u8, Layout, u8)> = Vec::new();
    let mut state = 0x5ae0_u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let check = |block: *mut u8, layout: Layout, fill: u8| {
        let bytes = unsafe { std::slice::from_raw_parts(block, layout.size()) };
        assert!(bytes.iter().all(|&byte| byte == fill), "contents intact");
    };
    for _ in 0..rounds {
        if live.is_empty() || next() % 3 != 0 {
            let size = match next() % 10 {
                0 => (next() % large_limit) as usize + 1,
                _ => (next() % 2000) as usize + 1,
            };
            let align = 1 << (next() % 7);
            let layout = layout(size, align);
            let block = heap.alloc(layout);
            assert!(!block.is_null());
            assert_eq!(block as usize % align, 0);
            let fill = next() as u8;
            unsafe { block.write_bytes(fill, size) };
            live.push((block, layout, fill));
        } else {
            let (block, layout, fill) = live.swap_remove((next() as usize) % live.len());
            check(block, layout, fill);
            unsafe { heap.dealloc(block, layout) };
        }
    }
    for (block, layout, fill) in live.drain(..) {
        check(block, layout, fill);
        unsafe { heap.dealloc(block, layout) };
    }
    assert!(
        heap.source().mappings.is_empty(),
        "every large block was unmapped"
    );
}

#[test]
fn line_reader_splits_buffers_into_lines() {
    // Data arrives in awkward pieces, like a pipe or a file read in chunks.
    let chunks: Vec<&[u8]> = vec![b"first li", b"ne\nsecond\nthi", b"rd", b"", b"never"];
    // After the last chunk, every read is end of file.
    let mut source = chunks.into_iter();
    let mut read = |buffer: &mut [u8]| -> Result<usize, ()> {
        let chunk = source.next().unwrap_or(b"");
        buffer[..chunk.len()].copy_from_slice(chunk);
        Ok(chunk.len())
    };
    let mut reader = LineReader::new();
    let mut lines = Vec::new();
    loop {
        let mut line = Vec::new();
        match reader.read_line(&mut line, &mut read).unwrap() {
            0 => break,
            count => {
                assert_eq!(count, line.len());
                lines.push(line);
            }
        }
    }
    // "third" ends at the first end-of-file read. Reading on after end of
    // file can return more, as a terminal does after Ctrl-D.
    assert_eq!(
        lines,
        [
            b"first line\n".to_vec(),
            b"second\n".to_vec(),
            b"third".to_vec(),
            b"never".to_vec()
        ]
    );
    // Errors pass through.
    let mut failing = LineReader::new();
    assert_eq!(
        failing.read_line(&mut Vec::new(), |_| Err::<usize, i32>(5)),
        Err(5)
    );
}

#[test]
fn start_info_reads_what_the_kernel_builds() {
    // Build a real initial stack, at a real host address, with the kernel's
    // builder, then parse it the way a program's _start does.
    let mut memory = vec![0u64; 1024];
    // The stack holds plain addresses into this buffer, as the kernel's
    // does; expose its provenance so they can be followed.
    let top = memory.as_mut_ptr().expose_provenance() as u64 + 1024 * 8;
    let built = crate::exec_stack::build(
        top,
        &["prog", "-v"],
        &["HOME=/", "PATH=/bin"],
        &[(AT_PAGESZ, 4096), (AT_ENTRY, 0x401000)],
        [9; 16],
        8192,
    )
    .unwrap();
    let start =
        std::ptr::with_exposed_provenance_mut::<u8>((top - built.bytes.len() as u64) as usize);
    unsafe {
        std::ptr::copy_nonoverlapping(built.bytes.as_ptr(), start, built.bytes.len());
        let stack = std::ptr::with_exposed_provenance::<u64>(built.stack_pointer as usize);
        let info = StartInfo::from_stack(stack);
        assert_eq!(info.argc, 2);
        assert_eq!(c_string(entry(info.argv, 0)), b"prog");
        assert_eq!(c_string(entry(info.argv, 1)), b"-v");
        assert!(entry(info.argv, 2).is_null());
        assert_eq!(c_string(entry(info.envp, 1)), b"PATH=/bin");
        assert_eq!(info.aux(AT_ENTRY), Some(0x401000));
        assert_eq!(info.aux(AT_PAGESZ), Some(4096));
        assert_eq!(info.aux(12345), None);
    }
    drop(memory);
}
