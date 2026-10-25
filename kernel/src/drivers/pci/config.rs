//! PCI configuration space: every function's 256-byte register file,
//! reached through a [`ConfigSpace`]. The kernel uses the legacy port
//! interface (`0xCF8`/`0xCFC`, "configuration mechanism #1"); the host model
//! uses a fake one, so everything above this trait is tested on the host.
//!
//! Hardware only does aligned 32-bit accesses here: the narrower helpers
//! read or modify the containing dword.

use core::fmt;

/// A function's location: bus 0..=255, device 0..32, function 0..8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Address {
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self {
            bus,
            device,
            function,
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:02x}:{:02x}.{}",
            self.bus, self.device, self.function
        )
    }
}

/// Access to configuration space. `offset` is a byte offset; implementors
/// only ever see 4-byte-aligned offsets.
pub trait ConfigSpace {
    fn read32(&self, address: Address, offset: u8) -> u32;
    fn write32(&self, address: Address, offset: u8, value: u32);

    fn read16(&self, address: Address, offset: u8) -> u16 {
        (self.read32(address, offset & !3) >> ((offset & 2) * 8)) as u16
    }

    fn read8(&self, address: Address, offset: u8) -> u8 {
        (self.read32(address, offset & !3) >> ((offset & 3) * 8)) as u8
    }

    fn write16(&self, address: Address, offset: u8, value: u16) {
        let aligned = offset & !3;
        let shift = (offset & 2) * 8;
        let old = self.read32(address, aligned);
        let new = (old & !(0xffff << shift)) | (u32::from(value) << shift);
        self.write32(address, aligned, new);
    }
}

// Standard header offsets.
pub const VENDOR_ID: u8 = 0x00;
pub const DEVICE_ID: u8 = 0x02;
pub const COMMAND: u8 = 0x04;
pub const STATUS: u8 = 0x06;
pub const REVISION: u8 = 0x08;
pub const PROG_IF: u8 = 0x09;
pub const SUBCLASS: u8 = 0x0a;
pub const CLASS: u8 = 0x0b;
pub const HEADER_TYPE: u8 = 0x0e;
pub const BAR0: u8 = 0x10;
/// Bridges (header type 1): the bus behind them.
pub const SECONDARY_BUS: u8 = 0x19;
pub const CAPABILITIES: u8 = 0x34;
pub const INTERRUPT_LINE: u8 = 0x3c;
pub const INTERRUPT_PIN: u8 = 0x3d;

// Command register bits.
pub const COMMAND_IO: u16 = 1 << 0;
pub const COMMAND_MEMORY: u16 = 1 << 1;
pub const COMMAND_BUS_MASTER: u16 = 1 << 2;
/// Status register: the capabilities list at 0x34 is valid.
pub const STATUS_CAPABILITIES: u16 = 1 << 4;

/// A vendor ID read from an empty slot.
pub const NO_DEVICE: u16 = 0xffff;

#[cfg(not(host_model))]
mod port {
    use super::{Address, ConfigSpace};
    use crate::arch::x86_64::cpu;
    use crate::sync::IrqSpinMutex;

    const CONFIG_ADDRESS: u16 = 0xcf8;
    const CONFIG_DATA: u16 = 0xcfc;

    /// Configuration mechanism #1. The address and data accesses form one
    /// operation, so a lock keeps two of them from interleaving.
    pub struct PortConfig {
        lock: IrqSpinMutex<()>,
    }

    impl PortConfig {
        pub const fn new() -> Self {
            Self {
                lock: IrqSpinMutex::new(()),
            }
        }
    }

    impl Default for PortConfig {
        fn default() -> Self {
            Self::new()
        }
    }

    fn select(address: Address, offset: u8) -> u32 {
        0x8000_0000
            | (u32::from(address.bus) << 16)
            | (u32::from(address.device & 0x1f) << 11)
            | (u32::from(address.function & 0x7) << 8)
            | u32::from(offset & 0xfc)
    }

    impl ConfigSpace for PortConfig {
        fn read32(&self, address: Address, offset: u8) -> u32 {
            let _guard = self.lock.lock();
            unsafe {
                cpu::outl(CONFIG_ADDRESS, select(address, offset));
                cpu::inl(CONFIG_DATA)
            }
        }

        fn write32(&self, address: Address, offset: u8, value: u32) {
            let _guard = self.lock.lock();
            unsafe {
                cpu::outl(CONFIG_ADDRESS, select(address, offset));
                cpu::outl(CONFIG_DATA, value);
            }
        }
    }
}

#[cfg(not(host_model))]
pub use port::PortConfig;
