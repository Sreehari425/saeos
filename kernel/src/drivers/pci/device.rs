//! One PCI function: identity, class, BARs, interrupt, capabilities.
//!
//! BARs (Base Address Registers) say where a device's registers live: a
//! range of I/O ports or of physical memory. Their size is learnt the
//! standard way: write all 1s, read back which bits stuck, restore.
//! Decoding is switched off in the command register meanwhile, so the
//! device never answers at the all-1s address. 64-bit memory BARs take two
//! slots.

use super::config::{
    Address, BAR0, CAPABILITIES, CLASS, COMMAND, COMMAND_IO, COMMAND_MEMORY, ConfigSpace,
    DEVICE_ID, HEADER_TYPE, INTERRUPT_LINE, INTERRUPT_PIN, NO_DEVICE, PROG_IF, REVISION, STATUS,
    STATUS_CAPABILITIES, SUBCLASS, VENDOR_ID,
};
use alloc::vec::Vec;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    /// Registers in I/O port space.
    Io { port: u16, size: u32 },
    /// Registers in physical memory.
    Memory {
        address: u64,
        size: u64,
        prefetchable: bool,
        /// A 64-bit BAR (it used the next slot too).
        wide: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PciDevice {
    pub address: Address,
    pub vendor: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    /// 0 for a device, 1 for a PCI-to-PCI bridge (multi-function bit
    /// removed).
    pub header_type: u8,
    pub multifunction: bool,
    /// Six for devices, two for bridges; `None` where unimplemented (or
    /// the upper half of a 64-bit BAR).
    pub bars: [Option<Bar>; 6],
    /// The legacy IRQ the firmware routed (`0xff`: none).
    pub interrupt_line: u8,
    /// 0 = no interrupt, 1..=4 = INTA#..INTD#.
    pub interrupt_pin: u8,
    /// `(capability id, offset)` from the capabilities list (MSI = 0x05,
    /// PCI Express = 0x10, MSI-X = 0x11, ...).
    pub capabilities: Vec<(u8, u8)>,
}

/// Header type 1: a PCI-to-PCI bridge.
pub const HEADER_BRIDGE: u8 = 1;

impl PciDevice {
    pub fn is_bridge(&self) -> bool {
        self.header_type == HEADER_BRIDGE
    }

    pub fn has_capability(&self, id: u8) -> Option<u8> {
        self.capabilities
            .iter()
            .find(|(capability, _)| *capability == id)
            .map(|&(_, offset)| offset)
    }
}

/// Size the BAR at `offset`, with decoding already off. Returns the BAR
/// and how many slots it used (2 for a 64-bit BAR).
fn size_bar(
    config: &dyn ConfigSpace,
    address: Address,
    offset: u8,
    last_slot: bool,
) -> (Option<Bar>, usize) {
    let original = config.read32(address, offset);
    config.write32(address, offset, u32::MAX);
    let mask = config.read32(address, offset);
    config.write32(address, offset, original);
    if original & 1 == 1 {
        // I/O: bits 2.. hold the base; the upper 16 bits may read as 0.
        let mask = mask & !0x3;
        let size = (!mask).wrapping_add(1) & 0xffff;
        if mask == 0 || size == 0 {
            return (None, 1);
        }
        return (
            Some(Bar::Io {
                port: (original & !0x3) as u16,
                size,
            }),
            1,
        );
    }
    let wide = (original >> 1) & 0x3 == 0x2;
    let prefetchable = original & 0x8 != 0;
    if wide && !last_slot {
        let upper_offset = offset + 4;
        let original_upper = config.read32(address, upper_offset);
        config.write32(address, upper_offset, u32::MAX);
        let mask_upper = config.read32(address, upper_offset);
        config.write32(address, upper_offset, original_upper);
        let mask = (u64::from(mask_upper) << 32) | u64::from(mask & !0xf);
        if mask == 0 {
            return (None, 2);
        }
        let base = (u64::from(original_upper) << 32) | u64::from(original & !0xf);
        return (
            Some(Bar::Memory {
                address: base,
                size: (!mask).wrapping_add(1),
                prefetchable,
                wide: true,
            }),
            2,
        );
    }
    let mask = mask & !0xf;
    if mask == 0 {
        return (None, 1);
    }
    (
        Some(Bar::Memory {
            address: u64::from(original & !0xf),
            size: u64::from((!mask).wrapping_add(1)),
            prefetchable,
            wide: false,
        }),
        1,
    )
}

/// Every BAR of a function. Decoding is off while sizing and the command
/// register is restored afterwards.
fn read_bars(config: &dyn ConfigSpace, address: Address, count: usize) -> [Option<Bar>; 6] {
    let mut bars = [None; 6];
    let command = config.read16(address, COMMAND);
    config.write16(address, COMMAND, command & !(COMMAND_IO | COMMAND_MEMORY));
    let mut slot = 0;
    while slot < count {
        let offset = BAR0 + (slot as u8) * 4;
        let (bar, used) = size_bar(config, address, offset, slot + 1 == count);
        bars[slot] = bar;
        slot += used;
    }
    config.write16(address, COMMAND, command);
    bars
}

/// The capabilities list, guarded against loops and bad offsets.
fn read_capabilities(config: &dyn ConfigSpace, address: Address) -> Vec<(u8, u8)> {
    let mut capabilities = Vec::new();
    if config.read16(address, STATUS) & STATUS_CAPABILITIES == 0 {
        return capabilities;
    }
    let mut offset = config.read8(address, CAPABILITIES) & !0x3;
    // At most this many fit in the 192 bytes after the header.
    for _ in 0..48 {
        if offset < 0x40 {
            break;
        }
        let id = config.read8(address, offset);
        capabilities.push((id, offset));
        offset = config.read8(address, offset + 1) & !0x3;
    }
    capabilities
}

/// The function at `address`, or `None` if the slot is empty.
pub fn read_device(config: &dyn ConfigSpace, address: Address) -> Option<PciDevice> {
    let vendor = config.read16(address, VENDOR_ID);
    if vendor == NO_DEVICE {
        return None;
    }
    let header = config.read8(address, HEADER_TYPE);
    let header_type = header & 0x7f;
    let bar_count = match header_type {
        0 => 6,
        HEADER_BRIDGE => 2,
        // CardBus and unknown layouts: identity only.
        _ => 0,
    };
    Some(PciDevice {
        address,
        vendor,
        device_id: config.read16(address, DEVICE_ID),
        class: config.read8(address, CLASS),
        subclass: config.read8(address, SUBCLASS),
        prog_if: config.read8(address, PROG_IF),
        revision: config.read8(address, REVISION),
        header_type,
        multifunction: header & 0x80 != 0,
        bars: read_bars(config, address, bar_count),
        interrupt_line: config.read8(address, INTERRUPT_LINE),
        interrupt_pin: config.read8(address, INTERRUPT_PIN),
        capabilities: read_capabilities(config, address),
    })
}

/// A human name for a class/subclass.
pub fn class_name(class: u8, subclass: u8) -> &'static str {
    match (class, subclass) {
        (0x01, 0x01) => "IDE controller",
        (0x01, 0x06) => "SATA controller",
        (0x01, 0x08) => "NVMe controller",
        (0x01, _) => "storage controller",
        (0x02, 0x00) => "Ethernet controller",
        (0x02, _) => "network controller",
        (0x03, 0x00) => "VGA controller",
        (0x03, _) => "display controller",
        (0x04, 0x01) => "multimedia audio controller",
        (0x04, 0x03) => "audio device (HD Audio)",
        (0x04, _) => "multimedia controller",
        (0x05, _) => "memory controller",
        (0x06, 0x00) => "host bridge",
        (0x06, 0x01) => "ISA bridge",
        (0x06, 0x04) => "PCI bridge",
        (0x06, _) => "bridge",
        (0x07, _) => "communication controller",
        (0x08, _) => "system peripheral",
        (0x0c, 0x03) => "USB controller",
        (0x0c, 0x05) => "SMBus controller",
        (0x0c, _) => "serial bus controller",
        _ => "device",
    }
}

