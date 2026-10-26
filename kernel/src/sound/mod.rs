//! Sound: one playback card behind `/dev/dsp` (the OSS interface).
//!
//! ```text
//! write(/dev/dsp) ─▶ DspHandle ─▶ format::Converter (u8/s16, mono/stereo
//!                                  ─▶ s16 stereo) into the ring's fill period
//!                                ring::Ring: which periods are free, when to
//!                                  submit, restart after an underrun
//!                    ─▶ dyn AudioDevice (drivers/ac97.rs; later HD Audio)
//! ```
//!
//! The core owns the ring buffer and all the bookkeeping; a driver only
//! points its card at the ring and starts, extends, polls and stops it.
//! Its one obligation: **never play past the last submitted period** (AC'97
//! stops there by itself; an HD Audio driver will stop its stream from the
//! period interrupt). Waiting for the card sleeps about half a period and
//! polls; a driver with interrupts can later wake the writer instead.
//!
//! `ioctl`s (OSS numbers, `soundcard.h`): `SNDCTL_DSP_SPEED` (returns the
//! rate the card really uses), `SNDCTL_DSP_CHANNELS`, `SNDCTL_DSP_SETFMT`
//! (`AFMT_U8`, `AFMT_S16_LE`; others get S16 back), `SNDCTL_DSP_SYNC` (wait
//! until everything written has played), `SNDCTL_DSP_RESET` (stop now).
//! Each open starts at 48 kHz, S16, stereo. One opener at a time
//! (`EBUSY`). Unlike OSS, `close` does not wait: queued audio plays out on
//! its own, so call `SNDCTL_DSP_SYNC` to know it has.
//!
//! `format` and `ring` are pure and tested in the host model.

pub mod format;
pub mod ring;

pub const SNDCTL_DSP_RESET: u64 = 0x5000;
pub const SNDCTL_DSP_SYNC: u64 = 0x5001;
pub const SNDCTL_DSP_SPEED: u64 = 0xc004_5002;
pub const SNDCTL_DSP_SETFMT: u64 = 0xc004_5005;
pub const SNDCTL_DSP_CHANNELS: u64 = 0xc004_5006;

#[cfg(not(host_model))]
mod kernel {
    use super::format::{AFMT_QUERY, Converter, Format, OUTPUT_FRAME, Sample};
    use super::ring::{Action, PERIOD_BYTES, RING_BYTES, Ring, Status};
    use super::{
        SNDCTL_DSP_CHANNELS, SNDCTL_DSP_RESET, SNDCTL_DSP_SETFMT, SNDCTL_DSP_SPEED, SNDCTL_DSP_SYNC,
    };
    use crate::fs::{Errno, FsResult, Inode, InodeKind, Stat};
    use crate::mm::dma::{DmaAddress, DmaBuffer, DmaConstraints};
    use crate::sync::{IrqSpinMutex, SpinMutex};
    use alloc::boxed::Box;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::any::Any;

    /// A playback card, as the core drives it.
    pub trait AudioDevice: Send {
        fn name(&self) -> &'static str;
        /// Point the card at the ring: `ring::PERIODS` periods of
        /// `ring::PERIOD_BYTES`, 16-bit stereo, at `ring`. Called once.
        fn attach(&mut self, ring: DmaAddress);
        /// Set the sample rate; returns the rate the card really uses.
        /// Only called while the card is stopped.
        fn set_rate(&mut self, hz: u32) -> u32;
        /// Play from period 0 up to `last`, then stop.
        fn start(&mut self, last: usize);
        /// Extend playback up to `last`.
        fn submit(&mut self, last: usize);
        fn status(&self) -> Status;
        /// Stop at once.
        fn stop(&mut self);
    }

    struct Card {
        device: Box<dyn AudioDevice>,
        buffer: DmaBuffer,
        ring: Ring,
        /// The rate the card is set to.
        rate: u32,
        open: bool,
        underruns: u64,
    }

