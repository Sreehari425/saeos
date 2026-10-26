//! Sample formats: what a program writes to `/dev/dsp`, converted to what
//! the card plays (16-bit signed little-endian stereo).
//!
//! | format       | sample                    | to S16                |
//! |--------------|---------------------------|-----------------------|
//! | `AFMT_U8`    | 1 byte, unsigned, 128 = 0 | `(s - 128) << 8`      |
//! | `AFMT_S16_LE`| 2 bytes, signed           | as is                 |
//!
//! Mono is played on both channels. A frame split across two `write`s is
//! kept until the rest arrives. Pure, tested in the host model.

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

/// Converts a stream of input frames, carrying a partial frame between
/// calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converter {
    format: Format,
    carry: [u8; 4],
    carried: usize,
}

impl Converter {
    pub fn new(format: Format) -> Self {
        Self {
            format,
            carry: [0; 4],
            carried: 0,
        }
    }

    pub fn format(&self) -> Format {
        self.format
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

    fn output_frame(&self, frame: &[u8], output: &mut [u8]) {
        let size = self.format.sample.bytes();
        let left = self.decode(&frame[..size]);
        let right = if self.format.channels == 2 {
            self.decode(&frame[size..2 * size])
        } else {
            left
        };
        output[..2].copy_from_slice(&left.to_le_bytes());
        output[2..4].copy_from_slice(&right.to_le_bytes());
    }

    /// Convert as much of `input` as fits in `output` as whole output
    /// frames. Returns (input bytes consumed, output bytes produced);
    /// trailing bytes of a partial input frame are consumed into the carry.
    pub fn convert(&mut self, input: &[u8], output: &mut [u8]) -> (usize, usize) {
        let frame = self.format.frame_bytes();
        let mut consumed = 0;
        let mut produced = 0;
        // Finish a carried frame first.
        if self.carried > 0 {
            let needed = frame - self.carried;
            if input.len() < needed {
                self.carry[self.carried..self.carried + input.len()].copy_from_slice(input);
                self.carried += input.len();
                return (input.len(), 0);
            }
            if output.len() < OUTPUT_FRAME {
                return (0, 0);
            }
            self.carry[self.carried..frame].copy_from_slice(&input[..needed]);
            let carry = self.carry;
            self.output_frame(&carry[..frame], &mut output[..OUTPUT_FRAME]);
            self.carried = 0;
            consumed = needed;
            produced = OUTPUT_FRAME;
        }
        while input.len() - consumed >= frame && output.len() - produced >= OUTPUT_FRAME {
            self.output_frame(
                &input[consumed..consumed + frame],
                &mut output[produced..produced + OUTPUT_FRAME],
            );
            consumed += frame;
            produced += OUTPUT_FRAME;
        }
        // A partial frame at the end of the input waits in the carry
        // (only once the whole frames before it are out).
        let rest = input.len() - consumed;
        if rest > 0 && rest < frame {
            self.carry[..rest].copy_from_slice(&input[consumed..]);
            self.carried = rest;
            consumed = input.len();
        }
        (consumed, produced)
    }
}
