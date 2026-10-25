//! PCI: finding devices, so drivers can claim them.
//!
//! ```text
//! pci::init()  scan (config.rs over port I/O, scan.rs, device.rs)
//!   -> DEVICES: every function, its class, BARs, interrupt, capabilities
//!   -> DRIVERS: each driver's `matches` / `probe` (none yet)
//!   -> boot log, /proc/pci, lspci
//! drivers:     pci::find(...), pci::enable(...), pci::map_bar(...)
//! ```
//!
//! `config`, `device` and `scan` are pure and tested in the host model
//! against a fake configuration space. Interrupt routing (MSI, or ACPI
//! `_PRT` for legacy INTx) and PCIe extended configuration space (ECAM)
//! come later; the first drivers poll.

pub mod config;
pub mod device;
pub mod scan;

#[cfg(not(host_model))]
mod kernel {
    use super::config::{
        COMMAND, COMMAND_BUS_MASTER, COMMAND_IO, COMMAND_MEMORY, ConfigSpace, PortConfig,
    };
    use super::device::{Bar, PciDevice};
    use crate::sync::SpinMutex;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::fmt::Write;

    static CONFIG: PortConfig = PortConfig::new();
    static DEVICES: SpinMutex<Vec<PciDevice>> = SpinMutex::new(Vec::new());

    /// A driver for some PCI devices.
    pub struct PciDriver {
        pub name: &'static str,
        pub matches: fn(&PciDevice) -> bool,
        pub probe: fn(&PciDevice) -> Result<(), &'static str>,
    }

    /// Drivers, tried in order; the first that matches claims a device.
    static DRIVERS: &[PciDriver] = &[];

    /// Scan the buses, log what is there, and hand devices to drivers.
    pub fn init() {
        let devices = super::scan::scan(&CONFIG);
        crate::kinfo!("pci: {} functions", devices.len());
        for device in &devices {
            let driver = DRIVERS.iter().find(|driver| (driver.matches)(device));
            let mut line = alloc::format!("pci: {device}");
            for bar in device.bars.iter().flatten() {
                let _ = write!(line, " {bar}");
            }
            match driver {
                Some(driver) => {
                    crate::kinfo!("{} -> {}", line, driver.name);
                    if let Err(error) = (driver.probe)(device) {
                        crate::kerror!("pci: {} {}: {}", device.address, driver.name, error);
                    }
                }
                None => crate::kinfo!("{} (no driver)", line),
            }
        }
        *DEVICES.lock() = devices;
    }

    pub fn devices() -> Vec<PciDevice> {
        DEVICES.lock().clone()
    }

    pub fn find(predicate: impl Fn(&PciDevice) -> bool) -> Option<PciDevice> {
        DEVICES
            .lock()
            .iter()
            .find(|device| predicate(device))
            .cloned()
    }

    pub fn by_id(vendor: u16, device_id: u16) -> Option<PciDevice> {
        find(|device| device.vendor == vendor && device.device_id == device_id)
    }

    pub fn by_class(class: u8, subclass: u8) -> Option<PciDevice> {
        find(|device| device.class == class && device.subclass == subclass)
    }

    /// Which decoders to turn on.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Enable {
        pub io: bool,
        pub memory: bool,
        /// Let the device do DMA (read and write RAM itself).
        pub bus_master: bool,
    }

    /// Turn on a device's I/O and/or memory decoding and bus mastering.
    /// Returns the command register as read back.
    pub fn enable(device: &PciDevice, enable: Enable) -> u16 {
        let mut command = CONFIG.read16(device.address, COMMAND);
        if enable.io {
            command |= COMMAND_IO;
        }
        if enable.memory {
            command |= COMMAND_MEMORY;
        }
        if enable.bus_master {
            command |= COMMAND_BUS_MASTER;
        }
        CONFIG.write16(device.address, COMMAND, command);
        CONFIG.read16(device.address, COMMAND)
    }

    /// Where a driver reaches a BAR's registers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MappedBar {
        Io {
            port: u16,
            size: u32,
        },
        /// Mapped uncached at `address` (kernel virtual).
        Memory {
            address: u64,
            size: u64,
        },
    }

    pub fn map_bar(device: &PciDevice, index: usize) -> Result<MappedBar, &'static str> {
        match device.bars.get(index).copied().flatten() {
            None => Err("no such BAR"),
            Some(Bar::Io { port, size }) => Ok(MappedBar::Io { port, size }),
            Some(Bar::Memory { address, size, .. }) => {
                let mapped = crate::mm::paging::map_mmio(crate::mm::PhysAddr(address), size)?;
                Ok(MappedBar::Memory {
                    address: mapped.0,
                    size,
                })
            }
        }
    }

    /// `/proc/pci`: one line per function, then its BARs indented.
    pub fn proc_text() -> Vec<u8> {
        let mut text = String::new();
        for device in DEVICES.lock().iter() {
            let _ = writeln!(text, "{device}");
            for (index, bar) in device.bars.iter().enumerate() {
                if let Some(bar) = bar {
                    let _ = writeln!(text, "    bar{index} {bar}");
                }
            }
        }
        text.into_bytes()
    }
}

#[cfg(not(host_model))]
pub use kernel::*;
