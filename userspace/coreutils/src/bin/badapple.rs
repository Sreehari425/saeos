//! `badapple [VIDEO.bav] [AUDIO.wav] [--check]`: play a BAV video on
//! `/dev/fb0` with its sound on `/dev/dsp`, in step.
//!
//! The defaults are `/badapple/video.bav` and `/badapple/audio.wav`
//! (`make badapple-data` makes them from the original video).
//!
//! ```text
//! loop: write the audio for the next video frame   (blocks once the
//!       sound card's ring is full: this paces everything)
//!       heard = written - SNDCTL_DSP_GETODELAY      (the audio clock)
//!       decode and draw every frame whose time <= heard
//! ```
//!
//! Frames are never drawn before their sound is heard, and none is
//! skipped (they are deltas, so each must be decoded anyway). Each frame
//! is drawn at the largest whole scale that fits, centred; only rows that
//! changed are drawn. After the sound ends, any frames left follow the
//! clock. The last frame stays up for a second.
//!
//! It prints `badapple: N frames, max lag L ms, K late, clock +A/-B ms`:
//! lag is how long after its time a frame went up by the audio clock
//! (late: more than a frame period). The clock is also held against the
//! time since the first write, while sound plays: it may not run ahead
//! (sound cannot be heard sooner) by A, nor fall behind (the card starts
//! at once) by B. Lag cannot see a wrong clock, as frames follow it; a
//! wrong delay shows up in A or B. With `--check` the exit status says
//! whether all three stayed under 100 ms (the QEMU test).

#![no_std]
#![no_main]

extern crate alloc;

use coreutils::bav::{self, Decoder};
use coreutils::wav;
use libsaeos::fb::Framebuffer;
use libsaeos::io::{File, SeekFrom};
use libsaeos::prelude::*;
use libsaeos::sound::{AFMT_S16_LE, AFMT_U8, Dsp};
use libsaeos::{Args, entry, time};

entry!(main);

const HEADER_MAX: usize = 64 * 1024;
const MAX_LAG_MS: u64 = 100;

struct Screen {
    fb: Framebuffer,
    scale: usize,
    left: usize,
    top: usize,
    black: u32,
    white: u32,
    /// One scaled row, reused.
    scratch: Vec<u32>,
}

impl Screen {
    fn new(mut fb: Framebuffer, width: usize, height: usize) -> Self {
        let scale = (fb.width / width).min(fb.height / height).max(1);
        let black = fb.pixel(0x000000);
        let white = fb.pixel(0xffffff);
        let screen_height = fb.height;
        for y in 0..screen_height {
            fb.row(y).fill(black);
        }
        Self {
            left: fb.width.saturating_sub(width * scale) / 2,
            top: fb.height.saturating_sub(height * scale) / 2,
            black,
            white,
            scratch: vec![0; width * scale],
            scale,
            fb,
        }
    }

    /// Draw video row `y` (0/1 pixels), `scale` screen rows high.
    fn draw_row(&mut self, y: usize, row: &[u8]) {
        let scale = self.scale;
        for (x, &pixel) in row.iter().enumerate() {
            let colour = if pixel != 0 { self.white } else { self.black };
            self.scratch[x * scale..(x + 1) * scale].fill(colour);
        }
        let (left, width) = (self.left, self.scratch.len());
        for line in 0..scale {
            let screen_y = self.top + y * scale + line;
            if screen_y < self.fb.height {
                let end = (left + width).min(self.fb.width);
                self.fb.row(screen_y)[left..end].copy_from_slice(&self.scratch[..end - left]);
            }
        }
    }
}

struct Stats {
    drawn: u32,
    max_lag_ns: u64,
    late: u32,
    max_ahead_ns: u64,
    max_behind_ns: u64,
}

/// Decode and draw every frame due by `clock_ns`.
fn catch_up(
    decoder: &mut Decoder,
    screen: &mut Screen,
    clock_ns: u64,
    stats: &mut Stats,
    changed: &mut Vec<u16>,
) -> core::result::Result<(), bav::BavError> {
    let header = decoder.header();
    let period = header.frame_time_ns(1);
    while decoder.decoded() < header.frames {
        let due = header.frame_time_ns(decoder.decoded());
        if due > clock_ns {
            break;
        }
        decoder.next_frame(changed)?;
        for &y in changed.iter() {
            let y = usize::from(y);
            // Borrow the row, then draw it.
            let row: Vec<u8> = decoder.row(y).to_vec();
            screen.draw_row(y, &row);
        }
        let lag = clock_ns - due;
        stats.max_lag_ns = stats.max_lag_ns.max(lag);
        if lag > period {
            stats.late += 1;
        }
        stats.drawn += 1;
    }
    Ok(())
}

fn main(_args: Args) -> i32 {
    let mut arguments = coreutils::arguments();
    let check = arguments.contains(&"--check");
    arguments.retain(|argument| *argument != "--check");
    let video_path = arguments.first().copied().unwrap_or("/badapple/video.bav");
    let audio_path = arguments.get(1).copied().unwrap_or("/badapple/audio.wav");
    match play(video_path, audio_path) {
        Ok(stats) => {
            println!(
                "badapple: {} frames, max lag {} ms, {} late, clock +{}/-{} ms",
                stats.drawn,
                stats.max_lag_ns / time::NANOS_PER_MILLISECOND,
                stats.late,
                stats.max_ahead_ns / time::NANOS_PER_MILLISECOND,
                stats.max_behind_ns / time::NANOS_PER_MILLISECOND
            );
            let limit = MAX_LAG_MS * time::NANOS_PER_MILLISECOND;
            let good = stats.max_lag_ns < limit
                && stats.max_ahead_ns < limit
                && stats.max_behind_ns < limit;
            i32::from(check && !good)
        }
        Err(message) => {
            eprintln!("badapple: {message}");
            1
        }
    }
}

