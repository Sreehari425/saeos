pub mod frame;
pub mod heap;
pub mod mapping_model;
pub mod multiboot;
pub mod paging;
pub mod selftest;

pub use frame::{
    PhysAddr, PhysFrame, VirtAddr, alloc_frame, alloc_frame_at_or_above, free_frame, reserve_range,
};
pub use heap::{HEAP_SIZE, heap_phys_start, heap_start, heap_virt_start};
pub use multiboot::MultibootInfo;

pub fn init_boot_memory(boot_info: &crate::boot::BootInfo) {
    frame::init(boot_info.memory_map);
    // These regions are never handed to a future mapper or heap.
    frame::reserve_range(
        PhysAddr(boot_info.kernel_physical_start),
        PhysAddr(boot_info.kernel_physical_end),
    );
    let heap_phys = heap_phys_start();
    frame::reserve_range(
        PhysAddr(heap_phys),
        PhysAddr(heap_phys + HEAP_SIZE as u64),
    );
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

pub fn init(boot_info: &crate::boot::BootInfo) {
    init_boot_memory(boot_info);
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
        boot_info.boot_mode == crate::boot::boot_info::BootMode::Bios,
    );
    paging::reserve_page_tables();
    if let Err(error) = mapping_model::check_memory_map(&boot_info.memory_map, true, true) {
        crate::serial_println!("Memory: mapping policy self-check failed: {:?}", error);
    }
    if let Some(rsdp) = boot_info.rsdp_addr {
        frame::reserve_range(PhysAddr(rsdp & !4095), PhysAddr((rsdp & !4095) + 4096));
    }
    match boot_info.display {
        crate::boot::DisplayMode::VgaText { buffer_addr } => frame::reserve_range(
            PhysAddr(buffer_addr as u64),
            PhysAddr(buffer_addr as u64 + 4000),
        ),
        crate::boot::DisplayMode::GopFramebuffer(info) => frame::reserve_range(
            PhysAddr(info.base_addr),
            PhysAddr(info.base_addr.saturating_add(info.size as u64)),
        ),
    };
    activate_higher_half();
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

fn memory_self_check(boot_info: &crate::boot::BootInfo) {
    let Some(low_frame) = frame::alloc_frame() else {
        crate::serial_println!("Memory: unable to allocate low diagnostic frame.");
        return;
    };
    let low_virtual = if paging::prefer_identity_mmio() {
        paging::identity(PhysAddr(low_frame.0))
    } else {
        paging::phys_to_virt(PhysAddr(low_frame.0))
    };
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
        let mut address_space = match paging::AddressSpace::new() {
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
    // BIOS keeps the bootloader's writable identity map for VGA/MMIO holes.
    // Do not split the bootstrap huge page just to remap 0xb8000.
    if paging::prefer_identity_mmio() {
        return;
    }
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
    let mut physical = start;
    while physical < end {
        if paging::map_page(
            paging::phys_to_virt(PhysAddr(physical)),
            PhysAddr(physical),
            paging::PageFlags::MMIO,
        )
        .is_err()
        {
            crate::serial_println!("Memory: unable to map framebuffer page at {:#x}.", physical);
            break;
        }
        physical += frame::PAGE_SIZE;
    }
}
