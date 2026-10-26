//! Bochs VBE ("DISPI"): the graphics modes of the standard VGA in QEMU,
//! Bochs and VirtualBox (`1234:1111`). Not on real hardware (real machines
//! get pixels from UEFI GOP); this gives BIOS boots in QEMU a framebuffer.
//!
//! ```text
//! index port 0x1CE, data port 0x1CF (16-bit):
//!   0 ID           0xB0C0..=0xB0C5
//!   1 XRES  2 YRES  3 BPP
//!   4 ENABLE       bit 0 on, bit 1 report capabilities (XRES/YRES/BPP read
//!                  as maxima), bit 6 linear framebuffer
//!   6 VIRT_WIDTH   8 X_OFFSET  9 Y_OFFSET (the picture starts here)
//!   10 VIDEO_MEMORY_64K   video memory, in 64 KiB
//! the framebuffer: PCI BAR0 (the whole video memory)
//! ```
//!
//! The mode is 960x720, 32 bpp (exactly twice Bad Apple's 480x360), or
//! 640x480 if the card cannot; it is switched on when `/dev/fb0` is first
//! opened and off after the last close. Getting back to the BIOS's text
//! mode takes care, since no BIOS call is available:
//!
//! - enabling VBE reprograms the VGA registers for graphics, and disabling
//!   it does not undo that, so they are saved before and restored after
//!   ([`VgaRegisters`]);
//! - the start of video memory holds the text mode's planes, font
//!   included: the mode is enabled without clearing memory (`NOCLEARMEM`),
//!   and the picture starts past the first 256 KiB (`Y_OFFSET`), on a page
//!   boundary so it can be mapped.

use crate::arch::x86_64::cpu;
use crate::drivers::fb::{self, Display, Mode, PixelOrder};
use crate::drivers::pci::{self, Enable, MappedBar, device::PciDevice};
use alloc::boxed::Box;

const INDEX_PORT: u16 = 0x1ce;
const DATA_PORT: u16 = 0x1cf;
const ID: u16 = 0;
const XRES: u16 = 1;
const YRES: u16 = 2;
const BPP: u16 = 3;
const ENABLE: u16 = 4;
const VIRT_WIDTH: u16 = 6;
const X_OFFSET: u16 = 8;
const Y_OFFSET: u16 = 9;
const VIDEO_MEMORY_64K: u16 = 10;
const ENABLED: u16 = 1 << 0;
const GET_CAPABILITIES: u16 = 1 << 1;
const LINEAR_FRAMEBUFFER: u16 = 1 << 6;
const NO_CLEAR_MEMORY: u16 = 1 << 7;
/// VGA text mode's planes and font live in the first 256 KiB.
const TEXT_PLANES: usize = 256 * 1024;
const PAGE: usize = 4096;
/// Preferred mode first.
const MODES: [(usize, usize); 2] = [(960, 720), (640, 480)];

fn read(index: u16) -> u16 {
    unsafe {
        cpu::outw(INDEX_PORT, index);
        cpu::inw(DATA_PORT)
    }
}

fn write(index: u16, value: u16) {
    unsafe {
        cpu::outw(INDEX_PORT, index);
        cpu::outw(DATA_PORT, value);
    }
}

/// The first row past the text planes that starts on a page boundary.
fn first_row(stride: usize) -> usize {
    let mut row = TEXT_PLANES.div_ceil(stride);
    while !(row * stride).is_multiple_of(PAGE) {
        row += 1;
    }
    row
}

/// The VGA registers that make up a mode (all of them readable), so the
/// text mode can be put back exactly as the BIOS left it.
#[derive(Clone, Copy)]
struct VgaRegisters {
    misc: u8,
    sequencer: [u8; 5],
    crtc: [u8; 25],
    graphics: [u8; 9],
    attribute: [u8; 21],
}

const MISC_READ: u16 = 0x3cc;
const MISC_WRITE: u16 = 0x3c2;
const SEQUENCER: u16 = 0x3c4;
const CRTC: u16 = 0x3d4;
const GRAPHICS: u16 = 0x3ce;
const ATTRIBUTE: u16 = 0x3c0;
const ATTRIBUTE_READ: u16 = 0x3c1;
/// Reading it resets the attribute controller's index/data flip-flop.
const INPUT_STATUS: u16 = 0x3da;
/// Attribute index bit: the screen shows the palette (video on).
const PALETTE_ENABLE: u8 = 0x20;

fn indexed_read(port: u16, index: u8) -> u8 {
    unsafe {
        cpu::outb(port, index);
        cpu::inb(port + 1)
    }
}

fn indexed_write(port: u16, index: u8, value: u8) {
    unsafe {
        cpu::outb(port, index);
        cpu::outb(port + 1, value);
    }
}

