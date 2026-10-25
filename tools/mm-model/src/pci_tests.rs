//! PCI enumeration against a fake configuration space that behaves like
//! hardware where it matters: BARs keep only their writable bits (so
//! sizing works), empty slots read 0xFFFF, and decoding during a BAR write
//! is recorded.

use crate::pci::config::{Address, COMMAND_IO, COMMAND_MEMORY, ConfigSpace};
use crate::pci::device::{Bar, read_device};
use crate::pci::scan::scan;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

#[derive(Clone)]
struct Function {
    config: [u32; 64],
    /// Writable bits of each BAR register (size masks).
    bar_masks: [u32; 6],
}

#[derive(Default)]
struct FakeConfig {
    functions: RefCell<BTreeMap<Address, Function>>,
    /// Set if a BAR was written while I/O or memory decoding was on.
    decoded_while_sizing: Cell<bool>,
    /// Functions the scan read (to catch probing that should not happen).
    reads: RefCell<Vec<Address>>,
}

impl ConfigSpace for FakeConfig {
    fn read32(&self, address: Address, offset: u8) -> u32 {
        assert_eq!(offset % 4, 0, "unaligned config access");
        self.reads.borrow_mut().push(address);
        self.functions
            .borrow()
            .get(&address)
            .map_or(u32::MAX, |function| {
                function.config[usize::from(offset / 4)]
            })
    }

    fn write32(&self, address: Address, offset: u8, value: u32) {
        assert_eq!(offset % 4, 0, "unaligned config access");
        let mut functions = self.functions.borrow_mut();
        let Some(function) = functions.get_mut(&address) else {
            return;
        };
        let index = usize::from(offset / 4);
        if (4..10).contains(&index) {
            let command = function.config[1] as u16;
            if command & (COMMAND_IO | COMMAND_MEMORY) != 0 {
                self.decoded_while_sizing.set(true);
            }
            let mask = function.bar_masks[index - 4];
            // Read-only low bits (type) stay; only the mask is writable.
            function.config[index] = (function.config[index] & !mask) | (value & mask);
        } else {
            function.config[index] = value;
        }
    }
}

const IO_BAR: u32 = 0x1;
const MEMORY_64: u32 = 0x4;
const PREFETCHABLE: u32 = 0x8;

struct Builder {
    function: Function,
}

impl Builder {
    fn new(vendor: u16, device: u16, class: u8, subclass: u8) -> Self {
        let mut config = [0u32; 64];
        config[0] = u32::from(vendor) | (u32::from(device) << 16);
        config[2] = (u32::from(class) << 24) | (u32::from(subclass) << 16) | 0x01;
        // Decoding on, as firmware leaves it.
        config[1] = u32::from(COMMAND_IO | COMMAND_MEMORY);
        Self {
            function: Function {
                config,
                bar_masks: [0; 6],
            },
        }
    }

    fn header(mut self, header: u8) -> Self {
        self.function.config[3] |= u32::from(header) << 16;
        self
    }

    fn io_bar(mut self, slot: usize, port: u16, size: u32) -> Self {
        self.function.config[4 + slot] = u32::from(port) | IO_BAR;
        // I/O BARs: the upper 16 bits read as zero on many devices.
        self.function.bar_masks[slot] = !(size - 1) & 0xfffc;
        self
    }

    fn memory_bar(mut self, slot: usize, address: u32, size: u32, prefetchable: bool) -> Self {
        let flags = if prefetchable { PREFETCHABLE } else { 0 };
        self.function.config[4 + slot] = address | flags;
        self.function.bar_masks[slot] = !(size - 1) & !0xf;
        self
    }

    fn memory64_bar(mut self, slot: usize, address: u64, size: u64) -> Self {
        let mask = !(size - 1);
        self.function.config[4 + slot] = (address as u32) | MEMORY_64 | PREFETCHABLE;
        self.function.config[5 + slot] = (address >> 32) as u32;
        self.function.bar_masks[slot] = (mask as u32) & !0xf;
        self.function.bar_masks[slot + 1] = (mask >> 32) as u32;
        self
    }

    fn secondary_bus(mut self, bus: u8) -> Self {
        self.function.config[6] = u32::from(bus) << 8;
        self.function.bar_masks = [0; 6];
        self
    }

