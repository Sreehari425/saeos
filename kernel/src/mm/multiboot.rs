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

/// One entry of the `mods_addr` array.
#[repr(C)]
pub struct MultibootModule {
    pub mod_start: u32,
    pub mod_end: u32,
    pub string: u32,
    pub reserved: u32,
}

impl MultibootInfo {
    /// The bootloader's command line (flag bit 2), up to its NUL. Must be
    /// called while low memory is still identity mapped.
    pub fn cmdline_bytes(&self) -> Option<&'static [u8]> {
        if self.flags & (1 << 2) == 0 || self.cmdline == 0 {
            return None;
        }
        let start = self.cmdline as usize as *const u8;
        let mut length = 0;
        // Bounded so a missing terminator cannot run off into memory.
        while length < crate::boot::boot_info::COMMAND_LINE_SIZE * 2
            && unsafe { start.add(length).read() } != 0
        {
            length += 1;
        }
        Some(unsafe { core::slice::from_raw_parts(start, length) })
    }

    /// Loaded modules (flag bit 3) as physical `start..end`.
    pub fn modules(&self) -> &'static [MultibootModule] {
        if self.flags & (1 << 3) == 0 || self.mods_addr == 0 {
            return &[];
        }
        unsafe {
            core::slice::from_raw_parts(
                self.mods_addr as usize as *const MultibootModule,
                self.mods_count as usize,
            )
        }
    }

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
