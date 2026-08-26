//! Multiboot 1 Information and Memory Map structures.

pub const MULTIBOOT_MEMORY_AVAILABLE: u32 = 1;
pub const MULTIBOOT_MEMORY_RESERVED: u32 = 2;
use crate::boot::boot_info::{PhysicalMemoryKind, PhysicalMemoryMap, PhysicalMemoryRegion};

#[repr(C)]
pub struct MultibootInfo {
    pub flags: u32,
    pub mem_lower: u32, // Lower memory in KB (0..640KB)
    pub mem_upper: u32, // Upper memory in KB (1MB..)
    pub boot_device: u32,
    pub cmdline: u32,
    pub mods_count: u32,
    pub mods_addr: u32,
    pub syms: [u32; 4],
    pub mmap_length: u32,
    pub mmap_addr: u32,
}

#[repr(C, packed)]
pub struct MultibootMmapEntry {
    pub size: u32,
    pub base_addr: u64,
    pub length: u64,
    pub memory_type: u32,
}

impl MultibootInfo {
    pub fn has_mem_info(&self) -> bool {
        (self.flags & (1 << 0)) != 0
    }

    pub fn has_mmap_info(&self) -> bool {
        (self.flags & (1 << 6)) != 0
    }

    /// Returns the total detected physical RAM in megabytes.
    pub fn total_memory_mb(&self) -> u64 {
        if self.has_mem_info() {
            (self.mem_lower as u64 + self.mem_upper as u64).div_ceil(1024)
        } else {
            0
        }
    }

    pub fn usable_memory_map(&self) -> PhysicalMemoryMap {
        let mut map = PhysicalMemoryMap::empty();
        if !self.has_mmap_info() {
            return map;
        }
        let mut offset = 0u32;
        while offset < self.mmap_length {
            let entry = unsafe { &*((self.mmap_addr + offset) as *const MultibootMmapEntry) };
            if entry.memory_type == MULTIBOOT_MEMORY_AVAILABLE {
                map.push(PhysicalMemoryRegion {
                    start: entry.base_addr,
                    length: entry.length,
                    kind: PhysicalMemoryKind::Usable,
                });
            }
            let step = entry.size.saturating_add(4);
            if step == 0 {
                break;
            }
            offset = offset.saturating_add(step);
        }
        map
    }
}
