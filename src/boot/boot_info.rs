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
    VgaText {
        buffer_addr: usize,
    },
    GopFramebuffer(FramebufferInfo),
}

#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    pub display: DisplayMode,
    pub total_memory_mb: u64,
    pub rsdp_addr: Option<u64>,
}

impl BootInfo {
    pub const fn default_bios() -> Self {
        Self {
            display: DisplayMode::VgaText {
                buffer_addr: 0xb8000,
            },
            total_memory_mb: 128,
            rsdp_addr: None,
        }
    }
}
