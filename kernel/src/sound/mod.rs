//! Sound: one playback card behind `/dev/dsp` (the OSS interface).
//!
//! ```text
//! write(/dev/dsp) ─▶ DspHandle ─▶ format::Converter (u8/s16, mono/stereo
//!                                  ─▶ s16 stereo, resampled to the card's
//!                                  rate) into the ring's fill period
//!                                ring::Ring: which periods are free, when to
//!                                  submit, restart after an underrun
//!                    ─▶ dyn AudioDevice (drivers/ac97.rs, drivers/hda/)
//! ```
//!
//! The core owns the ring buffer and all the bookkeeping; a driver only
//! points its card at the ring and starts, extends, polls and stops it.
//! Its one obligation: **stop after the last submitted period** (AC'97 does
//! by itself; HD Audio loops, so its driver stops the stream once its
//! position passes it). Whatever a card reads before it stops is silence:
//! the period being filled is staged and copied into the ring only when
//! complete, and finished periods are zeroed at every poll. While the card
//! plays, a `sound` kernel task polls it every half period, so this holds
//! with no writer too (after a close without a sync). A card that stops
//! making progress (no period finished for [`STALL_MS`]: misrouted, or its
//! DMA never started) is stopped and its queue dropped, so writers and
//! drains carry on instead of waiting forever. Waiting for the card
//! sleeps about half a period and polls; a driver with interrupts can later
//! wake the writer instead. A card plays the rate nearest to the one asked
//! for that it can ([`AudioDevice::rate_for`]); the converter resamples the
//! rest, so a program always gets the rate it asks for.
//!
//! `ioctl`s (OSS numbers, `soundcard.h`): `SNDCTL_DSP_SPEED` (returns the
//! rate asked for, 1 to 192 kHz), `SNDCTL_DSP_CHANNELS`, `SNDCTL_DSP_SETFMT`
//! (`AFMT_U8`, `AFMT_S16_LE`; others get S16 back), `SNDCTL_DSP_SYNC` (wait
//! until everything written has played), `SNDCTL_DSP_RESET` (stop now).
//! Each open starts at 48 kHz, S16, stereo. One opener at a time
//! (`EBUSY`). Unlike OSS, `close` does not wait: queued audio plays out on
//! its own, so call `SNDCTL_DSP_SYNC` to know it has.
//!
//! `format`, `resample` and `ring` are pure and tested in the host model.

pub mod format;
pub mod resample;
pub mod ring;

pub const SNDCTL_DSP_RESET: u64 = 0x5000;
pub const SNDCTL_DSP_SYNC: u64 = 0x5001;
pub const SNDCTL_DSP_SPEED: u64 = 0xc004_5002;
pub const SNDCTL_DSP_SETFMT: u64 = 0xc004_5005;
pub const SNDCTL_DSP_CHANNELS: u64 = 0xc004_5006;

#[cfg(not(host_model))]
mod kernel {
    use super::format::{AFMT_QUERY, Converter, Format, OUTPUT_FRAME, Sample};
    use super::ring::{Action, PERIOD_BYTES, PERIODS, RING_BYTES, Ring, Status};
    use super::{
        SNDCTL_DSP_CHANNELS, SNDCTL_DSP_RESET, SNDCTL_DSP_SETFMT, SNDCTL_DSP_SPEED, SNDCTL_DSP_SYNC,
    };
    use crate::fs::{Errno, FsResult, Inode, InodeKind, Stat};
    use crate::mm::dma::{DmaAddress, DmaBuffer, DmaConstraints};
    use crate::sync::{IrqSpinMutex, SpinMutex};
    use crate::task::WaitQueue;
    use alloc::boxed::Box;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::any::Any;
    use core::sync::atomic::{AtomicBool, Ordering};

    /// A playback card, as the core drives it.
    pub trait AudioDevice: Send {
        fn name(&self) -> &'static str;
        /// Point the card at the ring: `ring::PERIODS` periods of
        /// `ring::PERIOD_BYTES`, 16-bit stereo, at `ring`. Called once.
        fn attach(&mut self, ring: DmaAddress);
        /// The rate the card would play for a program asking `hz`: `hz`
        /// itself if it can, else the closest it can (the core resamples).
        fn rate_for(&self, hz: u32) -> u32;
        /// Set the sample rate (one [`AudioDevice::rate_for`] returned);
        /// returns the rate the card really uses. Only called while the
        /// card is stopped.
        fn set_rate(&mut self, hz: u32) -> u32;
        /// Play from period 0 up to `last`, then stop.
        fn start(&mut self, last: usize);
        /// Extend playback up to `last`.
        fn submit(&mut self, last: usize);
        fn status(&mut self) -> Status;
        /// Stop at once.
        fn stop(&mut self);
        /// More lines for `/proc/sound` (the codec, its outputs).
        fn describe(&self) -> alloc::string::String {
            alloc::string::String::new()
        }
    }

