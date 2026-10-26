//! Intel AC'97 audio (ICH, `8086:2415`; QEMU's `-device AC97`): playback
//! for the sound core, polled.
//!
//! ```text
//! BAR0  mixer (NAM), 16-bit registers      BAR1  bus master (NABM)
//!   0x00 reset (any write)                   0x10 PCM out box:
//!   0x02 master volume  (bit 15 mute)          0x10 BDBAR  descriptor list address
//!   0x18 PCM out volume (bit 15 mute)          0x14 CIV    entry playing
//!   0x28 extended audio ID (bit 0: VRA)        0x15 LVI    last valid entry
//!   0x2a extended control  (bit 0: VRA on)     0x16 SR     status (bit 0 DCH: halted)
//!   0x2c front DAC rate (Hz, with VRA)         0x1b CR     control (bit 0 run, bit 1 reset)
//!                                            0x2c GLOB_CNT (bit 1: out of cold reset)
//!                                            0x30 GLOB_STA (bit 8: codec ready)
//! ```
//!
//! The card plays a list of 32 buffer descriptors, `{address: u32,
//! samples: u16, flags: u16}`, from entry `CIV` up to `LVI`, then halts (DCH).
//! Each descriptor here is one period of the core's ring. `samples` counts
//! 16-bit samples (two per stereo frame), not bytes or frames. The
//! buffer-underrun-policy flag (BUP, bit 30) is left clear, as Linux's
//! `intel8x0` leaves it: with it set, QEMU's card holds the last sample
//! once it runs out (a DC offset, and a click at each end of a gap)
//! instead of sending zeros. Without variable rate audio (VRA) the codec
//! plays 48 kHz only.
//!
//! No interrupts: AC'97 has no MSI, and its INTx line would need ACPI
//! `_PRT` routing in APIC mode. The core polls [`AudioDevice::status`].

use crate::arch::x86_64::cpu;
use crate::drivers::pci::{self, Enable, MappedBar, device::PciDevice};
use crate::mm::dma::{DmaAddress, DmaBuffer, DmaConstraints};
use crate::sound::AudioDevice;
use crate::sound::ring::{PERIOD_BYTES, PERIODS, Status};
use alloc::boxed::Box;

// Mixer (NAM).
const RESET: u16 = 0x00;
const MASTER_VOLUME: u16 = 0x02;
const PCM_OUT_VOLUME: u16 = 0x18;
const EXTENDED_ID: u16 = 0x28;
const EXTENDED_CONTROL: u16 = 0x2a;
const FRONT_DAC_RATE: u16 = 0x2c;
const EXTENDED_VRA: u16 = 1 << 0;
/// 0 dB on both channels, unmuted.
const VOLUME_0DB: u16 = 0x0808;
const VOLUME_MAX: u16 = 0x0000;

// Bus master (NABM): the PCM out box, and global registers.
const PO_BDBAR: u16 = 0x10;
const PO_CIV: u16 = 0x14;
const PO_LVI: u16 = 0x15;
const PO_SR: u16 = 0x16;
const PO_CR: u16 = 0x1b;
const GLOBAL_CONTROL: u16 = 0x2c;
const GLOBAL_STATUS: u16 = 0x30;
const SR_HALTED: u16 = 1 << 0;
/// Write-1-to-clear status bits (last valid, completion, FIFO error).
const SR_CLEAR: u16 = 0x1c;
const CR_RUN: u8 = 1 << 0;
const CR_RESET: u8 = 1 << 1;
const GLOBAL_COLD_RESET_OFF: u32 = 1 << 1;
const GLOBAL_CODEC_READY: u32 = 1 << 8;

const MIN_RATE: u32 = 8_000;
const NATIVE_RATE: u32 = 48_000;

struct Ac97 {
    mixer: u16,
    bus_master: u16,
    descriptors: DmaBuffer,
    variable_rate: bool,
}

impl Ac97 {
    fn mixer_read(&self, register: u16) -> u16 {
        unsafe { cpu::inw(self.mixer + register) }
    }

    fn mixer_write(&self, register: u16, value: u16) {
        unsafe { cpu::outw(self.mixer + register, value) }
    }

    fn reset_box(&self) {
        unsafe {
            cpu::outb(self.bus_master + PO_CR, 0);
            cpu::outb(self.bus_master + PO_CR, CR_RESET);
            // Reset clears itself; bounded in case the card never does.
            for _ in 0..100_000 {
                if cpu::inb(self.bus_master + PO_CR) & CR_RESET == 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            cpu::outw(self.bus_master + PO_SR, SR_CLEAR);
        }
    }
}

impl AudioDevice for Ac97 {
    fn name(&self) -> &'static str {
        "Intel AC'97"
    }

