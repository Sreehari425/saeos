pub mod frame;
pub mod heap;
pub mod multiboot;
pub mod paging;

pub use frame::{PhysAddr, PhysFrame, VirtAddr, alloc_frame, free_frame, reserve_range};
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
    init_heap();
    crate::serial_println!(
        "Memory: {} MiB usable; higher-half page tables active at {:#x}.",
        boot_info.total_memory_mb(),
        paging::KERNEL_VIRT_BASE
    );
}