    struct Card {
        device: Box<dyn AudioDevice>,
        buffer: DmaBuffer,
        ring: Ring,
        /// The rate the card is set to.
        rate: u32,
        /// The rate the last program asked for (resampled to `rate`).
        requested_rate: u32,
        open: bool,
        /// A drain is waiting for the end: stopping is not an underrun.
        draining: bool,
        /// The card halted but may still be running through silence (HD
        /// Audio): stop it before new audio goes into the ring.
        quiet: bool,
        underruns: u64,
        /// The period being filled, until it is complete.
        staging: [u8; PERIOD_BYTES],
        /// When the card last finished a period (or was idle), in ticks.
        progress_at: u64,
        stalls: u64,
    }

    impl Card {
        fn apply(&mut self, action: Action) {
            match action {
                Action::None => {}
                Action::Start { last } => {
                    self.buffer.sync_for_device();
                    self.device.start(last);
                    crate::scheduler::wake_all(&POLL_WAIT);
                }
                Action::Submit { last } => {
                    self.buffer.sync_for_device();
                    self.device.submit(last);
                }
            }
        }

        /// Catch up with the card, and silence what it has finished. A
        /// stop while a drain waits, or with no one writing, is the end of
        /// the audio, not an underrun.
        fn poll(&mut self) {
            let now = crate::time::ticks();
            let update = self.ring.update(self.device.status());
            if update.finished.is_some() || !self.ring.is_running() {
                self.progress_at = now;
            } else if now.saturating_sub(self.progress_at) > STALL_MS {
                crate::kerror!(
                    "sound: {} stalled (no progress for {} ms); dropping its queue",
                    self.device.name(),
                    STALL_MS
                );
                self.reset();
                self.stalls += 1;
                return;
            }
            if update.halted {
                self.quiet = true;
                if self.open && !self.draining {
                    self.underruns += 1;
                }
            }
            if let Some((first, count)) = update.finished {
                let ring = self.buffer.as_mut_slice();
                for period in (first..first + count).map(|period| period % PERIODS) {
                    ring[period * PERIOD_BYTES..(period + 1) * PERIOD_BYTES].fill(0);
                }
            }
        }

        /// Stop the card and drop everything queued. The ring goes back to
        /// silence: a looping card must find nothing old in it later.
        fn reset(&mut self) {
            self.device.stop();
            self.ring.reset();
            self.buffer.as_mut_slice().fill(0);
            self.quiet = false;
            self.progress_at = crate::time::ticks();
        }

        /// `produced` bytes were staged at ring `offset`: once their period
        /// is complete it goes into the ring, then the ring is told.
        fn commit_staged(&mut self, offset: usize, produced: usize) {
            let (period, within) = (offset / PERIOD_BYTES, offset % PERIOD_BYTES);
            if within + produced == PERIOD_BYTES {
                // A card still running through silence would play this
                // early: stop it now (only silence is cut).
                if core::mem::take(&mut self.quiet) {
                    self.device.stop();
                }
                let start = period * PERIOD_BYTES;
                self.buffer.as_mut_slice()[start..start + PERIOD_BYTES]
                    .copy_from_slice(&self.staging);
            }
            let action = self.ring.commit(produced);
            self.apply(action);
        }

        /// How long to wait for the card to finish some of the queue.
        fn wait_ms(&self) -> u64 {
            let frames = (PERIOD_BYTES / OUTPUT_FRAME) as u64;
            (frames * 1000 / u64::from(self.rate.max(1)) / 2).max(1)
        }
    }

    static CARD: IrqSpinMutex<Option<Card>> = IrqSpinMutex::new(None);

    /// A running card that finishes no period for this long is stuck
    /// (periods are 10.7 to 64 ms at any rate).
    pub const STALL_MS: u64 = 1000;

