//! Kernel virtual memory: the guarded kernel-stack arena.
//!
//! The arena is a fixed window (`layout::STACK_ARENA_BASE`) split into equal
//! slots. Each slot is an unmapped guard region followed by the stack pages:
//!
//! ```text
//! slot_base                     slot_base + GUARD_SIZE         slot_base + SLOT_SIZE
//! |  guard (never mapped, 64K)  |  stack (64K, grows down)  <- top |
//! ```
//!
//! A stack overflow therefore faults on the guard instead of silently
//! corrupting a neighbouring allocation, and `classify_fault` can name the
//! slot from the fault address alone. The guard is larger than one page so a
//! function with a big frame cannot step over it.

use crate::mm::frame::{self, PAGE_SIZE, PhysAddr, VirtAddr};
use crate::mm::{layout, paging};
use crate::sync::IrqSpinMutex;
use crate::task::{DEFAULT_KERNEL_STACK_SIZE, KernelStack};

pub const STACK_SIZE: u64 = DEFAULT_KERNEL_STACK_SIZE as u64;
pub const GUARD_SIZE: u64 = 64 * 1024;
pub const SLOT_SIZE: u64 = STACK_SIZE + GUARD_SIZE;
pub const MAX_STACKS: usize = (layout::STACK_ARENA_SIZE / SLOT_SIZE) as usize;
const STACK_PAGES: u64 = STACK_SIZE / PAGE_SIZE;

const _: () = assert!(SLOT_SIZE.is_power_of_two() && MAX_STACKS.is_multiple_of(64));

/// One bit per slot; set means the slot is in use.
static SLOTS: IrqSpinMutex<[u64; MAX_STACKS / 64]> = IrqSpinMutex::new([0; MAX_STACKS / 64]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KvmError {
    NoSlots,
    OutOfFrames,
    MapFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// The address lies in the guard region below `stack`.
    StackGuard { slot: usize, stack: KernelStack },
}

fn slot_base(slot: usize) -> u64 {
    layout::STACK_ARENA_BASE + slot as u64 * SLOT_SIZE
}

fn stack_bounds(slot: usize) -> KernelStack {
    let base = slot_base(slot) + GUARD_SIZE;
    KernelStack {
        base,
        top: base + STACK_SIZE,
    }
}

/// A mapped kernel stack in the guarded arena. Dropping it unmaps the pages,
/// frees their frames, and returns the slot.
#[derive(Debug)]
pub struct KernelStackAllocation {
    slot: usize,
}

impl KernelStackAllocation {
    pub fn new() -> Result<Self, KvmError> {
        let slot = take_slot().ok_or(KvmError::NoSlots)?;
        let stack = stack_bounds(slot);
        for page in 0..STACK_PAGES {
            let address = VirtAddr(stack.base + page * PAGE_SIZE);
            let Some(frame) = frame::alloc_frame_zeroed() else {
                unmap_pages(slot, page);
                release_slot(slot);
                return Err(KvmError::OutOfFrames);
            };
            if paging::map_page(address, PhysAddr(frame.0), paging::PageFlags::WRITABLE).is_err() {
                frame::free_frame(frame);
                unmap_pages(slot, page);
                release_slot(slot);
                return Err(KvmError::MapFailed);
            }
        }
        Ok(Self { slot })
    }

    pub fn bounds(&self) -> KernelStack {
        stack_bounds(self.slot)
    }
}

impl Drop for KernelStackAllocation {
    fn drop(&mut self) {
        unmap_pages(self.slot, STACK_PAGES);
        release_slot(self.slot);
    }
}

/// Unmap and free the first `pages` stack pages of `slot`.
// SMP: a stack can only be freed once no CPU runs on it; unmap_page's local
// invlpg is then sufficient.
fn unmap_pages(slot: usize, pages: u64) {
    let stack = stack_bounds(slot);
    for page in 0..pages {
        let address = VirtAddr(stack.base + page * PAGE_SIZE);
        match paging::unmap_page(address) {
            Ok(physical) => frame::free_frame(frame::PhysFrame(physical.0)),
            Err(error) => panic!("kernel stack page {:#x} not mapped: {}", address.0, error),
        }
    }
}

fn take_slot() -> Option<usize> {
    let mut slots = SLOTS.lock();
    for (word_index, word) in slots.iter_mut().enumerate() {
        if *word != u64::MAX {
            let bit = (!*word).trailing_zeros() as usize;
            *word |= 1 << bit;
            return Some(word_index * 64 + bit);
        }
    }
    None
}

fn release_slot(slot: usize) {
    let mut slots = SLOTS.lock();
    let mask = 1u64 << (slot % 64);
    assert!(
        slots[slot / 64] & mask != 0,
        "kernel stack slot {slot} released twice"
    );
    slots[slot / 64] &= !mask;
}

/// Identify faults on arena guard regions. Pure arithmetic and lock free, so
/// it is safe from the double-fault handler.
pub fn classify_fault(address: u64) -> Option<FaultKind> {
    let offset = address.checked_sub(layout::STACK_ARENA_BASE)?;
    let slot = usize::try_from(offset / SLOT_SIZE).ok()?;
    (slot < MAX_STACKS && offset % SLOT_SIZE < GUARD_SIZE).then(|| FaultKind::StackGuard {
        slot,
        stack: stack_bounds(slot),
    })
}

/// Number of stacks currently allocated.
pub fn stacks_in_use() -> usize {
    SLOTS
        .lock()
        .iter()
        .map(|word| word.count_ones() as usize)
        .sum()
}
