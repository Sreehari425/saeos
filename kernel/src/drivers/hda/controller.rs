//! The HD Audio controller: memory-mapped registers (BAR0), reset, and the
//! command rings through which the codecs are reached.
//!
//! ```text
//! 0x00 GCAP      u16  streams: output 15:12, input 11:8; bit 0: 64-bit DMA
//! 0x08 GCTL      u32  bit 0 CRST: 0 holds the link in reset
//! 0x0E STATESTS  u16  bit n: a codec answered at address n (write 1 to clear)
//! 0x40 CORBLBASE / 0x44 CORBUBASE   command ring (CORB) address
//! 0x48 CORBWP    u16  last command written (by us)
//! 0x4A CORBRP    u16  last command read (by the controller); bit 15 resets it
//! 0x4C CORBCTL   u8   bit 1: run      0x4E CORBSIZE u8: 2 = 256 entries
//! 0x50 RIRBLBASE / 0x54 RIRBUBASE   response ring (RIRB) address
//! 0x58 RIRBWP    u16  last response written (by the controller); bit 15 resets
//! 0x5A RINTCNT   u16  responses per interrupt (must be non-zero)
//! 0x5C RIRBCTL   u8   bit 1: DMA on, bit 0: response interrupt (flag)
//!                                      0x5D RIRBSTS u8: bit 0 response
//!                     count reached, bit 2 overrun (write 1 to clear)
//!                                      0x5E RIRBSIZE u8: 2 = 256 entries
//! 0x60 ICOI / 0x64 ICII / 0x68 ICIS   immediate command (one at a time)
//! 0x80 + 0x20 n  stream descriptors: input streams first, then output
//! ```
//!
//! A command is one 32-bit word in the CORB; the controller sends it to the
//! codec and writes the answer, 8 bytes (response, then codec address in
//! bits 3:0 and "unsolicited" in bit 4), into the RIRB. Commands here wait
//! for their answer before the next is sent, so matching is trivial;
//! unsolicited responses (jack events) are skipped. The controller may
//! hold further responses back until the "RINTCNT responses arrived" flag
//! in RIRBSTS is cleared (an interrupt handler's job; here, after every
//! response). That flag is only raised with the response interrupt
//! enabled in RIRBCTL, so it is: with INTCTL left at 0 no interrupt is
//! ever delivered. If the rings never answer, the immediate command
//! registers are used instead, as Linux's `single_cmd` fallback does.
//!
//! Probing runs before the scheduler, so every wait spins, bounded.

use super::codec::{CodecLink, command_word};
use crate::mm::dma::{DmaBuffer, DmaConstraints};

const GCAP: u64 = 0x00;
const GCTL: u64 = 0x08;
const STATESTS: u64 = 0x0e;
const CORBLBASE: u64 = 0x40;
const CORBUBASE: u64 = 0x44;
const CORBWP: u64 = 0x48;
const CORBRP: u64 = 0x4a;
const CORBCTL: u64 = 0x4c;
const CORBSIZE: u64 = 0x4e;
const RIRBLBASE: u64 = 0x50;
const RIRBUBASE: u64 = 0x54;
const RIRBWP: u64 = 0x58;
const RINTCNT: u64 = 0x5a;
const RIRBCTL: u64 = 0x5c;
const RIRBSTS: u64 = 0x5d;
const RIRBSIZE: u64 = 0x5e;
const ICOI: u64 = 0x60;
const ICII: u64 = 0x64;
const ICIS: u64 = 0x68;
const STREAMS: u64 = 0x80;
const STREAM_STRIDE: u64 = 0x20;

const GCTL_RUN: u32 = 1 << 0;
const RING_RUN: u8 = 1 << 1;
const RIRB_RESPONSE_INTERRUPT: u8 = 1 << 0;
const POINTER_RESET: u16 = 1 << 15;
const RING_ENTRIES: usize = 256;
const SIZE_256: u8 = 2;
const ICIS_BUSY: u16 = 1 << 0;
const ICIS_VALID: u16 = 1 << 1;
const UNSOLICITED: u32 = 1 << 4;
/// RIRBSTS: response count reached, and overrun.
const RIRBSTS_CLEAR: u8 = 0x05;
/// Spins for a register to change, or a codec to answer.
const SPINS: usize = 1_000_000;

