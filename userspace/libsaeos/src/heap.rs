//! The allocator core: size classes over a growable heap, plus direct
//! mappings for large blocks. Pure (it asks a [`Source`] for memory), so
//! the host model tests it on a fake heap; `alloc.rs` plugs in `brk` and
//! `mmap`.
//!
//! ```text
//! size/align  ≤ 64 KiB  -> class = next power of two (16 B .. 64 KiB)
//!                          free list per class; refilled by carving from
//!                          the heap, which grows by at least 64 KiB at a time
//!             > 64 KiB  -> its own mapping (page-rounded), unmapped on free
//! ```
//!
//! Blocks of a class are carved at addresses aligned to their size, so any
//! alignment up to the class size holds. A freed block keeps the free-list
//! link in its first word.

use core::alloc::Layout;
use core::ptr::null_mut;

pub const MIN_BLOCK: usize = 16;
pub const LARGE: usize = 64 * 1024;
const CLASSES: usize = 13; // 16 << 0 ..= 16 << 12 (64 KiB)
const PAGE: usize = 4096;
/// The heap grows by at least this much, so small refills are amortised.
const GROWTH: usize = 64 * 1024;

/// Where the heap gets memory.
pub trait Source {
    /// Extend the heap by `bytes`; returns the start of the new bytes.
    /// Growth is contiguous with the previous growth when possible
    /// (`brk`); a new region is fine too.
    fn grow(&mut self, bytes: usize) -> Option<*mut u8>;
    /// A separate page-aligned mapping of `bytes` (a multiple of 4 KiB).
    fn map(&mut self, bytes: usize) -> Option<*mut u8>;
    fn unmap(&mut self, address: *mut u8, bytes: usize);
}

pub struct Heap<S> {
    source: S,
    free: [*mut u8; CLASSES],
    /// The unused part of the heap: `bump..end`.
    bump: usize,
    end: usize,
}

// The heap is only used behind a lock (`alloc.rs`); its raw pointers are
// plain addresses in this process.
unsafe impl<S: Send> Send for Heap<S> {}

/// The size class for a layout, or `None` if it gets its own mapping.
pub fn class(layout: Layout) -> Option<usize> {
    let size = layout.size().max(layout.align()).max(MIN_BLOCK);
    let block = size.checked_next_power_of_two()?;
    (block <= LARGE).then(|| (block / MIN_BLOCK).trailing_zeros() as usize)
}

const fn block_size(class: usize) -> usize {
    MIN_BLOCK << class
}

fn large_size(layout: Layout) -> Option<usize> {
    layout.size().checked_next_multiple_of(PAGE)
}

impl<S: Source> Heap<S> {
    pub const fn new(source: S) -> Self {
        Self {
            source,
            free: [null_mut(); CLASSES],
            bump: 0,
            end: 0,
        }
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    /// Carve a fresh block of `class` from the heap, growing it if needed.
    fn carve(&mut self, class: usize) -> *mut u8 {
        let size = block_size(class);
        let mut start = self.bump.next_multiple_of(size);
        if start.checked_add(size).is_none_or(|end| end > self.end) {
            // Enough for this block whether or not the growth is contiguous.
            let wanted = (2 * size).max(GROWTH);
            let Some(new) = self.source.grow(wanted) else {
                return null_mut();
            };
            let new = new as usize;
            if new != self.end {
                // Not contiguous: abandon the old tail.
                self.bump = new;
            }
            self.end = new + wanted;
            start = self.bump.next_multiple_of(size);
        }
        self.bump = start + size;
        start as *mut u8
    }

    pub fn alloc(&mut self, layout: Layout) -> *mut u8 {
        match class(layout) {
            Some(class) => {
                let head = self.free[class];
                if head.is_null() {
                    return self.carve(class);
                }
                self.free[class] = unsafe { (head as *mut *mut u8).read() };
                head
            }
            None => {
                if layout.align() > PAGE {
                    return null_mut();
                }
                large_size(layout)
                    .and_then(|bytes| self.source.map(bytes))
                    .unwrap_or_default()
            }
        }
    }

    /// # Safety
    /// `block` must come from `alloc` on this heap with the same `layout`.
    pub unsafe fn dealloc(&mut self, block: *mut u8, layout: Layout) {
        match class(layout) {
            Some(class) => unsafe {
                (block as *mut *mut u8).write(self.free[class]);
                self.free[class] = block;
            },
            None => {
                if let Some(bytes) = large_size(layout) {
                    self.source.unmap(block, bytes);
                }
            }
        }
    }

    /// Resize, staying in place when the new size has the same class.
    ///
    /// # Safety
    /// As [`Self::dealloc`]; `new_size` must be non-zero.
    pub unsafe fn realloc(&mut self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
            return null_mut();
        };
        if class(layout).is_some() && class(layout) == class(new_layout) {
            return block;
        }
        let new = self.alloc(new_layout);
        if !new.is_null() {
            unsafe {
                core::ptr::copy_nonoverlapping(block, new, layout.size().min(new_size));
                self.dealloc(block, layout);
            }
        }
        new
    }
}
