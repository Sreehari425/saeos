pub mod frame;
pub mod heap;
pub mod multiboot;
pub mod paging;

pub use frame::{
    PhysAddr, PhysFrame, VirtAddr, alloc_frame, alloc_frame_at_or_above, free_frame, reserve_range,
};
pub use heap::{HEAP_SIZE, heap_start};
pub use multiboot::MultibootInfo;

pub fn init_boot_memory(boot_info: &crate::boot::BootInfo) {
    frame::init(boot_info.memory_map);
    // These regions are never handed to a future mapper or heap.
    frame::reserve_range(
        PhysAddr(boot_info.kernel_physical_start),
        PhysAddr(boot_info.kernel_physical_end),
    );
}

pub fn init_paging() {
    paging::init();
    paging::reserve_page_tables();
}

pub fn activate_higher_half() {
    paging::activate();
}

pub fn init_heap() {
    frame::reserve_range(
        PhysAddr(heap_start() as u64),
        PhysAddr(heap_start() as u64 + HEAP_SIZE as u64),
    );
    heap::init();
}

pub fn init(boot_info: &crate::boot::BootInfo) {
    init_boot_memory(boot_info);
    init_paging();
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
    }
    activate_higher_half();
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
