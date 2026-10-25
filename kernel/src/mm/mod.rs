//! Memory management.
//!
//! Address layout: see [`layout`]. Physical frames: [`frame`] (bitmap
//! ownership, [`buddy`] free lists, reference counts). Page tables:
//! [`paging`]. Kernel heap: [`heap`] (static small-object heap plus a
//! page-backed heap). Guarded kernel stacks: [`kvm`]. Device memory:
//! [`dma`].
//!
//! # Lock order
//!
//! Locks are taken in this order and never the reverse (modelled and tested
//! in `tools/mm-model/src/lock_model.rs`):
//!
//! `SCHEDULER`/`PROCESSES` -> `AddressSpace.state` -> small-object heap ->
//! `KERNEL_MAP_LOCK` -> frame allocator
//!
//! - The scheduler and process locks may be held while a `Vec` grows, and a
//!   large allocation maps page-heap pages, so memory-management code must
//!   never take them (fault handlers only `try_lock`).
//! - The page-heap and stack-arena bitmaps are leaves: they are released
//!   before any page is mapped.
//! - Reference counts are atomics; only the final drop takes the frame lock.
//! - The heap never allocates while holding `KERNEL_MAP_LOCK` or the frame
//!   lock, so `GlobalAlloc` cannot recurse into a held lock.
pub mod address_space;
pub mod allocator_model;
pub mod buddy;
pub mod dma;
pub mod frame;
pub mod heap;
pub mod holds;
pub mod kvm;
pub mod layout;
pub mod mapping_model;
pub mod multiboot;
pub mod paging;
pub mod selftest;
pub mod uaccess;
pub mod user_memory;
pub mod vma;

pub use frame::{
    PhysAddr, PhysFrame, VirtAddr, alloc_frame, alloc_frame_at_or_above, free_frame, reserve_range,
};
pub use heap::{HEAP_SIZE, heap_phys_start, heap_start, heap_virt_start};
pub use multiboot::MultibootInfo;

pub fn init_boot_memory(boot_info: &crate::boot::BootInfo) -> bool {
    if let Err(error) = frame::init(boot_info.memory_map) {
        crate::serial_println!(
            "Memory: allocator metadata initialization failed: {:?}.",
            error
        );
        return false;
    }
    // The bootloader put the initramfs in memory the map calls usable; keep
    // it until fs::init has unpacked it.
    let mut holds = INITRAMFS_HOLDS.lock();
    for (slot, range) in holds.iter_mut().zip(&boot_info.initramfs) {
        *slot = range.and_then(|range| frame::hold_range(range.start, range.end));
    }
    drop(holds);
    // UEFI reports the firmware stack we are running on as usable
    // boot-services memory; keep it until init_late runs on its own stack.
    if boot_info.boot_mode == crate::boot::boot_info::BootMode::Uefi {
        frame::hold_boot_stack(crate::arch::x86_64::cpu::stack_pointer());
    }
    // These regions are never handed to a future mapper or heap.
    frame::reserve_range(
        PhysAddr(boot_info.kernel_physical_start),
        PhysAddr(boot_info.kernel_physical_end),
    );
    let heap_phys = heap_phys_start();
    frame::reserve_range(PhysAddr(heap_phys), PhysAddr(heap_phys + HEAP_SIZE as u64));
    true
}

static INITRAMFS_HOLDS: crate::sync::IrqSpinMutex<
    [Option<holds::HoldId>; crate::boot::boot_info::MAX_INITRAMFS],
> = crate::sync::IrqSpinMutex::new([None; crate::boot::boot_info::MAX_INITRAMFS]);

/// Give the initramfs memory back to the allocator once it is unpacked.
pub fn release_initramfs() {
    for id in INITRAMFS_HOLDS.lock().iter_mut().filter_map(Option::take) {
        frame::release_hold(id);
    }
}

pub fn init_paging() {
    paging::init(0, 0);
    paging::reserve_page_tables();
}

pub fn activate_higher_half() {
    paging::activate();
}

pub fn init_heap() {
    heap::init();
    if !heap::validate() {
        crate::serial_println!(
            "Memory: heap allocator integrity failed after initialization: {}.",
            heap::validate_reason()
        );
    }
}

/// Where the final tables place the kernel image in the top-2-GiB window.
/// BIOS links the image there, so its delta is already active.
fn planned_image_delta(boot_info: &crate::boot::BootInfo) -> u64 {
    match boot_info.boot_mode {
        crate::boot::boot_info::BootMode::Bios => layout::image_delta(),
        crate::boot::boot_info::BootMode::Uefi if boot_info.kernel_physical_start != 0 => {
            layout::KERNEL_IMAGE_BASE.wrapping_sub(boot_info.kernel_physical_start)
        }
        crate::boot::boot_info::BootMode::Uefi => 0,
    }
}

