//! MSI (Message Signalled Interrupts): instead of raising an interrupt
//! line, the device writes a small message to a special address, and that
//! write is the interrupt. No IOAPIC routing or ACPI `_PRT` needed.
//!
//! The capability (id 0x05) comes in four layouts; where the data register
//! sits depends on whether the address is 64-bit:
//!
//! ```text
//!          32-bit address        64-bit address
//! +0x00    id, next, control     id, next, control
//! +0x04    address               address (low)
//! +0x08    data                  address (high)
//! +0x0c    mask *                data
//! +0x10    pending *             mask *
//! +0x14                          pending *
//!          * only with per-vector masking (control bit 8)
//! ```
//!
//! Control: bit 0 enable, bits 1-3 vectors the device can use (log2),
//! bits 4-6 vectors enabled (log2), bit 7 64-bit, bit 8 per-vector masking.
//!
//! The message is an x86 Local APIC interrupt: address `0xFEE0_0000` with
//! the destination APIC ID in bits 12-19 (physical destination), data the
//! vector (fixed delivery, edge-triggered). One vector per device; MSI is
//! switched on by the last write, once everything else is in place.

use super::config::{Address, COMMAND, COMMAND_INTX_DISABLE, ConfigSpace};

const CONTROL_ENABLE: u16 = 1 << 0;
const CONTROL_ENABLED_VECTORS: u16 = 0x7 << 4;
const CONTROL_64_BIT: u16 = 1 << 7;
const CONTROL_PER_VECTOR_MASK: u16 = 1 << 8;

/// The x86 MSI address window.
pub const MESSAGE_BASE: u32 = 0xfee0_0000;

/// A device's MSI capability and its layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsiCapability {
    /// The capability's offset in configuration space.
    pub offset: u8,
    pub wide: bool,
    pub per_vector_mask: bool,
    /// How many vectors the device could use (we enable one).
    pub vectors: u8,
}

impl MsiCapability {
    /// The capability at `offset`.
    pub fn read(config: &dyn ConfigSpace, address: Address, offset: u8) -> Self {
        let control = config.read16(address, offset + 2);
        Self {
            offset,
            wide: control & CONTROL_64_BIT != 0,
            per_vector_mask: control & CONTROL_PER_VECTOR_MASK != 0,
            vectors: 1 << ((control >> 1) & 0x7).min(5),
        }
    }

    fn control(&self) -> u8 {
        self.offset + 2
    }

    pub fn address_low(&self) -> u8 {
        self.offset + 4
    }

    pub fn address_high(&self) -> Option<u8> {
        self.wide.then_some(self.offset + 8)
    }

    pub fn data(&self) -> u8 {
        self.offset + if self.wide { 0x0c } else { 0x08 }
    }

    pub fn mask(&self) -> Option<u8> {
        self.per_vector_mask
            .then_some(self.offset + if self.wide { 0x10 } else { 0x0c })
    }

    pub fn is_enabled(&self, config: &dyn ConfigSpace, address: Address) -> bool {
        config.read16(address, self.control()) & CONTROL_ENABLE != 0
    }
}

/// The message address that interrupts the Local APIC `destination`.
pub fn message_address(destination: u8) -> u32 {
    MESSAGE_BASE | (u32::from(destination) << 12)
}

/// Make the device send `vector` to `destination` and switch MSI on (and
/// legacy INTx off).
pub fn program(
    config: &dyn ConfigSpace,
    address: Address,
    capability: &MsiCapability,
    destination: u8,
    vector: u8,
) {
    // Off while the message changes: a half-written one must never fire.
    let control = config.read16(address, capability.control());
    let control = control & !(CONTROL_ENABLE | CONTROL_ENABLED_VECTORS);
    config.write16(address, capability.control(), control);

    config.write32(
        address,
        capability.address_low(),
        message_address(destination),
    );
    if let Some(high) = capability.address_high() {
        config.write32(address, high, 0);
    }
    config.write16(address, capability.data(), u16::from(vector));
    if let Some(mask) = capability.mask() {
        let bits = config.read32(address, mask);
        config.write32(address, mask, bits & !1);
    }

    let command = config.read16(address, COMMAND);
    config.write16(address, COMMAND, command | COMMAND_INTX_DISABLE);
    // Last: one vector (enabled-vectors field 0), MSI on.
    config.write16(address, capability.control(), control | CONTROL_ENABLE);
}

/// Switch MSI off and give the device its INTx line back.
pub fn disable(config: &dyn ConfigSpace, address: Address, capability: &MsiCapability) {
    let control = config.read16(address, capability.control());
    config.write16(address, capability.control(), control & !CONTROL_ENABLE);
    let command = config.read16(address, COMMAND);
    config.write16(address, COMMAND, command & !COMMAND_INTX_DISABLE);
}
