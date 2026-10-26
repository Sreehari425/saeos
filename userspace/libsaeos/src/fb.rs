//! The framebuffer, `/dev/fb0`: mapped into the process and drawn on
//! directly.
//!
//! ```text
//! let mut screen = Framebuffer::open()?;     // 960x720 on BIOS (QEMU), the GOP mode on UEFI
//! screen.fill_rect(0, 0, 100, 50, 0xff0000); // colours are 0xRRGGBB
//! screen.put(10, 10, 0xffffff);
//! ```
//!
//! While a `Framebuffer` exists the screen is the program's: the kernel
//! console stops drawing on it, and draws again once it is dropped (the
//! last descriptor to `/dev/fb0` closed). Forked children share the
//! mapping (and the open file).

use crate::errno::{Errno, Result, check};
use crate::raw;

const FBIOGET_VSCREENINFO: u64 = 0x4600;
const FBIOGET_FSCREENINFO: u64 = 0x4602;

pub struct Framebuffer {
    fd: u32,
    pixels: *mut u32,
    /// Bytes mapped.
    size: usize,
    pub width: usize,
    pub height: usize,
    /// Bytes per row.
    pub stride: usize,
    red_shift: u32,
    green_shift: u32,
    blue_shift: u32,
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

impl Framebuffer {
    pub fn open() -> Result<Self> {
        let fd = check(raw::open(b"/dev/fb0\0", raw::O_RDWR))? as u32;
        let result = Self::map(fd);
        if result.is_err() {
            raw::close(fd);
        }
        result
    }

    fn map(fd: u32) -> Result<Self> {
        let mut var = [0u8; 160];
        let mut fix = [0u8; 80];
        check(raw::ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr() as u64))?;
        check(raw::ioctl(fd, FBIOGET_FSCREENINFO, fix.as_mut_ptr() as u64))?;
        if u32_at(&var, 24) != 32 {
            return Err(Errno::EINVAL);
        }
        let stride = u32_at(&fix, 48) as usize;
        let height = u32_at(&var, 4) as usize;
        let size = (stride * height).next_multiple_of(4096);
        let address = check(raw::mmap_file(
            size as u64,
            raw::PROT_READ | raw::PROT_WRITE,
            raw::MAP_SHARED,
            fd,
            0,
        ))?;
        Ok(Self {
            fd,
            pixels: address as *mut u32,
            size,
            width: u32_at(&var, 0) as usize,
            height,
            stride,
            red_shift: u32_at(&var, 32),
            green_shift: u32_at(&var, 44),
            blue_shift: u32_at(&var, 56),
        })
    }

    pub fn fd(&self) -> u32 {
        self.fd
    }

    /// A `0xRRGGBB` colour as this screen's pixel.
    pub fn pixel(&self, rgb: u32) -> u32 {
        let (red, green, blue) = ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
        (red << self.red_shift) | (green << self.green_shift) | (blue << self.blue_shift)
    }

    /// The pixel's `0xRRGGBB` colour.
    pub fn colour(&self, pixel: u32) -> u32 {
        let channel = |shift: u32| (pixel >> shift) & 0xff;
        (channel(self.red_shift) << 16)
            | (channel(self.green_shift) << 8)
            | channel(self.blue_shift)
    }

    /// Row `y` as pixels (`width` of them).
    pub fn row(&mut self, y: usize) -> &mut [u32] {
        assert!(y < self.height);
        unsafe {
            core::slice::from_raw_parts_mut(self.pixels.byte_add(y * self.stride), self.width)
        }
    }

    pub fn put(&mut self, x: usize, y: usize, rgb: u32) {
        if x < self.width && y < self.height {
            let pixel = self.pixel(rgb);
            self.row(y)[x] = pixel;
        }
    }

    pub fn get(&mut self, x: usize, y: usize) -> u32 {
        let pixel = self.row(y)[x];
        self.colour(pixel)
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, rgb: u32) {
        let pixel = self.pixel(rgb);
        let right = (x + width).min(self.width);
        for row in y..(y + height).min(self.height) {
            if x < right {
                self.row(row)[x..right].fill(pixel);
            }
        }
    }
}

impl Drop for Framebuffer {
    fn drop(&mut self) {
        raw::munmap(self.pixels as u64, self.size as u64);
        raw::close(self.fd);
    }
}
