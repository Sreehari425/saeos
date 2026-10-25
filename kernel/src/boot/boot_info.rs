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
pub struct KernelSection {
    pub start: u64,
    pub end: u64,
}

impl KernelSection {
    pub const EMPTY: Self = Self { start: 0, end: 0 };

    pub const fn is_present(self) -> bool {
        self.start < self.end
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KernelSections {
    pub text: KernelSection,
    pub rodata: KernelSection,
    pub data: KernelSection,
    pub bss: KernelSection,
}

impl KernelSections {
    pub const EMPTY: Self = Self {
        text: KernelSection::EMPTY,
        rodata: KernelSection::EMPTY,
        data: KernelSection::EMPTY,
        bss: KernelSection::EMPTY,
    };
}

#[derive(Debug, Clone, Copy)]
pub enum DisplayMode {
    VgaText { buffer_addr: usize },
    GopFramebuffer(FramebufferInfo),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalMemoryKind {
    Usable,
    Reserved,
    AcpiReclaimable,
    AcpiNvs,
    Runtime,
    Mmio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalMemoryRegion {
    pub start: u64,
    pub length: u64,
    pub kind: PhysicalMemoryKind,
}

impl PhysicalMemoryRegion {
    pub const EMPTY: Self = Self {
        start: 0,
        length: 0,
        kind: PhysicalMemoryKind::Reserved,
    };
    pub const fn end(self) -> u64 {
        self.start.saturating_add(self.length)
    }
}

pub const MAX_MEMORY_REGIONS: usize = 256;

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

    pub fn push_merged(&mut self, region: PhysicalMemoryRegion) {
        if region.length == 0 {
            return;
        }
        if self.count != 0 {
            let previous = &mut self.regions[self.count - 1];
            if previous.kind == region.kind && previous.end() == region.start {
                previous.length = previous.length.saturating_add(region.length);
                return;
            }
        }
        self.push(region);
    }

    pub fn usable(&self) -> impl Iterator<Item = &PhysicalMemoryRegion> {
        self.regions[..self.count]
            .iter()
            .filter(|r| r.kind == PhysicalMemoryKind::Usable)
    }
}

/// A physical byte range, `start..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalRange {
    pub start: u64,
    pub end: u64,
}

/// Initramfs archives the bootloader loaded (Multiboot modules, or the file
/// read from the ESP), unpacked in order.
pub const MAX_INITRAMFS: usize = 4;

/// Linux's `COMMAND_LINE_SIZE`; longer command lines are truncated.
pub const COMMAND_LINE_SIZE: usize = 2048;

/// The kernel command line, copied out of bootloader memory before that
/// memory can be reused. Fixed-size so `BootInfo` stays `Copy`.
#[derive(Clone, Copy)]
pub struct Cmdline {
    bytes: [u8; COMMAND_LINE_SIZE],
    length: usize,
}

impl Cmdline {
    pub const EMPTY: Self = Self {
        bytes: [0; COMMAND_LINE_SIZE],
        length: 0,
    };

    /// Copy `text` (stopping at a NUL, without surrounding whitespace),
    /// truncated to the limit on a character boundary. Control characters
    /// such as newlines become spaces.
    pub fn new(text: &[u8]) -> Self {
        let text = text.split(|&byte| byte == 0).next().unwrap_or(&[]);
        let text = text.trim_ascii();
        let text = &text[..text.len().min(COMMAND_LINE_SIZE)];
        let valid = match core::str::from_utf8(text) {
            Ok(text) => text.len(),
            Err(error) => error.valid_up_to(),
        };
        let mut line = Self::EMPTY;
        for (slot, &byte) in line.bytes.iter_mut().zip(&text[..valid]) {
            *slot = if byte.is_ascii_control() { b' ' } else { byte };
        }
        line.length = valid;
        line
    }

    pub fn as_str(&self) -> &str {
        // `new` only stores a valid UTF-8 prefix.
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("")
    }
}

impl core::fmt::Debug for Cmdline {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), formatter)
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
    pub kernel_sections: KernelSections,
    pub rsdp_addr: Option<u64>,
    pub memory_map_descriptor_count: usize,
    pub memory_map_discarded: usize,
    pub initramfs: [Option<PhysicalRange>; MAX_INITRAMFS],
    pub cmdline: Cmdline,
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
            kernel_sections: KernelSections::EMPTY,
            rsdp_addr: None,
            memory_map_descriptor_count: 0,
            memory_map_discarded: 0,
            initramfs: [None; MAX_INITRAMFS],
            cmdline: Cmdline::EMPTY,
        }
    }

    pub fn total_memory_mb(&self) -> u64 {
        self.memory_map.usable().map(|r| r.length).sum::<u64>() / (1024 * 1024)
    }
}
