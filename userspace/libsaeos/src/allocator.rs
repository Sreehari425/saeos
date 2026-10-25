//! The global allocator: the size-class heap (`heap.rs`) growing with
//! `brk`, large blocks with `mmap`. User programs are single-threaded so
//! far; a spin lock keeps it correct if that changes.

use crate::heap::{Heap, Source};
use crate::raw;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SyscallSource;

impl Source for SyscallSource {
    fn grow(&mut self, bytes: usize) -> Option<*mut u8> {
        // Ask each time: the program may move the break itself. Growth that
        // is not contiguous with the last is fine (the heap starts afresh).
        let start = raw::brk(0);
        let wanted = start.checked_add(bytes as u64)?;
        // brk returns the new break, or the old one if it could not move.
        if raw::brk(wanted) != wanted {
            return None;
        }
        Some(start as *mut u8)
    }

    fn map(&mut self, bytes: usize) -> Option<*mut u8> {
        let address = raw::mmap_anonymous(0, bytes as u64, raw::PROT_READ | raw::PROT_WRITE, 0);
        (address > 0).then_some(address as *mut u8)
    }

    fn unmap(&mut self, address: *mut u8, bytes: usize) {
        raw::munmap(address as u64, bytes as u64);
    }
}

pub struct Allocator {
    locked: AtomicBool,
    heap: UnsafeCell<Heap<SyscallSource>>,
}

unsafe impl Sync for Allocator {}

impl Allocator {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            heap: UnsafeCell::new(Heap::new(SyscallSource)),
        }
    }

    fn with<R>(&self, operation: impl FnOnce(&mut Heap<SyscallSource>) -> R) -> R {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        let result = operation(unsafe { &mut *self.heap.get() });
        self.locked.store(false, Ordering::Release);
        result
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.with(|heap| heap.alloc(layout))
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        self.with(|heap| unsafe { heap.dealloc(block, layout) })
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        self.with(|heap| unsafe { heap.realloc(block, layout, new_size) })
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator::new();