impl VgaRegisters {
    fn save() -> Self {
        let mut saved = Self {
            misc: unsafe { cpu::inb(MISC_READ) },
            sequencer: [0; 5],
            crtc: [0; 25],
            graphics: [0; 9],
            attribute: [0; 21],
        };
        for (index, value) in saved.sequencer.iter_mut().enumerate() {
            *value = indexed_read(SEQUENCER, index as u8);
        }
        for (index, value) in saved.crtc.iter_mut().enumerate() {
            *value = indexed_read(CRTC, index as u8);
        }
        for (index, value) in saved.graphics.iter_mut().enumerate() {
            *value = indexed_read(GRAPHICS, index as u8);
        }
        for (index, value) in saved.attribute.iter_mut().enumerate() {
            unsafe {
                cpu::inb(INPUT_STATUS);
                cpu::outb(ATTRIBUTE, index as u8);
                *value = cpu::inb(ATTRIBUTE_READ);
            }
        }
        unsafe {
            cpu::inb(INPUT_STATUS);
            cpu::outb(ATTRIBUTE, PALETTE_ENABLE);
        }
        saved
    }

    fn restore(&self) {
        unsafe { cpu::outb(MISC_WRITE, self.misc) };
        for (index, &value) in self.sequencer.iter().enumerate() {
            indexed_write(SEQUENCER, index as u8, value);
        }
        // CRTC registers 0-7 are write-protected while bit 7 of 0x11 is set.
        indexed_write(CRTC, 0x11, self.crtc[0x11] & 0x7f);
        for (index, &value) in self.crtc.iter().enumerate() {
            let value = if index == 0x11 { value & 0x7f } else { value };
            indexed_write(CRTC, index as u8, value);
        }
        indexed_write(CRTC, 0x11, self.crtc[0x11]);
        for (index, &value) in self.graphics.iter().enumerate() {
            indexed_write(GRAPHICS, index as u8, value);
        }
        for (index, &value) in self.attribute.iter().enumerate() {
            unsafe {
                cpu::inb(INPUT_STATUS);
                cpu::outb(ATTRIBUTE, index as u8);
                cpu::outb(ATTRIBUTE, value);
            }
        }
        unsafe {
            cpu::inb(INPUT_STATUS);
            cpu::outb(ATTRIBUTE, PALETTE_ENABLE);
        }
    }
}

struct BochsVbe {
    mode: Mode,
    /// The picture's first row in video memory.
    y_offset: u16,
    /// The text mode to go back to, while the graphics mode is on.
    text_mode: Option<VgaRegisters>,
}

impl Display for BochsVbe {
    fn name(&self) -> &'static str {
        "Bochs VBE"
    }

    fn mode(&self) -> Mode {
        self.mode
    }

    fn activate(&mut self) {
        if self.text_mode.is_none() {
            self.text_mode = Some(VgaRegisters::save());
        }
        write(ENABLE, 0);
        write(XRES, self.mode.width as u16);
        write(YRES, self.mode.height as u16);
        write(BPP, 32);
        write(ENABLE, ENABLED | LINEAR_FRAMEBUFFER | NO_CLEAR_MEMORY);
        write(VIRT_WIDTH, self.mode.width as u16);
        write(X_OFFSET, 0);
        write(Y_OFFSET, self.y_offset);
    }

    fn deactivate(&mut self) {
        write(ENABLE, 0);
        if let Some(text_mode) = self.text_mode.take() {
            text_mode.restore();
        }
    }
}

/// The `pci::DRIVERS` probe for QEMU's standard VGA.
pub fn probe(device: &PciDevice) -> Result<(), &'static str> {
    // Under UEFI the GOP framebuffer is already /dev/fb0, and its mode is
    // this card's: touching the registers (even reading the capabilities,
    // which turns the mode off) would blank the firmware's display.
    if let Some(name) = fb::registered() {
        crate::kinfo!("bochs-vbe: not used, {} is the display", name);
        return Ok(());
    }
    write(ID, 0xb0c5);
    let id = read(ID);
    if !(0xb0c0..=0xb0c5).contains(&id) {
        return Err("no Bochs VBE interface");
    }
    write(ENABLE, GET_CAPABILITIES);
    let (max_width, max_height, max_bpp) = (read(XRES), read(YRES), read(BPP));
    write(ENABLE, 0);
    let memory = usize::from(read(VIDEO_MEMORY_64K)) * 64 * 1024;
    pci::enable(
        device,
        Enable {
            io: true,
            memory: true,
            bus_master: false,
        },
    );
    let MappedBar::Memory { address, size } = pci::map_bar(device, 0)? else {
        return Err("expected a memory BAR");
    };
    let physical = match device.bars[0] {
        Some(pci::device::Bar::Memory { address, .. }) => address,
        _ => return Err("expected a memory BAR"),
    };
    let memory = memory.min(size as usize);
    let (width, height, y_offset) = MODES
        .iter()
        .map(|&(width, height)| (width, height, first_row(width * 4)))
        .find(|&(width, height, first)| {
            width <= usize::from(max_width)
                && height <= usize::from(max_height)
                && max_bpp >= 32
                && (first + height) * width * 4 <= memory
        })
        .ok_or("no mode fits the card")?;
    let stride = width * 4;
    let start = (y_offset * stride) as u64;
    fb::register(Box::new(BochsVbe {
        mode: Mode {
            physical: physical + start,
            mapped: address + start,
            width,
            height,
            stride,
            order: PixelOrder::Bgrx,
        },
        y_offset: y_offset as u16,
        text_mode: None,
    }));
    Ok(())
}

/// QEMU/Bochs standard VGA.
pub fn matches(device: &PciDevice) -> bool {
    (device.vendor, device.device_id) == (0x1234, 0x1111)
}