    impl Card {
        fn apply(&mut self, action: Action) {
            match action {
                Action::None => {}
                Action::Start { last } => {
                    self.buffer.sync_for_device();
                    self.device.start(last);
                }
                Action::Submit { last } => {
                    self.buffer.sync_for_device();
                    self.device.submit(last);
                }
            }
        }

        /// Catch up with the card. `expect_halt`: a drain is waiting for
        /// the end, so stopping is not an underrun.
        fn poll(&mut self, expect_halt: bool) {
            let update = self.ring.update(self.device.status());
            if update.halted && !expect_halt {
                self.underruns += 1;
            }
            if let Some((from, length)) = update.relocate {
                self.buffer
                    .as_mut_slice()
                    .copy_within(from..from + length, 0);
            }
        }

        /// How long to wait for the card to finish some of the queue.
        fn wait_ms(&self) -> u64 {
            let frames = (PERIOD_BYTES / OUTPUT_FRAME) as u64;
            (frames * 1000 / u64::from(self.rate.max(1)) / 2).max(1)
        }
    }

    static CARD: IrqSpinMutex<Option<Card>> = IrqSpinMutex::new(None);

    /// Make `device` the sound card.
    pub fn register(mut device: Box<dyn AudioDevice>) -> Result<(), &'static str> {
        let buffer = DmaBuffer::new(RING_BYTES, DmaConstraints::BELOW_4G)
            .map_err(|_| "no memory for the sound ring")?;
        device.attach(buffer.device_address());
        let rate = device.set_rate(Format::DEFAULT.rate);
        let mut card = CARD.lock();
        if card.is_some() {
            return Err("only one sound card is supported");
        }
        crate::kinfo!("sound: {} at {} Hz is /dev/dsp", device.name(), rate);
        *card = Some(Card {
            device,
            buffer,
            ring: Ring::new(),
            rate,
            open: false,
            underruns: 0,
        });
        Ok(())
    }

    /// Run `f` on the card, which exists once a handle does.
    fn with_card<T>(f: impl FnOnce(&mut Card) -> T) -> T {
        f(CARD
            .lock()
            .as_mut()
            .expect("a /dev/dsp handle without a card"))
    }

    impl Card {
        /// Pad a partial period with silence and start the card on
        /// whatever is queued, so everything written gets played.
        fn flush(&mut self) {
            if let Some((offset, length)) = self.ring.padding() {
                self.buffer.as_mut_slice()[offset..offset + length].fill(0);
                let action = self.ring.commit(length);
                self.apply(action);
            }
            let action = self.ring.start();
            self.apply(action);
        }
    }

    /// Wait until everything queued has played and the card has stopped.
    fn drain() {
        loop {
            let wait = with_card(|card| {
                card.poll(true);
                card.flush();
                let busy = card.ring.is_running() || card.ring.queued() > 0;
                (busy || card.ring.pending() > 0).then(|| card.wait_ms())
            });
            match wait {
                Some(wait) => crate::scheduler::sleep_ms(wait),
                None => return,
            }
        }
    }

    /// `open("/dev/dsp")`: the card for this open file.
    pub fn open(ino: u64, fs_id: u64) -> FsResult<Arc<dyn Inode>> {
        let mut card = CARD.lock();
        let card = card.as_mut().ok_or(Errno::ENODEV)?;
        if core::mem::replace(&mut card.open, true) {
            return Err(Errno::EBUSY);
        }
        Ok(Arc::new(DspHandle {
            ino,
            fs_id,
            converter: SpinMutex::new(Converter::new(Format::DEFAULT)),
        }))
    }

    /// One open of `/dev/dsp`; dropping the last descriptor releases it.
    struct DspHandle {
        ino: u64,
        fs_id: u64,
        converter: SpinMutex<Converter>,
    }

    impl DspHandle {
        /// Program the card's rate for this handle's format (after the
        /// audio queued at the old rate has played).
        fn apply_rate(&self) {
            let rate = self.converter.lock().format().rate;
            if with_card(|card| card.rate) == rate {
                return;
            }
            drain();
            with_card(|card| card.rate = card.device.set_rate(rate));
        }

        fn set_format(&self, change: impl FnOnce(&mut Format)) -> Format {
            let mut converter = self.converter.lock();
            let mut format = converter.format();
            change(&mut format);
            // A partial frame in the old format is dropped.
            *converter = Converter::new(format);
            format
        }
    }

    impl Inode for DspHandle {
        fn stat(&self) -> Stat {
            Stat {
                ino: self.ino,
                fs_id: self.fs_id,
                kind: InodeKind::CharDevice,
                size: 0,
                nlink: 1,
            }
        }

        fn kind(&self) -> InodeKind {
            InodeKind::CharDevice
        }

        fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> FsResult<usize> {
            Err(Errno::EINVAL)
        }

        /// Blocks until all of `input` is in the ring.
        fn write_at(&self, _offset: u64, input: &[u8]) -> FsResult<usize> {
            self.apply_rate();
            let mut consumed = 0;
            loop {
                let wait = with_card(|card| {
                    card.poll(false);
                    let mut converter = self.converter.lock();
                    while consumed < input.len() {
                        let Some((offset, room)) = card.ring.space() else {
                            break;
                        };
                        let output = &mut card.buffer.as_mut_slice()[offset..offset + room];
                        let (taken, produced) = converter.convert(&input[consumed..], output);
                        consumed += taken;
                        if produced > 0 {
                            let action = card.ring.commit(produced);
                            card.apply(action);
                        } else if taken == 0 {
                            break;
                        }
                    }
                    card.wait_ms()
                });
                if consumed == input.len() {
                    return Ok(input.len());
                }
                crate::scheduler::sleep_ms(wait);
            }
        }

        fn ioctl(&self, request: u64, value: &mut i32) -> FsResult<()> {
            match request {
                SNDCTL_DSP_SPEED => {
                    let wanted = u32::try_from(*value).map_err(|_| Errno::EINVAL)?;
                    if wanted == 0 {
                        return Err(Errno::EINVAL);
                    }
                    self.set_format(|format| format.rate = wanted);
                    self.apply_rate();
                    let actual = with_card(|card| card.rate);
                    self.set_format(|format| format.rate = actual);
                    *value = actual as i32;
                }
                SNDCTL_DSP_CHANNELS => {
                    if *value < 1 {
                        return Err(Errno::EINVAL);
                    }
                    let channels = if *value == 1 { 1 } else { 2 };
                    self.set_format(|format| format.channels = channels);
                    *value = i32::from(channels);
                }
                SNDCTL_DSP_SETFMT => {
                    let format = if *value == AFMT_QUERY {
                        self.converter.lock().format()
                    } else {
                        let sample = Sample::from_oss(*value).unwrap_or(Sample::S16);
                        self.set_format(|format| format.sample = sample)
                    };
                    *value = format.sample.oss();
                }
                SNDCTL_DSP_SYNC => drain(),
                SNDCTL_DSP_RESET => {
                    with_card(|card| {
                        card.device.stop();
                        card.ring.reset();
                    });
                    self.set_format(|_| {});
                }
                _ => return Err(Errno::ENOTTY),
            }
            Ok(())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    impl Drop for DspHandle {
        fn drop(&mut self) {
            // What was written plays out (a partial period too), without
            // waiting; the next opener continues from there.
            with_card(|card| {
                card.poll(false);
                card.flush();
                card.open = false;
            });
        }
    }

    /// `/proc/sound`, up to date with the card. A card stopping while no
    /// one has `/dev/dsp` open has just finished, not underrun.
    pub fn proc_text() -> Vec<u8> {
        let mut card = CARD.lock();
        let Some(card) = card.as_mut() else {
            return b"no sound card\n".to_vec();
        };
        card.poll(!card.open);
        alloc::format!(
            "card: {}\nrate: {} Hz\nstate: {}\nopen: {}\nperiods played: {}\nunderruns: {}\n",
            card.device.name(),
            card.rate,
            if card.ring.is_running() {
                "playing"
            } else {
                "idle"
            },
            if card.open { "yes" } else { "no" },
            card.ring.played(),
            card.underruns,
        )
        .into_bytes()
    }
}

#[cfg(not(host_model))]
pub use kernel::*;