    fn interrupt(mut self, line: u8, pin: u8) -> Self {
        self.function.config[15] = u32::from(line) | (u32::from(pin) << 8);
        self
    }

    /// A capabilities list: `(offset, id, next)`.
    fn capabilities(mut self, first: u8, list: &[(u8, u8, u8)]) -> Self {
        self.function.config[1] |= (1 << 4) << 16;
        self.function.config[13] = u32::from(first);
        for &(offset, id, next) in list {
            let index = usize::from(offset / 4);
            let shift = (offset % 4) * 8;
            self.function.config[index] |= (u32::from(id) | (u32::from(next) << 8)) << shift;
        }
        self
    }
}

impl FakeConfig {
    fn add(&self, address: Address, builder: Builder) {
        self.functions
            .borrow_mut()
            .insert(address, builder.function);
    }
}

fn host_bridge() -> Builder {
    Builder::new(0x8086, 0x1237, 0x06, 0x00)
}

fn pci_bridge(secondary: u8) -> Builder {
    Builder::new(0x8086, 0x244e, 0x06, 0x04)
        .header(0x01)
        .secondary_bus(secondary)
}

#[test]
fn bars_are_decoded_and_sized_with_decoding_off() {
    let config = FakeConfig::default();
    let address = Address::new(0, 4, 0);
    config.add(
        address,
        Builder::new(0x8086, 0x2415, 0x04, 0x01)
            .io_bar(0, 0xc000, 0x100)
            .memory_bar(1, 0xfebf_0000, 0x1000, false)
            .memory64_bar(2, 0x8_0000_0000, 256 << 20)
            .interrupt(11, 1),
    );
    let before = config.functions.borrow()[&address].config;
    let device = read_device(&config, address).unwrap();
    assert_eq!(
        device.bars,
        [
            Some(Bar::Io {
                port: 0xc000,
                size: 0x100
            }),
            Some(Bar::Memory {
                address: 0xfebf_0000,
                size: 0x1000,
                prefetchable: false,
                wide: false
            }),
            Some(Bar::Memory {
                address: 0x8_0000_0000,
                size: 256 << 20,
                prefetchable: true,
                wide: true
            }),
            None, // the upper half of the 64-bit BAR
            None,
            None,
        ]
    );
    assert_eq!((device.interrupt_line, device.interrupt_pin), (11, 1));
    assert_eq!(
        (
            device.class,
            device.subclass,
            device.prog_if,
            device.revision
        ),
        (0x04, 0x01, 0x00, 0x01)
    );
    assert!(
        !config.decoded_while_sizing.get(),
        "decoding must be off while sizing"
    );
    assert_eq!(
        config.functions.borrow()[&address].config,
        before,
        "every BAR and the command register are restored"
    );
    assert_eq!(
        device.to_string(),
        "00:04.0 8086:2415 class 0401 multimedia audio controller [Intel] pin A irq 11"
    );
}

#[test]
fn absent_slots_and_unaligned_fields() {
    let config = FakeConfig::default();
    assert!(read_device(&config, Address::new(0, 7, 0)).is_none());
    let address = Address::new(0, 1, 0);
    config.add(address, host_bridge());
    // A 16-bit field in the upper half of a dword.
    assert_eq!(config.read16(address, 0x02), 0x1237);
    assert_eq!(config.read8(address, 0x0b), 0x06);
    config.write16(address, 0x04, 0x0007);
    assert_eq!(config.read16(address, 0x04), 0x0007);
    assert_eq!(config.read16(address, 0x06), 0, "the other half is kept");
}

