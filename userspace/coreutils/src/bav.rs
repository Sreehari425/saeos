//! BAV video: black-and-white frames as run lengths of the rows that
//! changed (written by `scripts/bav.py`, which documents the format too).
//!
//! ```text
//! header  "BAV1", width u16, height u16, fps numerator u16,
//!         fps denominator u16, frame count u32 (little-endian)
//! frame   varint length, varint count of changed rows, per changed row:
//!         varint gap (first row's index, then rows skipped), varint run
//!         count, run lengths: black, white, black, ... (the first may be 0)
//! ```
//!
//! Frames are deltas from the previous one; before the first, all black.
//! [`Decoder::next_frame`] applies one and says which rows changed, so a
//! player redraws only those. Pure (core + alloc), tested in the host model.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub width: u16,
    pub height: u16,
    pub fps_numerator: u16,
    pub fps_denominator: u16,
    pub frames: u32,
}

impl Header {
    /// Nanoseconds from the start to frame `index`.
    pub fn frame_time_ns(&self, index: u32) -> u64 {
        u64::from(index) * 1_000_000_000 * u64::from(self.fps_denominator)
            / u64::from(self.fps_numerator.max(1))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BavError {
    NotBav,
    /// The file ends inside a frame (or its header).
    Truncated,
    /// A varint longer than 32 bits.
    BadVarint,
    /// A row index past the bottom, or runs that do not fill the row.
    BadRow,
    /// A frame's data does not end where its length said.
    BadLength,
}

impl BavError {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotBav => "not a BAV video",
            Self::Truncated => "truncated",
            Self::BadVarint => "bad number",
            Self::BadRow => "bad row",
            Self::BadLength => "bad frame length",
        }
    }
}

const HEADER_BYTES: usize = 16;

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

pub fn parse_header(bytes: &[u8]) -> Result<Header, BavError> {
    if bytes.len() < HEADER_BYTES || &bytes[..4] != b"BAV1" {
        return Err(BavError::NotBav);
    }
    let header = Header {
        width: u16_at(bytes, 4),
        height: u16_at(bytes, 6),
        fps_numerator: u16_at(bytes, 8),
        fps_denominator: u16_at(bytes, 10),
        frames: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
    };
    if header.width == 0
        || header.height == 0
        || header.fps_numerator == 0
        || header.fps_denominator == 0
    {
        return Err(BavError::NotBav);
    }
    Ok(header)
}

/// Plays a BAV file frame by frame into a bitmap (one byte per pixel: 0
/// black, 1 white).
pub struct Decoder<'a> {
    data: &'a [u8],
    offset: usize,
    header: Header,
    decoded: u32,
    pixels: Vec<u8>,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self, BavError> {
        let header = parse_header(data)?;
        Ok(Self {
            data,
            offset: HEADER_BYTES,
            header,
            decoded: 0,
            pixels: vec![0; usize::from(header.width) * usize::from(header.height)],
        })
    }

    pub fn header(&self) -> Header {
        self.header
    }

    /// Frames decoded so far.
    pub fn decoded(&self) -> u32 {
        self.decoded
    }

    pub fn row(&self, y: usize) -> &[u8] {
        let width = usize::from(self.header.width);
        &self.pixels[y * width..(y + 1) * width]
    }

    fn varint(&mut self, end: usize) -> Result<usize, BavError> {
        let mut value: u64 = 0;
        for shift in (0..35).step_by(7) {
            let &byte = self.data[..end]
                .get(self.offset)
                .ok_or(BavError::Truncated)?;
            self.offset += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return usize::try_from(value).map_err(|_| BavError::BadVarint);
            }
        }
        Err(BavError::BadVarint)
    }

    /// Apply the next frame; the rows it changed go into `changed`
    /// (cleared first). `Ok(false)` once every frame has been decoded.
    pub fn next_frame(&mut self, changed: &mut Vec<u16>) -> Result<bool, BavError> {
        changed.clear();
        if self.decoded == self.header.frames {
            return Ok(false);
        }
        let length = self.varint(self.data.len())?;
        let end = self
            .offset
            .checked_add(length)
            .filter(|&end| end <= self.data.len())
            .ok_or(BavError::Truncated)?;
        let width = usize::from(self.header.width);
        let height = usize::from(self.header.height);
        let rows = self.varint(end)?;
        let mut y: usize = 0;
        for index in 0..rows {
            let gap = self.varint(end)?;
            y = if index == 0 { gap } else { y + 1 + gap };
            if y >= height {
                return Err(BavError::BadRow);
            }
            let runs = self.varint(end)?;
            let (mut x, mut colour) = (0usize, 0u8);
            for _ in 0..runs {
                let run = self.varint(end)?;
                let stop = x
                    .checked_add(run)
                    .filter(|&stop| stop <= width)
                    .ok_or(BavError::BadRow)?;
                self.pixels[y * width + x..y * width + stop].fill(colour);
                x = stop;
                colour ^= 1;
            }
            if x != width {
                return Err(BavError::BadRow);
            }
            changed.push(y as u16);
        }
        if self.offset != end {
            return Err(BavError::BadLength);
        }
        self.decoded += 1;
        Ok(true)
    }
}
