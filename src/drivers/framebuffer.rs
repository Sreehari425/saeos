use crate::boot::boot_info::{FramebufferInfo, PixelFormat};
use crate::drivers::font::{FONT_8X16, FONT_HEIGHT, FONT_WIDTH};
use crate::drivers::vga::Color;
use crate::sync::SpinMutex;
use core::fmt::{self, Write};
use core::ptr::write_volatile;

pub struct GopWriter {
    base_addr: u64,
    width: usize,
    height: usize,
    stride: usize,
    pixel_format: PixelFormat,
    cursor_col: usize,
    cursor_row: usize,
    fg_color: u32,
    bg_color: u32,
}

unsafe impl Send for GopWriter {}

impl GopWriter {
    pub const fn empty() -> Self {
        Self {
            base_addr: 0,
            width: 0,
            height: 0,
            stride: 0,
            pixel_format: PixelFormat::Rgb,
            cursor_col: 0,
            cursor_row: 0,
            fg_color: 0x00FFFFFF, // White
            bg_color: 0x00000000, // Black
        }
    }

    pub fn init(&mut self, info: FramebufferInfo) {
        self.base_addr = info.base_addr;
        self.width = info.width;
        self.height = info.height;
        self.stride = info.stride;
        self.pixel_format = info.pixel_format;
        self.cursor_col = 0;
        self.cursor_row = 0;
        self.fg_color = 0x00FFFFFF;
        self.bg_color = 0x00000000;
        self.clear_screen();
    }

    pub fn is_active(&self) -> bool {
        self.base_addr != 0 && self.width > 0 && self.height > 0
    }

    pub fn cols(&self) -> usize {
        if self.width == 0 {
            0
        } else {
            self.width / FONT_WIDTH
        }
    }

    pub fn rows(&self) -> usize {
        if self.height == 0 {
            0
        } else {
            self.height / FONT_HEIGHT
        }
    }

    #[inline]
    fn color_to_u32(color: Color, format: PixelFormat) -> u32 {
        let (r, g, b): (u8, u8, u8) = match color {
            Color::Black => (0, 0, 0),
            Color::Blue => (0, 0, 170),
            Color::Green => (0, 170, 0),
            Color::Cyan => (0, 170, 170),
            Color::Red => (170, 0, 0),
            Color::Magenta => (170, 0, 170),
            Color::Brown => (170, 85, 0),
            Color::LightGray => (170, 170, 170),
            Color::DarkGray => (85, 85, 85),
            Color::LightBlue => (85, 85, 255),
            Color::LightGreen => (85, 255, 85),
            Color::LightCyan => (85, 255, 255),
            Color::LightRed => (255, 85, 85),
            Color::Pink => (255, 85, 255),
            Color::Yellow => (255, 255, 85),
            Color::White => (255, 255, 255),
        };

        match format {
            PixelFormat::Rgb => ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
            PixelFormat::Bgr | PixelFormat::Unknown => {
                ((b as u32) << 16) | ((g as u32) << 8) | (r as u32)
            }
        }
    }

    pub fn set_color(&mut self, fg: Color, bg: Color) {
        self.fg_color = Self::color_to_u32(fg, self.pixel_format);
        self.bg_color = Self::color_to_u32(bg, self.pixel_format);
    }

    #[inline]
    pub fn draw_pixel(&self, x: usize, y: usize, color: u32) {
        if x < self.width && y < self.height {
            let offset = (y * self.stride + x) * 4;
            unsafe {
                let pixel_ptr = (self.base_addr + offset as u64) as *mut u32;
                write_volatile(pixel_ptr, color);
            }
        }
    }

    pub fn draw_char(&self, c: char, col: usize, row: usize, fg: u32, bg: u32) {
        let x_start = col * FONT_WIDTH;
        let y_start = row * FONT_HEIGHT;
        let glyph_idx = (c as usize).min(255);
        let glyph = &FONT_8X16[glyph_idx];

        for (dy, &row_byte) in glyph.iter().enumerate() {
            for dx in 0..8 {
                let is_fg = (row_byte & (1 << (7 - dx))) != 0;
                let color = if is_fg { fg } else { bg };
                self.draw_pixel(x_start + dx, y_start + dy, color);
            }
        }
    }

    pub fn write_byte(&mut self, byte: u8) {
        if !self.is_active() {
            return;
        }

        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.cursor_col >= self.cols() {
                    self.new_line();
                }

                let c = byte as char;
                self.draw_char(
                    c,
                    self.cursor_col,
                    self.cursor_row,
                    self.fg_color,
                    self.bg_color,
                );
                self.cursor_col += 1;
            }
        }
    }

    pub fn new_line(&mut self) {
        if self.cursor_row + 1 < self.rows() {
            self.cursor_row += 1;
        } else {
            // Scroll screen up by 1 text-row (FONT_HEIGHT pixel rows).
            // Copy each scanline in bulk instead of pixel-by-pixel for performance.
            let pixel_rows_to_copy = (self.rows() - 1) * FONT_HEIGHT;
            let bytes_per_row = self.stride * 4; // 4 bytes per pixel (32-bit)

            for y in 0..pixel_rows_to_copy {
                let src_offset = ((y + FONT_HEIGHT) * self.stride) * 4;
                let dst_offset = (y * self.stride) * 4;
                unsafe {
                    let src = (self.base_addr + src_offset as u64) as *const u8;
                    let dst = (self.base_addr + dst_offset as u64) as *mut u8;
                    // Copy one full scanline (stride × 4 bytes) at once
                    core::ptr::copy_nonoverlapping(src, dst, bytes_per_row);
                }
            }

            // Clear bottom text row (FONT_HEIGHT pixel rows)
            let clear_start_y = pixel_rows_to_copy;
            for y in clear_start_y..self.height {
                let row_offset = (y * self.stride) * 4;
                unsafe {
                    let row_ptr = (self.base_addr + row_offset as u64) as *mut u32;
                    // Fill the entire scanline with background colour
                    for x in 0..self.width {
                        core::ptr::write_volatile(row_ptr.add(x), self.bg_color);
                    }
                }
            }
        }
        self.cursor_col = 0;
    }

    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.draw_char(
                ' ',
                self.cursor_col,
                self.cursor_row,
                self.fg_color,
                self.bg_color,
            );
        }
    }

    pub fn clear_screen(&mut self) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.draw_pixel(x, y, self.bg_color);
            }
        }
        self.cursor_col = 0;
        self.cursor_row = 0;
    }
}

impl Write for GopWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}

pub static GOP_WRITER: SpinMutex<GopWriter> = SpinMutex::new(GopWriter::empty());
