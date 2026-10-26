//! Sample formats: what a program writes to `/dev/dsp`, converted to what
//! the card plays (16-bit signed little-endian stereo).
//!
//! | format       | sample                    | to S16                |
//! |--------------|---------------------------|-----------------------|
//! | `AFMT_U8`    | 1 byte, unsigned, 128 = 0 | `(s - 128) << 8`      |
//! | `AFMT_S16_LE`| 2 bytes, signed           | as is                 |
//!
//! Mono is played on both channels. A frame split across two `write`s is
//! kept until the rest arrives. A rate the card cannot play is converted
//! ([`super::resample`]). Pure, tested in the host model.

use super::resample::{Frame, Resampler};

/// OSS format codes (`soundcard.h`).
pub const AFMT_U8: i32 = 0x08;
pub const AFMT_S16_LE: i32 = 0x10;
/// `SNDCTL_DSP_SETFMT` with this asks for the current format.
pub const AFMT_QUERY: i32 = 0;

/// Bytes per frame the card plays.
pub const OUTPUT_FRAME: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    U8,
    S16,
}

impl Sample {
    /// The OSS code's format, or `None` if unsupported.
    pub fn from_oss(code: i32) -> Option<Self> {
        match code {
            AFMT_U8 => Some(Self::U8),
            AFMT_S16_LE => Some(Self::S16),
            _ => None,
        }
    }

    pub fn oss(self) -> i32 {
        match self {
            Self::U8 => AFMT_U8,
            Self::S16 => AFMT_S16_LE,
        }
    }

    fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::S16 => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub sample: Sample,
    /// 1 or 2.
    pub channels: u8,
}

impl Format {
    /// What `/dev/dsp` starts with: the card's native format.
    pub const DEFAULT: Self = Self {
        rate: 48_000,
        sample: Sample::S16,
        channels: 2,
    };

    /// Bytes per input frame.
    pub fn frame_bytes(&self) -> usize {
        self.sample.bytes() * usize::from(self.channels)
    }
}

/// Converts a stream of input frames to the card's format and rate,
/// carrying a partial frame (and the resampler's state) between calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converter {
    format: Format,
    card_rate: u32,
    /// `None` when the card plays `format.rate` itself.
    resampler: Option<Resampler>,
    carry: [u8; 4],
    carried: usize,
}

impl Converter {
    /// `format` as written; `card_rate` what the card plays.
    pub fn new(format: Format, card_rate: u32) -> Self {
        Self {
            format,
            card_rate,
            resampler: (format.rate != card_rate).then(|| Resampler::new(format.rate, card_rate)),
            carry: [0; 4],
            carried: 0,
        }
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn card_rate(&self) -> u32 {
        self.card_rate
    }

    /// Bytes of a partial frame waiting for the rest.
    pub fn carried(&self) -> usize {
        self.carried
    }

    fn decode(&self, sample: &[u8]) -> i16 {
        match self.format.sample {
            Sample::U8 => (i16::from(sample[0]) - 128) << 8,
            Sample::S16 => i16::from_le_bytes([sample[0], sample[1]]),
        }
    }

    fn decode_frame(&self, frame: &[u8]) -> Frame {
        let size = self.format.sample.bytes();
        let left = self.decode(&frame[..size]);
        let right = if self.format.channels == 2 {
            self.decode(&frame[size..2 * size])
        } else {
            left
        };
        [left, right]
    }

    /// The next whole input frame (completing a carried one first), or
    /// `None` if the input does not hold one.
    fn next_frame(&mut self, input: &[u8], consumed: &mut usize) -> Option<Frame> {
        let size = self.format.frame_bytes();
        let rest = &input[*consumed..];
        if self.carried > 0 {
            let needed = size - self.carried;
            if rest.len() < needed {
                return None;
            }
            self.carry[self.carried..size].copy_from_slice(&rest[..needed]);
            self.carried = 0;
            *consumed += needed;
            let carry = self.carry;
            return Some(self.decode_frame(&carry[..size]));
        }
        if rest.len() < size {
            return None;
        }
        *consumed += size;
        Some(self.decode_frame(&rest[..size]))
    }

    /// Convert as much of `input` as fits in `output` as whole output
    /// frames. Returns (input bytes consumed, output bytes produced). Bytes
    /// that cannot make a whole frame yet are consumed into the carry.
    pub fn convert(&mut self, input: &[u8], output: &mut [u8]) -> (usize, usize) {
        let mut consumed = 0;
        let mut produced = 0;
        while output.len() - produced >= OUTPUT_FRAME {
            let frame = match &mut self.resampler {
                Some(resampler) => match resampler.produce() {
                    Some(frame) => frame,
                    None if !resampler.wants_input() => continue,
                    None => match self.next_frame(input, &mut consumed) {
                        Some(frame) => {
                            if let Some(resampler) = &mut self.resampler {
                                resampler.feed(frame);
                            }
                            continue;
                        }
                        None => break,
                    },
                },
                None => match self.next_frame(input, &mut consumed) {
                    Some(frame) => frame,
                    None => break,
                },
            };
            output[produced..produced + 2].copy_from_slice(&frame[0].to_le_bytes());
            output[produced + 2..produced + 4].copy_from_slice(&frame[1].to_le_bytes());
            produced += OUTPUT_FRAME;
        }
        // What cannot make a whole frame waits in the carry, whatever the
        // room (whole frames left over wait in `input` for the caller).
        let rest = input.len() - consumed;
        if self.carried + rest < self.format.frame_bytes() {
            self.carry[self.carried..self.carried + rest].copy_from_slice(&input[consumed..]);
            self.carried += rest;
            consumed = input.len();
        }
        (consumed, produced)
    }
}
