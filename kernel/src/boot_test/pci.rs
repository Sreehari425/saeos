//! PCI enumeration on QEMU's machines. `boot-test` runs add an AC'97
//! sound card; the `q35` runs (`saeos.test.pci=q35`) put it behind a
//! PCI-to-PCI bridge, so it is found on bus 1.

use crate::drivers::pci::{self, Enable, MappedBar, config::COMMAND_BUS_MASTER, device::Bar};
use crate::serial_println;

fn check(name: &str, passed: bool) -> bool {
    if !passed {
        serial_println!("boot-test: pci: {} FAILED", name);
    }
    passed
}

pub fn pci_test() -> bool {
    let q35 = crate::cmdline::params().get("saeos.test.pci") == Some("q35");
    let host = pci::find(|device| device.address == pci::config::Address::new(0, 0, 0));
    let host_ok = match (&host, q35) {
        (Some(host), true) => (host.vendor, host.device_id) == (0x8086, 0x29c0),
        (Some(host), false) => {
            (host.vendor, host.device_id) == (0x8086, 0x1237)
                && pci::by_id(0x8086, 0x7000).is_some()
        }
        (None, _) => false,
    };
    // The VGA device, with its framebuffer as a memory BAR.
    let display_ok = pci::by_id(0x1234, 0x1111).is_some_and(|display| {
        display.class == 0x03 && matches!(display.bars[0], Some(Bar::Memory { .. }))
    });
    let audio = pci::by_id(0x8086, 0x2415);
    let audio_ok = audio.as_ref().is_some_and(|audio| {
        (audio.class, audio.subclass) == (0x04, 0x01)
            && matches!(audio.bars[0], Some(Bar::Io { .. }))
            && matches!(audio.bars[1], Some(Bar::Io { .. }))
            && (1..=4).contains(&audio.interrupt_pin)
            // On q35 it sits behind the bridge.
            && (audio.address.bus != 0) == q35
    });
    // What a driver will do: turn on decoding and DMA, then reach the
    // registers.
    let enable_ok = audio.as_ref().is_some_and(|audio| {
        let command = pci::enable(
            audio,
            Enable {
                io: true,
                memory: false,
                bus_master: true,
            },
        );
        command & COMMAND_BUS_MASTER != 0
            && matches!(pci::map_bar(audio, 0), Ok(MappedBar::Io { size, .. }) if size >= 0x100)
            && pci::map_bar(audio, 5).is_err()
    });
    let proc_text = alloc::string::String::from_utf8(pci::proc_text()).unwrap_or_default();
    let proc_ok = proc_text.contains("8086:2415") && proc_text.contains("    bar0 io ");
    if !(host_ok && audio_ok) {
        serial_println!("boot-test: pci devices:\n{}", proc_text);
    }
    check("host bridge", host_ok)
        & check("display", display_ok)
        & check("AC'97 audio", audio_ok)
        & check("enable + map_bar", enable_ok)
        & check("/proc/pci", proc_ok)
}
