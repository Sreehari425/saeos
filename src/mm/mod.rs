pub mod heap;
pub mod multiboot;

pub use heap::{heap_start, HEAP_SIZE};
pub use multiboot::MultibootInfo;

pub fn init(multiboot_info_addr: usize) {
    // 1. Initialize Kernel Heap Allocator (10 MiB)
    heap::init();

    // 2. Multiboot info inspection
    if multiboot_info_addr != 0 {
        let mb_info = unsafe { &*(multiboot_info_addr as *const MultibootInfo) };
        if mb_info.has_mem_info() {
            let total_mb = mb_info.total_memory_mb();
            crate::serial_println!("Multiboot: Detected {} MB available physical RAM.", total_mb);
        }
    }
}
