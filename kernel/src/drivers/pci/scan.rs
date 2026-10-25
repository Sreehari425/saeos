//! Walking the PCI hierarchy.
//!
//! ```text
//! root buses: bus 0, plus bus n for each function n of a multi-function
//!             host bridge at 00:00.0 (several host controllers)
//! each bus:   devices 0..32; function 0 decides whether functions 1..8
//!             exist (multi-function bit) — probing them otherwise finds
//!             "ghost" copies on some chipsets
//! bridges:    class 06:04 with header type 1 lead to their secondary bus
//! ```
//!
//! Every bus is scanned once (bad firmware can make bridges point in a
//! loop), and the result is in bus, device, function order.

use super::config::{Address, ConfigSpace, SECONDARY_BUS};
use super::device::{PciDevice, read_device};
use alloc::vec::Vec;

/// Every function on every reachable bus.
pub fn scan(config: &dyn ConfigSpace) -> Vec<PciDevice> {
    let mut devices = Vec::new();
    let mut visited = [false; 256];
    let mut pending: Vec<u8> = alloc::vec![0];
    // Several host controllers: 00:00.f is the host bridge for bus f.
    if let Some(host) = read_device(config, Address::new(0, 0, 0))
        && host.multifunction
    {
        for function in 1..8 {
            if read_device(config, Address::new(0, 0, function)).is_some() {
                pending.push(function);
            }
        }
    }
    while let Some(bus) = pending.pop() {
        if core::mem::replace(&mut visited[usize::from(bus)], true) {
            continue;
        }
        for device in 0..32 {
            let Some(first) = read_device(config, Address::new(bus, device, 0)) else {
                continue;
            };
            let functions = if first.multifunction { 8 } else { 1 };
            let mut found = alloc::vec![first];
            for function in 1..functions {
                found.extend(read_device(config, Address::new(bus, device, function)));
            }
            for function in found {
                if function.is_bridge() && function.class == 0x06 && function.subclass == 0x04 {
                    let secondary = config.read8(function.address, SECONDARY_BUS);
                    // 0 (unconfigured) or the bridge's own bus lead nowhere.
                    if secondary != 0 && secondary != bus {
                        pending.push(secondary);
                    }
                }
                devices.push(function);
            }
        }
    }
    devices.sort_by_key(|device| device.address);
    devices
}
