//! Sample-rate conversion: a program's rate to the card's.
//!
//! Most HD Audio codecs play only 44.1 and 48 kHz, so 8, 16 or 22.05 kHz
//! audio is converted rather than played at the wrong pitch. Linear
//! interpolation between neighbouring input frames, stepping through the
//! input in 32.32 fixed point (`input rate / output rate` per output
//! frame), so long streams do not drift:
//!
//! ```text
//! input frames:  a ─────── b ─────── c        position: 0 .. 1 between a and b
//! output frames: x   x   x   x   x   x        x = a + (b - a) * position
//! ```
//!
//! Simple and cheap; it is not band-limited, so downsampling can alias
//! (fine for voice and music, not for measurement). State carries across
//! writes, so where a write is split never changes the output. The last
//! input frame of a stream waits for a successor that never comes: one
//! frame (a fraction of a millisecond) is dropped at the end. Pure, tested
//! in the host model.

const ONE: u64 = 1 << 32;

pub type Frame = [i16; 2];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resampler {
    /// Input frames per output frame, 32.32.
    step: u64,
    /// Where the next output frame lies between `previous` and `next`.
    position: u64,
    previous: Option<Frame>,
    next: Option<Frame>,
}

impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            step: (u64::from(input_rate) << 32) / u64::from(output_rate.max(1)),
            position: 0,
            previous: None,
            next: None,
        }
    }

    /// Whether [`Resampler::produce`] needs another input frame first.
    pub fn wants_input(&self) -> bool {
        self.next.is_none()
    }

    pub fn feed(&mut self, frame: Frame) {
        debug_assert!(self.wants_input(), "fed before the last frame was used");
        if self.previous.is_none() {
            self.previous = Some(frame);
        } else {
            self.next = Some(frame);
        }
    }

    /// The next output frame, or `None` when more input is needed.
    pub fn produce(&mut self) -> Option<Frame> {
        let (previous, next) = (self.previous?, self.next?);
        if self.position >= ONE {
            // Past `next`: it becomes the left neighbour.
            self.position -= ONE;
            self.previous = Some(next);
            self.next = None;
            return None;
        }
        let fraction = self.position as i64;
        let mix = |from: i16, to: i16| {
            let difference = i64::from(to) - i64::from(from);
            (i64::from(from) + ((difference * fraction) >> 32)) as i16
        };
        self.position += self.step;
        Some([mix(previous[0], next[0]), mix(previous[1], next[1])])
    }
}