pub fn init(boot_info: &crate::boot::BootInfo) {
    init_early(boot_info);
    init_late(boot_info);
}

/// Build the final page tables and switch CR3 to them. On UEFI the caller
/// then moves the running image into the top-2-GiB window before
/// [`init_late`]; the image is mapped there from this point on.
pub fn init_early(boot_info: &crate::boot::BootInfo) {
    if !init_boot_memory(boot_info) {
        crate::arch::x86_64::cpu::cli();
        loop {
            crate::arch::x86_64::cpu::hlt();
        }
    }
    crate::serial_println!(
        "Memory: kernel range {:#x}..{:#x}; heap range {:#x}..{:#x}.",
        boot_info.kernel_physical_start,
        boot_info.kernel_physical_end,
        heap_phys_start(),
        heap_phys_start() + HEAP_SIZE as u64
    );
    for region in boot_info.memory_map.regions[..boot_info.memory_map.count].iter() {
        let heap_start_u64 = heap_phys_start();
        let heap_end_u64 = heap_start_u64 + HEAP_SIZE as u64;
        if region.end() >= heap_start_u64.saturating_sub(0x10000)
            && region.start <= heap_end_u64.saturating_add(0x10000)
        {
            crate::serial_println!(
                "Memory: nearby {:?} region {:#x}..{:#x}.",
                region.kind,
                region.start,
                region.end()
            );
        }
    }
    paging::init_with_mode(
        boot_info.kernel_physical_start,
        boot_info.kernel_physical_end,
        boot_info.kernel_sections,
        planned_image_delta(boot_info),
    );
    paging::reserve_page_tables();
    if let Err(error) = mapping_model::check_memory_map(&boot_info.memory_map, true, true) {
        crate::serial_println!("Memory: mapping policy self-check failed: {:?}", error);
    }
    if let Some(rsdp) = boot_info.rsdp_addr {
        frame::reserve_range(PhysAddr(rsdp & !4095), PhysAddr((rsdp & !4095) + 4096));
    }
    match boot_info.display {
        crate::boot::DisplayMode::VgaText { buffer_addr } => {
            let start = (buffer_addr as u64) & !(frame::PAGE_SIZE - 1);
            let end = (buffer_addr as u64 + 4000).next_multiple_of(frame::PAGE_SIZE);
            frame::reserve_range(PhysAddr(start), PhysAddr(end));
        }
        crate::boot::DisplayMode::GopFramebuffer(info) => {
            let start = info.base_addr & !(frame::PAGE_SIZE - 1);
            let end = info
                .base_addr
                .saturating_add(info.size as u64)
                .next_multiple_of(frame::PAGE_SIZE);
            frame::reserve_range(PhysAddr(start), PhysAddr(end));
        }
    };
    activate_higher_half();
}

/// Finish memory-manager bring-up from the final image address.
pub fn init_late(boot_info: &crate::boot::BootInfo) {
    // Nothing runs on the firmware stack any more.
    frame::release_boot_stack();
    #[cfg(feature = "paging-debug")]
    paging::inspect_final_kernel_mappings(boot_info.kernel_sections);
    if let Some(rsdp) = boot_info.rsdp_addr {
        let start = rsdp & !(frame::PAGE_SIZE - 1);
        let end = rsdp.saturating_add(36).next_multiple_of(frame::PAGE_SIZE);
        let mut physical = start;
        while physical < end {
            let _ = paging::map_page(
                paging::phys_to_virt(PhysAddr(physical)),
                PhysAddr(physical),
                paging::PageFlags::READ_ONLY,
            );
            physical += frame::PAGE_SIZE;
        }
    }
    map_framebuffer(boot_info.display);
    memory_self_check(boot_info);
    if let Some(frame) = frame::alloc_frame_at_or_above(PhysAddr(4 * 1024 * 1024 * 1024)) {
        let address = paging::phys_to_virt(PhysAddr(frame.0)).0 as *mut u64;
        unsafe {
            address.write_volatile(0x5341_454f_5348_4947);
            if address.read_volatile() == 0x5341_454f_5348_4947 {
                crate::serial_println!(
                    "Memory: verified direct access to frame {:#x} above 4 GiB.",
                    frame.0
                );
            }
        }
        frame::free_frame(frame);
    }
    init_heap();
    #[cfg(feature = "mm-stress")]
    {
        let report = selftest::run();
        crate::serial_println!(
            "Memory: boot MM stress report: {} passed, {} failed, {} skipped.",
            report.passed,
            report.failed,
            report.skipped
        );
    }
    #[cfg(feature = "mm-allocator-stats")]
    {
        let stats = frame::stats();
        crate::serial_println!(
            "Memory: frames total={} usable={} allocated={} reserved={} free={} largest={}",
            stats.total_frames,
            stats.usable_frames,
            stats.allocated_frames,
            stats.reserved_frames,
            stats.free_frames,
            stats.largest_contiguous_block
        );
    }
    crate::serial_println!(
        "Memory: {} MiB usable, highest physical {:#x}; direct map active at {:#x}.",
        boot_info.total_memory_mb(),
        boot_info.memory_map.regions[..boot_info.memory_map.count]
            .iter()
            .map(|region| region.end())
            .max()
            .unwrap_or(0),
        paging::KERNEL_VIRT_BASE
    );
}