    /// Make `device` the sound card.
    pub fn register(mut device: Box<dyn AudioDevice>) -> Result<(), &'static str> {
        let buffer = DmaBuffer::new(RING_BYTES, DmaConstraints::BELOW_4G)
            .map_err(|_| "no memory for the sound ring")?;
        device.attach(buffer.device_address());
        let rate = device.set_rate(device.rate_for(Format::DEFAULT.rate));
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
            requested_rate: Format::DEFAULT.rate,
            open: false,
            draining: false,
            quiet: false,
            underruns: 0,
            staging: [0; PERIOD_BYTES],
            progress_at: 0,
            stalls: 0,
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
                let within = offset % PERIOD_BYTES;
                self.staging[within..within + length].fill(0);
                self.commit_staged(offset, length);
            }
            let action = self.ring.start();
            self.apply(action);
        }
    }

    /// Wait until everything queued has played and the card has stopped.
    fn drain() {
        with_card(|card| card.draining = true);
        loop {
            let wait = with_card(|card| {
                card.poll();
                card.flush();
                let busy = card.ring.is_running() || card.ring.queued() > 0;
                (busy || card.ring.pending() > 0).then(|| card.wait_ms())
            });
            match wait {
                Some(wait) => crate::scheduler::sleep_ms(wait),
                None => break,
            }
        }
        // The card has passed the last sample; give what is still in its
        // buffers (one period's worth) time to be heard.
        let period = with_card(|card| {
            card.draining = false;
            2 * card.wait_ms()
        });
        crate::scheduler::sleep_ms(period);
    }

    /// Woken when the card starts; the poller sleeps here while it is idle.
    static POLL_WAIT: IrqSpinMutex<WaitQueue> =
        IrqSpinMutex::new(WaitQueue::new(crate::scheduler::SOUND_WAIT_ID));
    static POLLER_STARTED: AtomicBool = AtomicBool::new(false);

    /// The `sound` task: polls the card while it plays.
    fn poller() -> ! {
        let playing = || {
            CARD.lock()
                .as_ref()
                .is_some_and(|card| card.ring.is_running())
        };
        loop {
            crate::scheduler::wait_event(&POLL_WAIT, playing);
            let wait = with_card(|card| {
                card.poll();
                card.wait_ms()
            });
            crate::scheduler::sleep_ms(wait);
        }
    }

    /// `open("/dev/dsp")`: the card for this open file.
    pub fn open(ino: u64, fs_id: u64) -> FsResult<Arc<dyn Inode>> {
        let rate = {
            let mut card = CARD.lock();
            let card = card.as_mut().ok_or(Errno::ENODEV)?;
            if core::mem::replace(&mut card.open, true) {
                return Err(Errno::EBUSY);
            }
            card.rate
        };
        // The first open starts the poller (task context, after boot).
        if !POLLER_STARTED.swap(true, Ordering::AcqRel) {
            crate::scheduler::spawn_kernel_task(poller, "sound");
        }
        Ok(Arc::new(DspHandle {
            ino,
            fs_id,
            converter: SpinMutex::new(Converter::new(Format::DEFAULT, rate)),
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
            let (current, target) = with_card(|card| {
                card.requested_rate = rate;
                (card.rate, card.device.rate_for(rate))
            });
            if current == target {
                return;
            }
            drain();
            with_card(|card| card.rate = card.device.set_rate(target));
        }

        fn set_format(&self, change: impl FnOnce(&mut Format)) -> Format {
            let mut converter = self.converter.lock();
            let mut format = converter.format();
            change(&mut format);
            // A partial frame in the old format is dropped.
            *converter = Converter::new(format, converter.card_rate());
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
                    card.poll();
                    let mut converter = self.converter.lock();
                    if converter.card_rate() != card.rate {
                        *converter = Converter::new(converter.format(), card.rate);
                    }
                    while consumed < input.len() {
                        let Some((offset, room)) = card.ring.space() else {
                            break;
                        };
                        let within = offset % PERIOD_BYTES;
                        let output = &mut card.staging[within..within + room];
                        let (taken, produced) = converter.convert(&input[consumed..], output);
                        consumed += taken;
                        if produced > 0 {
                            card.commit_staged(offset, produced);
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
                    // Resampling makes any sane rate true.
                    let rate = wanted.clamp(1_000, 192_000);
                    self.set_format(|format| format.rate = rate);
                    self.apply_rate();
                    *value = rate as i32;
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
                    with_card(Card::reset);
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
                card.poll();
                card.flush();
                card.open = false;
            });
        }
    }

    /// `/proc/sound`, up to date with the card.
    pub fn proc_text() -> Vec<u8> {
        let mut card = CARD.lock();
        let Some(card) = card.as_mut() else {
            return b"no sound card\n".to_vec();
        };
        card.poll();
        alloc::format!(
            "card: {}\nrate: {} Hz (card {} Hz)\nstate: {}\nopen: {}\nperiods played: {}\nunderruns: {}\nstalls: {}\n{}",
            card.device.name(),
            card.requested_rate,
            card.rate,
            if card.ring.is_running() {
                "playing"
            } else {
                "idle"
            },
            if card.open { "yes" } else { "no" },
            card.ring.played(),
            card.underruns,
            card.stalls,
            card.device.describe(),
        )
        .into_bytes()
    }
}

#[cfg(not(host_model))]
pub use kernel::*;