#[test]
fn functions_follow_the_multifunction_bit() {
    let config = FakeConfig::default();
    config.add(Address::new(0, 0, 0), host_bridge());
    // A multi-function device with functions 0, 1 and 3.
    config.add(
        Address::new(0, 1, 0),
        Builder::new(0x8086, 0x7000, 0x06, 0x01).header(0x80),
    );
    config.add(
        Address::new(0, 1, 1),
        Builder::new(0x8086, 0x7010, 0x01, 0x01),
    );
    config.add(
        Address::new(0, 1, 3),
        Builder::new(0x8086, 0x7113, 0x06, 0x80),
    );
    // A single-function device that answers on every function number
    // (some chipsets do): only function 0 is real.
    for function in 0..8 {
        config.add(
            Address::new(0, 2, function),
            Builder::new(0x1234, 0x1111, 0x03, 0x00),
        );
    }
    let found: Vec<String> = scan(&config)
        .iter()
        .map(|device| device.address.to_string())
        .collect();
    assert_eq!(
        found,
        ["00:00.0", "00:01.0", "00:01.1", "00:01.3", "00:02.0"]
    );
    assert!(
        !config.reads.borrow().contains(&Address::new(0, 2, 1)),
        "functions of a single-function device are not probed"
    );
}

#[test]
fn bridges_lead_to_secondary_buses_without_looping() {
    let config = FakeConfig::default();
    config.add(Address::new(0, 0, 0), host_bridge());
    config.add(Address::new(0, 1, 0), pci_bridge(1));
    config.add(Address::new(1, 0, 0), pci_bridge(2));
    config.add(
        Address::new(2, 5, 0),
        Builder::new(0x8086, 0x2415, 0x04, 0x01),
    );
    // A bridge on bus 2 pointing back at bus 1: must not loop.
    config.add(Address::new(2, 6, 0), pci_bridge(1));
    // A bridge on bus 2 pointing at its own bus or unconfigured (0).
    config.add(Address::new(2, 7, 0), pci_bridge(2));
    config.add(Address::new(2, 8, 0), pci_bridge(0));
    let found: Vec<String> = scan(&config)
        .iter()
        .map(|device| device.address.to_string())
        .collect();
    assert_eq!(
        found,
        [
            "00:00.0", "00:01.0", "01:00.0", "02:05.0", "02:06.0", "02:07.0", "02:08.0"
        ]
    );
    let bridge = read_device(&config, Address::new(0, 1, 0)).unwrap();
    assert!(bridge.is_bridge());
    assert_eq!(bridge.bars, [None; 6], "a bridge without BARs");
}

#[test]
fn a_multifunction_host_bridge_adds_root_buses() {
    let config = FakeConfig::default();
    config.add(Address::new(0, 0, 0), host_bridge().header(0x80));
    config.add(Address::new(0, 0, 1), host_bridge());
    config.add(
        Address::new(1, 3, 0),
        Builder::new(0x10ec, 0x8139, 0x02, 0x00),
    );
    let found: Vec<String> = scan(&config)
        .iter()
        .map(|device| device.address.to_string())
        .collect();
    assert_eq!(found, ["00:00.0", "00:00.1", "01:03.0"]);
}

#[test]
fn capabilities_are_listed_and_cycles_stop() {
    let config = FakeConfig::default();
    let address = Address::new(0, 3, 0);
    // MSI at 0x50 -> PCIe at 0x60 -> end.
    config.add(
        address,
        Builder::new(0x8086, 0x2668, 0x04, 0x03)
            .capabilities(0x50, &[(0x50, 0x05, 0x60), (0x60, 0x10, 0x00)]),
    );
    let device = read_device(&config, address).unwrap();
    assert_eq!(device.capabilities, [(0x05, 0x50), (0x10, 0x60)]);
    assert_eq!(device.has_capability(0x05), Some(0x50));
    assert_eq!(device.has_capability(0x11), None);
    // A list that points back at itself is cut off, not followed forever.
    let looping = Address::new(0, 4, 0);
    config.add(
        looping,
        Builder::new(0x8086, 1, 0x04, 0x03).capabilities(0x40, &[(0x40, 0x05, 0x40)]),
    );
    assert_eq!(
        read_device(&config, looping).unwrap().capabilities.len(),
        48
    );
    // A pointer into the standard header (below 0x40) ends the list.
    let bad = Address::new(0, 5, 0);
    config.add(
        bad,
        Builder::new(0x8086, 2, 0x04, 0x03).capabilities(0x10, &[]),
    );
    assert!(read_device(&config, bad).unwrap().capabilities.is_empty());
    // No capabilities bit in the status register: no list at all.
    let none = Address::new(0, 6, 0);
    config.add(none, Builder::new(0x8086, 3, 0x04, 0x03));
    assert!(read_device(&config, none).unwrap().capabilities.is_empty());
}