/// The controller's registers.
#[derive(Clone, Copy)]
pub struct Registers {
    base: u64,
}

impl Registers {
    pub fn new(base: u64) -> Self {
        Self { base }
    }

    pub fn read8(&self, offset: u64) -> u8 {
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u8) }
    }

    pub fn read16(&self, offset: u64) -> u16 {
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u16) }
    }

    pub fn read32(&self, offset: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    pub fn write8(&self, offset: u64, value: u8) {
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u8, value) }
    }

    pub fn write16(&self, offset: u64, value: u16) {
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u16, value) }
    }

    pub fn write32(&self, offset: u64, value: u32) {
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }

    /// Spin until `done` holds; false on timeout.
    pub fn wait(&self, mut done: impl FnMut(&Self) -> bool) -> bool {
        for _ in 0..SPINS {
            if done(self) {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}

/// Read a little-endian u32 the device wrote.
pub fn dma_read32(buffer: &DmaBuffer, offset: usize) -> u32 {
    buffer.sync_for_cpu();
    unsafe { core::ptr::read_volatile(buffer.as_slice().as_ptr().add(offset) as *const u32) }
}

pub fn dma_write32(buffer: &mut DmaBuffer, offset: usize, value: u32) {
    unsafe {
        core::ptr::write_volatile(
            buffer.as_mut_slice().as_mut_ptr().add(offset) as *mut u32,
            value,
        )
    };
    buffer.sync_for_device();
}

pub struct Controller {
    pub registers: Registers,
    /// Input streams come first among the descriptors.
    pub input_streams: u8,
    pub output_streams: u8,
    /// Codec addresses that answered.
    pub codecs: u16,
    corb: DmaBuffer,
    rirb: DmaBuffer,
    corb_write: u16,
    rirb_read: u16,
    /// The rings did not work: immediate commands instead.
    immediate: bool,
}

impl Controller {
    /// Reset the controller and start the command rings.
    pub fn new(base: u64) -> Result<Self, &'static str> {
        let registers = Registers::new(base);
        let capabilities = registers.read16(GCAP);
        // Rings off, then the link through reset.
        registers.write8(CORBCTL, 0);
        registers.write8(RIRBCTL, 0);
        registers.wait(|r| r.read8(CORBCTL) & RING_RUN == 0 && r.read8(RIRBCTL) & RING_RUN == 0);
        registers.write32(GCTL, registers.read32(GCTL) & !GCTL_RUN);
        if !registers.wait(|r| r.read32(GCTL) & GCTL_RUN == 0) {
            return Err("controller did not enter reset");
        }
        registers.write32(GCTL, registers.read32(GCTL) | GCTL_RUN);
        if !registers.wait(|r| r.read32(GCTL) & GCTL_RUN != 0) {
            return Err("controller did not leave reset");
        }
        // Codecs announce themselves within 521 µs of the link starting.
        registers.wait(|r| r.read16(STATESTS) != 0);
        let codecs = registers.read16(STATESTS) & 0x7fff;
        registers.write16(STATESTS, codecs);

        let ring = |size: usize| {
            DmaBuffer::new(size, DmaConstraints::BELOW_4G).map_err(|_| "no memory for a ring")
        };
        let mut controller = Self {
            registers,
            input_streams: ((capabilities >> 8) & 0xf) as u8,
            output_streams: ((capabilities >> 12) & 0xf) as u8,
            codecs,
            corb: ring(RING_ENTRIES * 4)?,
            rirb: ring(RING_ENTRIES * 8)?,
            corb_write: 0,
            rirb_read: 0,
            immediate: false,
        };
        controller.start_rings();
        Ok(controller)
    }

    fn start_rings(&mut self) {
        let r = self.registers;
        let corb = self.corb.device_address().0;
        r.write32(CORBLBASE, corb as u32);
        r.write32(CORBUBASE, (corb >> 32) as u32);
        r.write8(CORBSIZE, SIZE_256);
        r.write16(CORBWP, 0);
        // Reset the read pointer: set the bit, see it, clear it, see that.
        r.write16(CORBRP, POINTER_RESET);
        r.wait(|r| r.read16(CORBRP) & POINTER_RESET != 0);
        r.write16(CORBRP, 0);
        r.wait(|r| r.read16(CORBRP) & POINTER_RESET == 0);
        r.write8(CORBCTL, RING_RUN);

        let rirb = self.rirb.device_address().0;
        r.write32(RIRBLBASE, rirb as u32);
        r.write32(RIRBUBASE, (rirb >> 32) as u32);
        r.write8(RIRBSIZE, SIZE_256);
        r.write16(RIRBWP, POINTER_RESET);
        r.write16(RINTCNT, 1);
        r.write8(RIRBSTS, RIRBSTS_CLEAR);
        r.write8(RIRBCTL, RING_RUN | RIRB_RESPONSE_INTERRUPT);
        self.corb_write = 0;
        self.rirb_read = 0;
    }

    /// One command through the rings: its response, or `None` on timeout.
    fn ring_command(&mut self, word: u32) -> Option<u32> {
        let r = self.registers;
        let slot = (self.corb_write + 1) % RING_ENTRIES as u16;
        dma_write32(&mut self.corb, usize::from(slot) * 4, word);
        self.corb_write = slot;
        r.write16(CORBWP, slot);
        for _ in 0..SPINS {
            let written = r.read16(RIRBWP) % RING_ENTRIES as u16;
            if self.rirb_read != written {
                r.write8(RIRBSTS, RIRBSTS_CLEAR);
            }
            while self.rirb_read != written {
                self.rirb_read = (self.rirb_read + 1) % RING_ENTRIES as u16;
                let entry = usize::from(self.rirb_read) * 8;
                let response = dma_read32(&self.rirb, entry);
                let extended = dma_read32(&self.rirb, entry + 4);
                if extended & UNSOLICITED == 0 {
                    return Some(response);
                }
            }
            core::hint::spin_loop();
        }
        None
    }

    fn immediate_command(&mut self, word: u32) -> Option<u32> {
        let r = self.registers;
        if !r.wait(|r| r.read16(ICIS) & ICIS_BUSY == 0) {
            return None;
        }
        r.write16(ICIS, ICIS_VALID);
        r.write32(ICOI, word);
        r.write16(ICIS, ICIS_BUSY);
        if !r.wait(|r| r.read16(ICIS) & (ICIS_BUSY | ICIS_VALID) == ICIS_VALID) {
            return None;
        }
        Some(r.read32(ICII))
    }

    /// Stream descriptor `index` among the output streams.
    pub fn output_stream(&self, index: u8) -> u64 {
        STREAMS + STREAM_STRIDE * u64::from(self.input_streams + index)
    }
}

impl CodecLink for Controller {
    fn command(&mut self, codec: u8, node: u8, verb: u32) -> Option<u32> {
        let word = command_word(codec, node, verb);
        if !self.immediate {
            if let Some(response) = self.ring_command(word) {
                return Some(response);
            }
            let r = self.registers;
            crate::kwarn!(
                "hda: command ring timed out (command {:#010x}; CORB wp {} rp {:#x} ctl {:#x}, RIRB wp {:#x} ctl {:#x} sts {:#x}, read {}); using immediate commands",
                word,
                self.corb_write,
                r.read16(CORBRP),
                r.read8(CORBCTL),
                r.read16(RIRBWP),
                r.read8(RIRBCTL),
                r.read8(RIRBSTS),
                self.rirb_read
            );
            self.immediate = true;
            self.registers.write8(CORBCTL, 0);
            self.registers.write8(RIRBCTL, 0);
        }
        self.immediate_command(word)
    }
}
