//! Sound output through `/dev/dsp` (the OSS interface).
//!
//! ```text
//! let mut dsp = Dsp::open()?;          // 48 kHz, 16-bit, stereo
//! dsp.set_rate(22_050)?;               // the rate the card really uses
//! dsp.set_channels(1)?; dsp.set_format(AFMT_U8)?;
//! dsp.write_all(&samples)?;            // blocks while the card catches up
//! dsp.sync()?;                         // until it has all been played
//! ```
//!
//! Closing does not wait for the queued audio (it keeps playing); call
//! [`Dsp::sync`] first to know it has.

use crate::errno::{Result, check};
use crate::raw;

pub const AFMT_U8: i32 = 0x08;
pub const AFMT_S16_LE: i32 = 0x10;

const SNDCTL_DSP_RESET: u64 = 0x5000;
const SNDCTL_DSP_SYNC: u64 = 0x5001;
const SNDCTL_DSP_SPEED: u64 = 0xc004_5002;
const SNDCTL_DSP_SETFMT: u64 = 0xc004_5005;
const SNDCTL_DSP_CHANNELS: u64 = 0xc004_5006;

pub struct Dsp {
    fd: u32,
}

impl Dsp {
    pub fn open() -> Result<Self> {
        let fd = check(raw::open(b"/dev/dsp\0", raw::O_WRONLY))?;
        Ok(Self { fd: fd as u32 })
    }

    pub fn fd(&self) -> u32 {
        self.fd
    }

    fn control(&self, request: u64, value: i32) -> Result<i32> {
        let mut value = value;
        check(raw::ioctl(self.fd, request, &raw mut value as u64))?;
        Ok(value)
    }

    /// Ask for `hz`; returns the rate the card plays at.
    pub fn set_rate(&mut self, hz: u32) -> Result<u32> {
        self.control(SNDCTL_DSP_SPEED, hz as i32)
            .map(|rate| rate as u32)
    }

    /// 1 (mono) or 2 (stereo); returns what was set.
    pub fn set_channels(&mut self, channels: u32) -> Result<u32> {
        self.control(SNDCTL_DSP_CHANNELS, channels as i32)
            .map(|channels| channels as u32)
    }

    /// `AFMT_U8` or `AFMT_S16_LE`; returns the format in use.
    pub fn set_format(&mut self, format: i32) -> Result<i32> {
        self.control(SNDCTL_DSP_SETFMT, format)
    }

    pub fn write_all(&mut self, samples: &[u8]) -> Result<()> {
        crate::io::write_all(self.fd, samples)
    }

    /// Wait until everything written has been played.
    pub fn sync(&mut self) -> Result<()> {
        check(raw::ioctl(self.fd, SNDCTL_DSP_SYNC, 0)).map(|_| ())
    }

    /// Stop at once, dropping what is queued.
    pub fn reset(&mut self) -> Result<()> {
        check(raw::ioctl(self.fd, SNDCTL_DSP_RESET, 0)).map(|_| ())
    }
}

impl Drop for Dsp {
    fn drop(&mut self) {
        raw::close(self.fd);
    }
}
