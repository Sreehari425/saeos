//! Unified boot information passed to kernel_main across both BIOS and UEFI.

#[derive(Debug, Clone, Copy)]
pub enum PixelFormat {
    Rgb,
    Bgr,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    pub base_addr: u64,
    pub size: usize,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub pixel_format: PixelFormat,
}

#[derive(Debug, Clone, Copy)]
pub enum DisplayMode {
    VgaText { buffer_addr: usize },
    GopFramebuffer(FramebufferInfo),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalMemoryRegion {
    pub start: u64,
    pub length: u64,
}

impl PhysicalMemoryRegion {
    pub const EMPTY: Self = Self {
        start: 0,
        length: 0,
    };
    pub const fn end(self) -> u64 {
        self.start.saturating_add(self.length)
    }
}

pub const MAX_MEMORY_REGIONS: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct PhysicalMemoryMap {
    pub regions: [PhysicalMemoryRegion; MAX_MEMORY_REGIONS],
    pub count: usize,
}

impl PhysicalMemoryMap {
    pub const fn empty() -> Self {
        Self {
            regions: [PhysicalMemoryRegion::EMPTY; MAX_MEMORY_REGIONS],
            count: 0,
        }
    }
    pub fn push(&mut self, region: PhysicalMemoryRegion) {
        if region.length != 0 && self.count < MAX_MEMORY_REGIONS {
            self.regions[self.count] = region;
            self.count += 1;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    Bios,
    Uefi,
}

#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    pub display: DisplayMode,
    pub boot_mode: BootMode,
    pub memory_map: PhysicalMemoryMap,
    pub kernel_physical_start: u64,
    pub kernel_physical_end: u64,
    pub rsdp_addr: Option<u64>,
}

impl BootInfo {
    pub const fn default_bios() -> Self {
        Self {
            display: DisplayMode::VgaText {
                buffer_addr: 0xb8000,
            },
            boot_mode: BootMode::Bios,
            memory_map: PhysicalMemoryMap::empty(),
            kernel_physical_start: 0,
            kernel_physical_end: 0,
            rsdp_addr: None,
        }
    }

    pub fn total_memory_mb(&self) -> u64 {
        self.memory_map.regions[..self.memory_map.count]
            .iter()
            .map(|r| r.length)
            .sum::<u64>()
            / (1024 * 1024)
    }
}
