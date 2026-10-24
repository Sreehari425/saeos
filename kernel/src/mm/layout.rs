//! Kernel virtual address layout.
//!
//! ```text
//! 0x0000_0000_0000_0000 .. 0x0000_7fff_ffff_ffff  user space (PML4 0..=255), per process;
//!                                                  empty in the kernel root after boot
//! 0xffff_8000_0000_0000 .. 0xffff_bfff_ffff_ffff  direct map of physical memory, 64 TiB
//!                                                  (PML4 256..=383)
//! 0xffff_c000_0000_0000 .. +1 GiB                  page-backed heap        (PML4 384)
//! 0xffff_c000_4000_0000 .. +512 MiB                kernel stack arena      (PML4 384)
//! 0xffff_ffff_8000_0000 .. 0xffff_ffff_ffff_ffff  kernel image, top 2 GiB (PML4 511)
//! ```
//!
//! BIOS links the image at `KERNEL_IMAGE_BASE` directly (see `linker.ld` and
//! `boot.asm`). On UEFI the firmware loads the PE image identity mapped; once
//! the final page tables are live it applies its own base relocations and
//! jumps here (`arch::x86_64::uefi::relocate`). Lock order: see `mm/mod.rs`.
//!
//! Every region owns whole PML4 slots or a fixed window inside one, so regions
//! cannot overlap regardless of how much RAM the direct map covers. All
//! upper-half PML4 entries are created at boot and then never change; process
//! roots copy them once and share every kernel table below them.

use core::sync::atomic::{AtomicU64, Ordering};

pub const DIRECT_MAP_BASE: u64 = 0xffff_8000_0000_0000;
pub const DIRECT_MAP_LIMIT: u64 = 1 << 46;

pub const KERNEL_VM_BASE: u64 = 0xffff_c000_0000_0000;
pub const PAGE_HEAP_BASE: u64 = KERNEL_VM_BASE;
pub const PAGE_HEAP_SIZE: u64 = 1 << 30;
pub const STACK_ARENA_BASE: u64 = KERNEL_VM_BASE + PAGE_HEAP_SIZE;
pub const STACK_ARENA_SIZE: u64 = 512 << 20;

pub const KERNEL_IMAGE_BASE: u64 = 0xffff_ffff_8000_0000;

/// First PML4 slot that belongs to the shared kernel half.
pub const KERNEL_PML4_FIRST: usize = 256;

/// `virtual - physical` for the running kernel image. Zero while the image
/// still runs from its identity mapping (UEFI before the higher-half switch).
static IMAGE_DELTA: AtomicU64 = AtomicU64::new(0);

/// Record where the image is mapped. BIOS sets this before any Rust code
/// takes a static's address; UEFI sets it immediately after relocation.
pub fn set_image_delta(delta: u64) {
    IMAGE_DELTA.store(delta, Ordering::Release);
}

pub fn image_delta() -> u64 {
    IMAGE_DELTA.load(Ordering::Acquire)
}

/// Physical address of an address inside the kernel image.
///
/// Any static whose address reaches CR3, a page-table entry, or the frame
/// allocator must go through this; `lgdt`, `lidt`, and the TSS keep virtual
/// addresses. Addresses below the image window are assumed to be identity
/// mapped (the UEFI image before relocation, or the BIOS low boot stub).
pub fn image_virt_to_phys(virtual_address: u64) -> u64 {
    if virtual_address >= KERNEL_IMAGE_BASE {
        virtual_address - image_delta()
    } else {
        virtual_address
    }
}

/// High-half virtual address of a physical address inside the kernel image.
pub fn image_phys_to_virt(physical: u64) -> u64 {
    physical + image_delta()
}

/// Physical address of a static inside the kernel image.
pub fn static_phys<T>(pointer: *const T) -> u64 {
    image_virt_to_phys(pointer as u64)
}

const _: () = {
    assert!(DIRECT_MAP_BASE + DIRECT_MAP_LIMIT <= KERNEL_VM_BASE);
    assert!(STACK_ARENA_BASE + STACK_ARENA_SIZE <= KERNEL_VM_BASE + (1 << 39));
    assert!(KERNEL_VM_BASE + (1 << 39) <= KERNEL_IMAGE_BASE);
};
