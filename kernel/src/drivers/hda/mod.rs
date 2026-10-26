//! Intel High Definition Audio: the standard sound hardware of PCs since
//! 2004 (Intel, AMD and NVIDIA controllers; Realtek, Conexant, ... codecs),
//! as a playback card for the sound core.
//!
//! ```text
//! probe (class 04:03)
//!   controller.rs  BAR0 registers, reset, command rings (CORB/RIRB)
//!   codec.rs       per codec: widgets, outputs (pin <- ... <- DAC), routing
//!   stream.rs      one output stream over the core's ring, silence past
//!                  what was submitted
//! ```
//!
//! The first codec with an analog output is used (HDMI-only codecs have
//! none); all its outputs play together. Rates are those every chosen DAC
//! supports (often only 44.1 and 48 kHz); the core resamples the rest.
//! Polled, like the core: no interrupts yet. `codec` is pure and tested in
//! the host model against dumps of real codecs.

pub mod codec;
#[cfg(not(host_model))]
mod controller;
#[cfg(not(host_model))]
mod stream;

#[cfg(not(host_model))]
mod kernel {
    use super::codec::{self, OutputPlan};
    use super::controller::Controller;
    use super::stream::Stream;
    use crate::drivers::pci::{self, Enable, MappedBar, device::PciDevice};
    use crate::mm::dma::DmaAddress;
    use crate::sound::AudioDevice;
    use crate::sound::ring::Status;
    use alloc::boxed::Box;
    use alloc::string::String;

    /// The stream tag the DACs listen for (1..=15; 0 means none).
    const STREAM_TAG: u8 = 1;

    struct Hda {
        controller: Controller,
        plan: OutputPlan,
        stream: Stream,
        format: u16,
    }

    impl AudioDevice for Hda {
        fn name(&self) -> &'static str {
            "Intel HD Audio"
        }

        fn attach(&mut self, ring: DmaAddress) {
            self.stream.attach(ring);
        }

        fn rate_for(&self, hz: u32) -> u32 {
            codec::choose_rate(&self.plan.rates, hz)
        }

        fn set_rate(&mut self, hz: u32) -> u32 {
            let rate = codec::choose_rate(&self.plan.rates, hz);
            // `plan.rates` only holds rates with a format word.
            self.format = codec::stream_format(rate, 2).unwrap_or(self.format);
            rate
        }

        fn start(&mut self, last: usize) {
            codec::set_stream(&mut self.controller, &self.plan, STREAM_TAG, self.format);
            self.stream.start(last, self.format);
        }

        fn submit(&mut self, last: usize) {
            self.stream.submit(last);
        }

        fn status(&mut self) -> Status {
            self.stream.status()
        }

        fn stop(&mut self) {
            self.stream.stop();
        }

        fn describe(&self) -> String {
            self.plan.describe()
        }
    }

    /// The `pci::DRIVERS` probe.
    pub fn probe(device: &PciDevice) -> Result<(), &'static str> {
        pci::enable(
            device,
            Enable {
                io: false,
                memory: true,
                bus_master: true,
            },
        );
        let MappedBar::Memory { address, .. } = pci::map_bar(device, 0)? else {
            return Err("expected a memory BAR");
        };
        let mut controller = Controller::new(address)?;
        if controller.output_streams == 0 {
            return Err("no output streams");
        }
        let mut chosen = None;
        let present = controller.codecs;
        for address in (0..15).filter(|bit| present & (1 << bit) != 0) {
            match codec::discover(&mut controller, address) {
                Ok(found) => match codec::plan_outputs(&found) {
                    Ok(plan) => {
                        crate::kinfo!(
                            "hda: codec {} {:08x}: {} outputs",
                            address,
                            found.vendor,
                            plan.outputs.len()
                        );
                        chosen.get_or_insert((found, plan));
                    }
                    Err(error) => {
                        crate::kinfo!("hda: codec {} {:08x}: {:?}", address, found.vendor, error)
                    }
                },
                Err(error) => crate::kwarn!("hda: codec {}: {:?}", address, error),
            }
        }
        let (found, plan) = chosen.ok_or("no codec with an analog output")?;
        codec::configure(&mut controller, &found, &plan);
        let stream = Stream::new(
            controller.registers,
            controller.output_stream(0),
            STREAM_TAG,
        )?;
        let format = codec::stream_format(48_000, 2).expect("48 kHz has a format");
        crate::sound::register(Box::new(Hda {
            controller,
            plan,
            stream,
            format,
        }))
    }

    /// HD Audio controllers (multimedia, audio device: class 04:03).
    pub fn matches(device: &PciDevice) -> bool {
        (device.class, device.subclass) == (0x04, 0x03)
    }
}

#[cfg(not(host_model))]
pub use kernel::{matches, probe};
