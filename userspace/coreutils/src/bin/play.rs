//! `play FILE.wav`: play a WAV file (PCM, 8 or 16-bit, mono or stereo) on
//! `/dev/dsp`, and wait until it has been heard.
//!
//! The samples are streamed in [`CHUNK`]-byte pieces, so a long file needs
//! no more memory than a short one. The header must lie in the first
//! [`HEADER_MAX`] bytes (it is a few dozen in practice).

#![no_std]
#![no_main]

extern crate alloc;

use coreutils::wav;
use libsaeos::io::{File, SeekFrom};
use libsaeos::prelude::*;
use libsaeos::sound::{AFMT_S16_LE, AFMT_U8, Dsp};
use libsaeos::{Args, entry};

entry!(main);

const HEADER_MAX: usize = 64 * 1024;
const CHUNK: usize = 8 * 1024;

fn main(_args: Args) -> i32 {
    let arguments = coreutils::arguments();
    let [path] = arguments.as_slice() else {
        eprintln!("usage: play FILE.wav");
        return 1;
    };
    let opened = File::open(path).and_then(|mut file| {
        let size = file.seek(SeekFrom::End(0))? as usize;
        file.seek(SeekFrom::Start(0))?;
        Ok((file, size))
    });
    let (mut file, size) = match opened {
        Ok(opened) => opened,
        Err(error) => return coreutils::report("play", path, error),
    };
    let mut prefix = vec![0u8; HEADER_MAX.min(size)];
    if let Err(error) = coreutils::read_full(&mut file, &mut prefix) {
        return coreutils::report("play", path, error);
    }
    let header = match wav::parse_header(&prefix) {
        Ok(header) => header,
        Err(error) => {
            eprintln!("play: {path}: {}", error.message());
            return 1;
        }
    };
    drop(prefix);
    let mut dsp = match Dsp::open() {
        Ok(dsp) => dsp,
        Err(error) => return coreutils::report("play", "/dev/dsp", error),
    };
    let format = if header.bits == 8 {
        AFMT_U8
    } else {
        AFMT_S16_LE
    };
    let set = dsp.set_format(format).and_then(|_| {
        dsp.set_channels(u32::from(header.channels))?;
        dsp.set_rate(header.rate)
    });
    match set {
        Ok(rate) if rate != header.rate => {
            eprintln!(
                "play: {path}: the card plays {} Hz at {rate} Hz (pitch will be off)",
                header.rate
            );
        }
        Ok(_) => {}
        Err(error) => return coreutils::report("play", "/dev/dsp", error),
    }
    let streamed = (|| {
        file.seek(SeekFrom::Start(header.data_offset as u64))?;
        let mut left = header.playable(size);
        let mut chunk = vec![0u8; CHUNK];
        while left > 0 {
            let count = coreutils::read_full(&mut file, &mut chunk[..CHUNK.min(left)])?;
            if count == 0 {
                break;
            }
            dsp.write_all(&chunk[..count])?;
            left -= count;
        }
        dsp.sync()
    })();
    match streamed {
        Ok(()) => 0,
        Err(error) => coreutils::report("play", path, error),
    }
}