pub fn vendor_name(vendor: u16) -> Option<&'static str> {
    Some(match vendor {
        0x8086 => "Intel",
        0x1234 => "QEMU/Bochs",
        0x1af4 | 0x1b36 => "Red Hat",
        0x10ec => "Realtek",
        0x10de => "NVIDIA",
        0x1002 => "AMD/ATI",
        0x1022 => "AMD",
        0x15ad => "VMware",
        0x80ee => "VirtualBox",
        _ => return None,
    })
}

impl fmt::Display for Bar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Bar::Io { port, size } => write!(formatter, "io {port:#x}+{size:#x}"),
            Bar::Memory {
                address,
                size,
                prefetchable,
                wide,
            } => write!(
                formatter,
                "mem {address:#x}+{size:#x}{}{}",
                if wide { " 64-bit" } else { "" },
                if prefetchable { " prefetchable" } else { "" }
            ),
        }
    }
}

/// `00:03.0 8086:2415 class 0401 multimedia audio controller [Intel] irq 11 pin A`
impl fmt::Display for PciDevice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {:04x}:{:04x} class {:02x}{:02x} {}",
            self.address,
            self.vendor,
            self.device_id,
            self.class,
            self.subclass,
            class_name(self.class, self.subclass)
        )?;
        if let Some(vendor) = vendor_name(self.vendor) {
            write!(formatter, " [{vendor}]")?;
        }
        if (1..=4).contains(&self.interrupt_pin) {
            write!(
                formatter,
                " pin {}",
                (b'A' + self.interrupt_pin - 1) as char
            )?;
            if self.interrupt_line != 0xff {
                write!(formatter, " irq {}", self.interrupt_line)?;
            }
        }
        Ok(())
    }
}
