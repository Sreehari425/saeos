//! One HD Audio output stream playing the sound core's ring.
//!
//! ```text
//! descriptor (controller.output_stream(0)):
//! +0x00 CTL   u8 bit 0 SRST (reset), bit 1 RUN;  +0x02 u8: stream tag << 4
//! +0x03 STS   u8 (write 1 to clear)    +0x04 LPIB u32: byte position in the buffer
//! +0x08 CBL   u32 cyclic buffer length +0x0C LVI  u16: last buffer list entry
//! +0x12 FMT   u16 the stream format    +0x18 BDPL / +0x1C BDPU: buffer list address
//! buffer list entry (16 bytes): address u64, length u32, flags u32 (bit 0 IOC)
//! ```
//!
//! The buffer list is written once: entry *i* is ring period *i*, all 32
//! of them, over a cyclic buffer of the whole ring (Linux works the same
//! way; controllers may cache the list, so it is never edited while the
//! stream runs). HD Audio plays it round and round: unlike AC'97 it never
//! stops at the last valid entry. Each poll ([`Stream::status`]) reads the
//! position (LPIB): once it has moved past the last period the core
//! submitted, the card is reported halted, so nothing more is submitted
//! into periods it has passed. The stream itself keeps running, through
//! silence (the core keeps every unsubmitted period zeroed, see
//! `sound::ring`), until the core starts or stops it: LPIB counts what the
//! controller fetched, and stopping at once would cut audio still buffered
//! on the way to the codec (QEMU drops it).

use super::controller::{Registers, dma_write32};
use crate::mm::dma::{DmaAddress, DmaBuffer, DmaConstraints};
use crate::sound::ring::{PERIOD_BYTES, PERIODS, RING_BYTES, Status};

const CTL: u64 = 0x00;
const CTL_TAG: u64 = 0x02;
const STS: u64 = 0x03;
const LPIB: u64 = 0x04;
const CBL: u64 = 0x08;
const LVI: u64 = 0x0c;
const FMT: u64 = 0x12;
const BDPL: u64 = 0x18;
const BDPU: u64 = 0x1c;
const CTL_RESET: u8 = 1 << 0;
const CTL_RUN: u8 = 1 << 1;
const STS_CLEAR: u8 = 0x1c;
const ENTRY_BYTES: usize = 16;

pub struct Stream {
    registers: Registers,
    descriptor: u64,
    pub tag: u8,
    list: DmaBuffer,
    ring: DmaAddress,
    /// The period the controller was on at the last poll.
    previous: usize,
    /// The last period the core submitted.
    last: usize,
    running: bool,
    /// Past the last submitted period: running through silence.
    drained: bool,
}

impl Stream {
    pub fn new(registers: Registers, descriptor: u64, tag: u8) -> Result<Self, &'static str> {
        let list = DmaBuffer::new(PERIODS * ENTRY_BYTES, DmaConstraints::BELOW_4G)
            .map_err(|_| "no memory for the buffer list")?;
        Ok(Self {
            registers,
            descriptor,
            tag,
            list,
            ring: DmaAddress(0),
            previous: 0,
            last: 0,
            running: false,
            drained: false,
        })
    }

    /// Entry *i* plays ring period *i*.
    pub fn attach(&mut self, ring: DmaAddress) {
        self.ring = ring;
        for index in 0..PERIODS {
            let address = ring.0 + (index * PERIOD_BYTES) as u64;
            let entry = index * ENTRY_BYTES;
            dma_write32(&mut self.list, entry, address as u32);
            dma_write32(&mut self.list, entry + 4, (address >> 32) as u32);
            dma_write32(&mut self.list, entry + 8, PERIOD_BYTES as u32);
            dma_write32(&mut self.list, entry + 12, 0);
        }
    }

    fn register(&self, offset: u64) -> u64 {
        self.descriptor + offset
    }

    fn reset(&mut self) {
        let r = self.registers;
        r.write8(self.register(CTL), 0);
        r.wait(|r| r.read8(self.descriptor + CTL) & CTL_RUN == 0);
        r.write8(self.register(CTL), CTL_RESET);
        r.wait(|r| r.read8(self.descriptor + CTL) & CTL_RESET != 0);
        r.write8(self.register(CTL), 0);
        r.wait(|r| r.read8(self.descriptor + CTL) & CTL_RESET == 0);
        r.write8(self.register(STS), STS_CLEAR);
        self.running = false;
        self.drained = false;
    }

    /// Play from period 0, up to `last` (and whatever is submitted
    /// later), in `format`.
    pub fn start(&mut self, last: usize, format: u16) {
        self.reset();
        let r = self.registers;
        let list = self.list.device_address().0;
        r.write32(self.register(BDPL), list as u32);
        r.write32(self.register(BDPU), (list >> 32) as u32);
        r.write32(self.register(CBL), RING_BYTES as u32);
        r.write16(self.register(LVI), (PERIODS - 1) as u16);
        r.write16(self.register(FMT), format);
        r.write8(self.register(CTL_TAG), self.tag << 4);
        self.previous = 0;
        self.last = last;
        r.write8(self.register(CTL), CTL_RUN);
        self.running = true;
    }

    pub fn submit(&mut self, last: usize) {
        self.last = last % PERIODS;
    }

    pub fn status(&mut self) -> Status {
        if !self.running || self.drained {
            return Status {
                current: self.last,
                halted: true,
            };
        }
        let position = self.registers.read32(self.register(LPIB)) as usize % RING_BYTES;
        let current = position / PERIOD_BYTES;
        // The submitted periods run from the one playing at the last poll
        // to `last`; polls come every half period, so the controller cannot
        // have gone round the ring in between.
        let submitted = (self.last + PERIODS - self.previous) % PERIODS + 1;
        let ahead = (current + PERIODS - self.previous) % PERIODS;
        if ahead >= submitted {
            // Past the last submitted period: playing silence from here.
            self.drained = true;
            return Status {
                current: self.last,
                halted: true,
            };
        }
        self.previous = current;
        Status {
            current,
            halted: false,
        }
    }

    pub fn stop(&mut self) {
        self.reset();
    }
}
