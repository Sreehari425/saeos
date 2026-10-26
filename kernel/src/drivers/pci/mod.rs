//! PCI: finding devices, so drivers can claim them.
//!
//! ```text
//! pci::init()  scan (config.rs over port I/O, scan.rs, device.rs)
//!   -> DEVICES: every function, its class, BARs, interrupt, capabilities
//!   -> DRIVERS: each driver's `matches` / `probe` (AC'97, HD Audio)
//!   -> boot log, /proc/pci, lspci
//! drivers:     pci::find(...), pci::enable(...), pci::map_bar(...),
//!              pci::enable_msi(...) for interrupts
//! ```
//!
//! `config`, `device`, `scan` and `msi` are pure and tested in the host
//! model against a fake configuration space. Interrupts are MSI only:
//! legacy INTx would need ACPI `_PRT` routing. MSI-X and PCIe extended
//! configuration space (ECAM) come later.

pub mod config;
pub mod device;
pub mod msi;
pub mod scan;

#[cfg(not(host_model))]
mod kernel {
    use super::config::{
        Address, CAPABILITY_MSI, COMMAND, COMMAND_BUS_MASTER, COMMAND_IO, COMMAND_MEMORY,
        ConfigSpace, PortConfig,
    };
    use super::device::{Bar, PciDevice};
    use super::msi::{self, MsiCapability};
    use crate::arch::x86_64::{interrupt_controller, vectors};
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
    static DRIVERS: &[PciDriver] = &[
        PciDriver {
            name: "ac97",
            matches: crate::drivers::ac97::matches,
            probe: crate::drivers::ac97::probe,
        },
        PciDriver {
            name: "hda",
            matches: crate::drivers::hda::matches,
            probe: crate::drivers::hda::probe,
        },
    ];

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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MsiError {
        /// The device has no MSI capability.
        NoCapability,
        /// The legacy PIC is in use: nothing would deliver the message.
        NotApicMode,
        NoFreeVector,
    }

    /// A device's MSI, switched on. Dropping it switches MSI off on the
    /// device, then frees the vector.
    pub struct Msi {
        address: Address,
        capability: MsiCapability,
        vector: u8,
    }

    impl Msi {
        pub fn vector(&self) -> u8 {
            self.vector
        }
    }

    impl Drop for Msi {
        fn drop(&mut self) {
            // Off first: a vector freed while the device can still send it
            // would land on its next owner.
            msi::disable(&CONFIG, self.address, &self.capability);
            vectors::free(self.vector);
        }
    }

    /// Give `device` a vector of its own and have it interrupt with MSI;
    /// `handler` runs in interrupt context (no allocation, no blocking:
    /// acknowledge the device and wake a task).
    pub fn enable_msi(
        device: &PciDevice,
        owner: &'static str,
        handler: vectors::Handler,
    ) -> Result<Msi, MsiError> {
        let offset = device
            .has_capability(CAPABILITY_MSI)
            .ok_or(MsiError::NoCapability)?;
        let destination = interrupt_controller::msi_destination().ok_or(MsiError::NotApicMode)?;
        let capability = MsiCapability::read(&CONFIG, device.address, offset);
        let vector = vectors::allocate(owner, handler).ok_or(MsiError::NoFreeVector)?;
        msi::program(&CONFIG, device.address, &capability, destination, vector);
        Ok(Msi {
            address: device.address,
            capability,
            vector,
        })
    }

    /// Whether MSI is on for `device` (read from the device itself).
    pub fn msi_enabled(device: &PciDevice) -> bool {
        device.has_capability(CAPABILITY_MSI).is_some_and(|offset| {
            MsiCapability::read(&CONFIG, device.address, offset).is_enabled(&CONFIG, device.address)
        })
    }

    /// `/proc/pci`: one line per function, then its BARs indented.
    pub fn proc_text() -> Vec<u8> {
        let mut text = String::new();
        for device in DEVICES.lock().iter() {
            let _ = write!(text, "{device}");
            if device.has_capability(CAPABILITY_MSI).is_some() {
                text.push_str(if msi_enabled(device) {
                    " msi on"
                } else {
                    " msi"
                });
            }
            text.push('\n');
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