fn play(video_path: &str, audio_path: &str) -> core::result::Result<Stats, String> {
    let video = libsaeos::fs::read(video_path).map_err(|error| format!("{video_path}: {error}"))?;
    let mut decoder =
        Decoder::new(&video).map_err(|error| format!("{video_path}: {}", error.message()))?;
    let header = decoder.header();

    let mut audio = File::open(audio_path).map_err(|error| format!("{audio_path}: {error}"))?;
    let size = audio
        .seek(SeekFrom::End(0))
        .and_then(|size| audio.seek(SeekFrom::Start(0)).map(|_| size))
        .map_err(|error| format!("{audio_path}: {error}"))? as usize;
    let mut prefix = vec![0u8; HEADER_MAX.min(size)];
    coreutils::read_full(&mut audio, &mut prefix)
        .map_err(|error| format!("{audio_path}: {error}"))?;
    let sound =
        wav::parse_header(&prefix).map_err(|error| format!("{audio_path}: {}", error.message()))?;
    drop(prefix);
    let mut left = sound.playable(size);
    audio
        .seek(SeekFrom::Start(sound.data_offset as u64))
        .map_err(|error| format!("{audio_path}: {error}"))?;

    let fb = Framebuffer::open().map_err(|error| format!("/dev/fb0: {error}"))?;
    let mut screen = Screen::new(fb, usize::from(header.width), usize::from(header.height));
    let mut dsp = Dsp::open().map_err(|error| format!("/dev/dsp: {error}"))?;
    let format = if sound.bits == 8 {
        AFMT_U8
    } else {
        AFMT_S16_LE
    };
    dsp.set_format(format)
        .and_then(|_| dsp.set_channels(u32::from(sound.channels)))
        .and_then(|_| dsp.set_rate(sound.rate))
        .map_err(|error| format!("/dev/dsp: {error}"))?;

    let frame_bytes = sound.frame_bytes() as u64;
    let bytes_per_second = u64::from(sound.rate) * frame_bytes;
    // Audio bytes up to video frame n, in whole sample frames (no drift).
    let audio_until = |frame: u32| {
        u64::from(sound.rate) * u64::from(frame) * u64::from(header.fps_denominator)
            / u64::from(header.fps_numerator)
            * frame_bytes
    };
    let started = time::now();
    let heard_ns = |written: u64, dsp: &mut Dsp, stats: &mut Stats| {
        let delay = dsp.delay().unwrap_or(0) as u64;
        let heard = written.saturating_sub(delay) * 1_000_000_000 / bytes_per_second;
        let elapsed = time::now() - started;
        stats.max_ahead_ns = stats.max_ahead_ns.max(heard.saturating_sub(elapsed));
        stats.max_behind_ns = stats.max_behind_ns.max(elapsed.saturating_sub(heard));
        heard
    };
    let bad = |error: bav::BavError| format!("{video_path}: {}", error.message());

    let mut stats = Stats {
        drawn: 0,
        max_lag_ns: 0,
        late: 0,
        max_ahead_ns: 0,
        max_behind_ns: 0,
    };
    let mut changed = Vec::new();
    let mut chunk = Vec::new();
    let mut written: u64 = 0;
    let mut slice = 1;
    while left > 0 {
        // One video frame's worth of sound.
        let want = (audio_until(slice) - written).min(left as u64) as usize;
        slice += 1;
        if want == 0 {
            continue;
        }
        chunk.resize(want, 0);
        let count = coreutils::read_full(&mut audio, &mut chunk)
            .map_err(|error| format!("{audio_path}: {error}"))?;
        if count == 0 {
            break;
        }
        dsp.write_all(&chunk[..count])
            .map_err(|error| format!("/dev/dsp: {error}"))?;
        written += count as u64;
        left -= count;
        let clock = heard_ns(written, &mut dsp, &mut stats);
        catch_up(&mut decoder, &mut screen, clock, &mut stats, &mut changed).map_err(bad)?;
    }
    // The sound is all written: draw along as the rest is heard, then go
    // on by the clock if the video is longer.
    let audio_ns = written * 1_000_000_000 / bytes_per_second;
    loop {
        let clock = heard_ns(written, &mut dsp, &mut stats);
        catch_up(&mut decoder, &mut screen, clock, &mut stats, &mut changed).map_err(bad)?;
        if clock >= audio_ns || decoder.decoded() == header.frames {
            break;
        }
        time::sleep(5 * time::NANOS_PER_MILLISECOND);
    }
    let drained = time::now();
    while decoder.decoded() < header.frames {
        let clock = audio_ns + (time::now() - drained);
        catch_up(&mut decoder, &mut screen, clock, &mut stats, &mut changed).map_err(bad)?;
        time::sleep(5 * time::NANOS_PER_MILLISECOND);
    }
    dsp.sync().map_err(|error| format!("/dev/dsp: {error}"))?;
    println!("badapple: done");
    time::sleep(time::NANOS_PER_SECOND);
    Ok(stats)
}