/// Retire boot-only mappings once nothing references them.
pub fn finish_boot() {
    paging::drop_lower_half();
    crate::serial_println!("Memory: boot identity map removed; lower half reserved for processes.");
    heap::enable_page_heap();
    crate::serial_println!(
        "Memory: page heap enabled at {:#x} for allocations above {} bytes.",
        layout::PAGE_HEAP_BASE,
        4096
    );
}

fn memory_self_check(boot_info: &crate::boot::BootInfo) {
    let Some(low_frame) = frame::alloc_frame() else {
        crate::serial_println!("Memory: unable to allocate low diagnostic frame.");
        return;
    };
    let low_virtual = paging::phys_to_virt(PhysAddr(low_frame.0));
    let low_address = low_virtual.0 as *mut u64;
    let marker = 0x0053_4145_4f4c_4f57_u64;
    unsafe {
        low_address.write_volatile(marker);
        if low_address.read_volatile() == marker
            && paging::virt_to_phys(low_virtual) == Some(PhysAddr(low_frame.0))
        {
            crate::serial_println!("Memory: low frame access and address round-trip verified.");
        } else {
            crate::serial_println!("Memory: low frame diagnostic failed.");
        }
    }
    frame::free_frame(low_frame);

    let holes_ok = boot_info.memory_map.regions[..boot_info.memory_map.count]
        .iter()
        .filter(|region| region.kind != crate::boot::PhysicalMemoryKind::Usable)
        .all(|region| !mapping_model::usable_region_contains(&boot_info.memory_map, region.start));
    if holes_ok {
        crate::serial_println!("Memory: reserved holes excluded from usable direct-map coverage.");
    } else {
        crate::serial_println!("Memory: reserved-hole coverage check failed.");
    }

    if let Some(frame) = frame::alloc_frame() {
        let address_space = match paging::AddressSpace::new() {
            Ok(space) => space,
            Err(error) => {
                crate::serial_println!("Memory: address-space self-check unavailable: {:?}", error);
                frame::free_frame(frame);
                return;
            }
        };
        let result = address_space
            .map_user_page(
                VirtAddr(0x4000_0000),
                frame,
                paging::UserPageFlags::USER_READ,
            )
            .and_then(|_| address_space.unmap_user_page(VirtAddr(0x4000_0000)));
        match result {
            Ok(unmapped) if unmapped == frame => {
                crate::serial_println!(
                    "Memory: kernel address-space user mapping self-check passed (root {:#x}).",
                    address_space.root_frame().0
                );
                frame::free_frame(unmapped);
            }
            Ok(unmapped) => {
                crate::serial_println!(
                    "Memory: address-space returned unexpected frame {:#x}.",
                    unmapped.0
                );
                frame::free_frame(unmapped);
            }
            Err(error) => {
                crate::serial_println!("Memory: address-space self-check failed: {:?}", error);
                frame::free_frame(frame);
            }
        }
    }
}

fn map_framebuffer(display: crate::boot::DisplayMode) {
    let Some((base, size)) = (match display {
        crate::boot::DisplayMode::VgaText { buffer_addr } => Some((buffer_addr as u64, 4000)),
        crate::boot::DisplayMode::GopFramebuffer(info) => Some((info.base_addr, info.size as u64)),
    }) else {
        return;
    };
    let start = base & !(frame::PAGE_SIZE - 1);
    let Some(end) = base
        .checked_add(size)
        .map(|value| value.next_multiple_of(frame::PAGE_SIZE))
    else {
        return;
    };
    // GOP framebuffers are write-combining; legacy VGA text memory stays
    // uncached. Remap: the range may already sit, uncached, inside a
    // direct-mapped MMIO region.
    let flags = match display {
        crate::boot::DisplayMode::VgaText { .. } => paging::PageFlags::MMIO,
        crate::boot::DisplayMode::GopFramebuffer(_) => paging::PageFlags::FRAMEBUFFER,
    };
    let mut physical = start;
    while physical < end {
        let virtual_address = paging::phys_to_virt(PhysAddr(physical));
        if paging::remap_page(virtual_address, PhysAddr(physical), flags).is_err() {
            crate::serial_println!("Memory: unable to map framebuffer page at {:#x}.", physical);
            break;
        }
        physical += frame::PAGE_SIZE;
    }
}