    fn attach(&mut self, ring: DmaAddress) {
        let entries = self.descriptors.as_mut_slice();
        for period in 0..PERIODS {
            let address = u32::try_from(ring.0 + (period * PERIOD_BYTES) as u64)
                .expect("the ring is below 4 GiB");
            let samples = (PERIOD_BYTES / 2) as u32;
            let entry = &mut entries[period * 8..period * 8 + 8];
            entry[..4].copy_from_slice(&address.to_le_bytes());
            entry[4..].copy_from_slice(&samples.to_le_bytes());
        }
        self.descriptors.sync_for_device();
    }

    fn rate_for(&self, hz: u32) -> u32 {
        if self.variable_rate {
            hz.clamp(MIN_RATE, NATIVE_RATE)
        } else {
            NATIVE_RATE
        }
    }

    fn set_rate(&mut self, hz: u32) -> u32 {
        if !self.variable_rate {
            return NATIVE_RATE;
        }
        self.mixer_write(FRONT_DAC_RATE, hz.clamp(MIN_RATE, NATIVE_RATE) as u16);
        u32::from(self.mixer_read(FRONT_DAC_RATE))
    }

    fn start(&mut self, last: usize) {
        self.reset_box();
        let descriptors = self.descriptors.device_address().0 as u32;
        unsafe {
            cpu::outl(self.bus_master + PO_BDBAR, descriptors);
            cpu::outb(self.bus_master + PO_LVI, last as u8);
            cpu::outb(self.bus_master + PO_CR, CR_RUN);
        }
    }

    fn submit(&mut self, last: usize) {
        unsafe { cpu::outb(self.bus_master + PO_LVI, last as u8) };
    }

    fn status(&mut self) -> Status {
        unsafe {
            Status {
                current: usize::from(cpu::inb(self.bus_master + PO_CIV)) % PERIODS,
                halted: cpu::inw(self.bus_master + PO_SR) & SR_HALTED != 0,
            }
        }
    }

    fn stop(&mut self) {
        self.reset_box();
    }
}

fn io_bar(device: &PciDevice, index: usize) -> Result<u16, &'static str> {
    match pci::map_bar(device, index)? {
        MappedBar::Io { port, .. } => Ok(port),
        MappedBar::Memory { .. } => Err("expected an I/O BAR"),
    }
}

/// The `pci::DRIVERS` probe: bring the codec up and register the card.
pub fn probe(device: &PciDevice) -> Result<(), &'static str> {
    pci::enable(
        device,
        Enable {
            io: true,
            memory: false,
            bus_master: true,
        },
    );
    let mixer = io_bar(device, 0)?;
    let bus_master = io_bar(device, 1)?;
    unsafe {
        cpu::outl(bus_master + GLOBAL_CONTROL, GLOBAL_COLD_RESET_OFF);
    }
    // The codec reports ready within a few hundred microseconds. Probing
    // runs before the scheduler, so this spins (each port read takes about
    // a microsecond on real hardware).
    let mut ready = false;
    for _ in 0..1_000_000 {
        if unsafe { cpu::inl(bus_master + GLOBAL_STATUS) } & GLOBAL_CODEC_READY != 0 {
            ready = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !ready {
        return Err("codec not ready");
    }
    let descriptors = DmaBuffer::new(PERIODS * 8, DmaConstraints::BELOW_4G)
        .map_err(|_| "no memory for the descriptor list")?;
    let mut card = Ac97 {
        mixer,
        bus_master,
        descriptors,
        variable_rate: false,
    };
    card.mixer_write(RESET, 0);
    card.mixer_write(MASTER_VOLUME, VOLUME_MAX);
    card.mixer_write(PCM_OUT_VOLUME, VOLUME_0DB);
    if card.mixer_read(EXTENDED_ID) & EXTENDED_VRA != 0 {
        let control = card.mixer_read(EXTENDED_CONTROL);
        card.mixer_write(EXTENDED_CONTROL, control | EXTENDED_VRA);
        card.variable_rate = card.mixer_read(EXTENDED_CONTROL) & EXTENDED_VRA != 0;
    }
    card.reset_box();
    crate::sound::register(Box::new(card))
}

/// AC'97 audio controllers (multimedia, audio: class 04:01).
pub fn matches(device: &PciDevice) -> bool {
    (device.class, device.subclass) == (0x04, 0x01)
}
