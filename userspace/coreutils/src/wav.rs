//! WAV files: a RIFF container with a `fmt ` chunk (the sample format)
//! and a `data` chunk (the samples).
//!
//! ```text
//! "RIFF" size "WAVE"   then chunks: id (4 bytes), size (u32 LE), body,
//!                      padded to an even length
//!   "fmt "  format u16 (1 = PCM), channels u16, rate u32, byte rate u32,
//!           block align u16, bits per sample u16
//!   "data"  the frames
//! ```
//!
//! Plain PCM only, 8 or 16 bits, mono or stereo, any rate; other chunks
//! (`LIST`, `fact`, ...) are skipped. A `data` size larger than the file
//! (streamed WAVs leave it unset) is cut to what is there, and to whole
//! frames. [`parse_header`] needs only the start of the file, up to the
//! `data` chunk's header, so a player can stream the samples. Pure (core
//! only), tested in the host model.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wav<'a> {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub data: &'a [u8],
}

/// The format, and where the samples are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    /// Byte offset of the first sample in the file.
    pub data_offset: usize,
    /// The `data` chunk's size as recorded (may exceed the file).
    pub data_length: usize,
}

impl Header {
    pub fn frame_bytes(&self) -> usize {
        usize::from(self.channels) * usize::from(self.bits / 8)
    }

    /// The bytes of samples to play in a file of `file_size` bytes: the
    /// recorded size, cut to the file and to whole frames.
    pub fn playable(&self, file_size: usize) -> usize {
        let length = self
            .data_length
            .min(file_size.saturating_sub(self.data_offset));
        length - length % self.frame_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavError {
    /// Not a RIFF/WAVE file.
    NotWave,
    /// A chunk header or the `fmt ` chunk is cut short.
    Truncated,
    NoFormat,
    NoData,
    Unsupported(&'static str),
}

impl WavError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotWave => "not a WAV file",
            Self::Truncated => "truncated",
            Self::NoFormat => "no fmt chunk",
            Self::NoData => "no data chunk",
            Self::Unsupported(what) => what,
        }
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

/// A whole file in memory.
pub fn parse(bytes: &[u8]) -> Result<Wav<'_>, WavError> {
    let header = parse_header(bytes)?;
    let length = header.playable(bytes.len());
    Ok(Wav {
        rate: header.rate,
        channels: header.channels,
        bits: header.bits,
        data: &bytes[header.data_offset..header.data_offset + length],
    })
}

/// The header from the start of a file (`prefix`), which must reach the
/// `data` chunk's header; the samples need not be there.
pub fn parse_header(bytes: &[u8]) -> Result<Header, WavError> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWave);
    }
    let mut format = None;
    let mut offset = 12;
    while offset < bytes.len() {
        if bytes.len() - offset < 8 {
            return Err(WavError::Truncated);
        }
        let id = &bytes[offset..offset + 4];
        let size = u32_at(bytes, offset + 4) as usize;
        let body = offset + 8;
        let available = bytes.len() - body;
        match id {
            b"fmt " => {
                if size < 16 || available < 16 {
                    return Err(WavError::Truncated);
                }
                let tag = u16_at(bytes, body);
                let channels = u16_at(bytes, body + 2);
                let rate = u32_at(bytes, body + 4);
                let bits = u16_at(bytes, body + 14);
                if tag != 1 {
                    return Err(WavError::Unsupported("not plain PCM"));
                }
                if !(1..=2).contains(&channels) {
                    return Err(WavError::Unsupported("only mono and stereo"));
                }
                if bits != 8 && bits != 16 {
                    return Err(WavError::Unsupported("only 8 and 16-bit samples"));
                }
                if rate == 0 {
                    return Err(WavError::Unsupported("a rate of 0"));
                }
                format = Some((rate, channels, bits));
            }
            b"data" => {
                let (rate, channels, bits) = format.ok_or(WavError::NoFormat)?;
                return Ok(Header {
                    rate,
                    channels,
                    bits,
                    data_offset: body,
                    data_length: size,
                });
            }
            _ => {}
        }
        // Chunks are padded to an even size.
        offset = body.saturating_add(size).saturating_add(size & 1);
    }
    Err(if format.is_some() {
        WavError::NoData
    } else {
        WavError::NoFormat
    })
}
