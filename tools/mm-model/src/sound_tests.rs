//! The sound core's pure parts: the playback ring against a fake card,
//! and the sample format conversion.
//!
//! The fake card follows AC'97 as QEMU implements it: it plays from period
//! 0 after a start, one period after another up to the last valid one,
//! then halts; a new last valid index written while it is halted (but not
//! stopped) resumes it at the next period. The model checks what matters:
//! nothing is ever written into a period the card owns, the card is never
//! restarted while playing, and what it plays is exactly what was written,
//! in order.

use crate::sound::format::{AFMT_S16_LE, AFMT_U8, Converter, Format, Sample};
use crate::sound::ring::{Action, PERIOD_BYTES, PERIODS, RING_BYTES, Ring, START_PERIODS, Status};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

struct FakeCard {
    started: bool,
    halted: bool,
    current: usize,
    last: usize,
}

impl FakeCard {
    /// Whether the card may still read `period`: it is playing it or will.
    fn owns(&self, period: usize) -> bool {
        if !self.started || self.halted {
            return false;
        }
        (period + PERIODS - self.current) % PERIODS
            <= (self.last + PERIODS - self.current) % PERIODS
    }

    fn status(&self) -> Status {
        Status {
            current: self.current,
            halted: !self.started || self.halted,
        }
    }
}

struct World {
    ring: Ring,
    card: FakeCard,
    memory: Vec<u8>,
    /// Every byte committed, in order (silence padding included).
    written: Vec<u8>,
    played: Vec<u8>,
    next_byte: u8,
    /// Paths the random test must reach.
    resumes: usize,
    full: usize,
    relocations: usize,
}

impl World {
    fn new() -> Self {
        Self {
            ring: Ring::new(),
            card: FakeCard {
                started: false,
                halted: true,
                current: 0,
                last: 0,
            },
            memory: vec![0; RING_BYTES],
            written: Vec::new(),
            played: Vec::new(),
            next_byte: 1,
            resumes: 0,
            full: 0,
            relocations: 0,
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Start { last } => {
                assert!(
                    !self.card.started || self.card.halted,
                    "card restarted while playing"
                );
                self.card = FakeCard {
                    started: true,
                    halted: false,
                    current: 0,
                    last,
                };
            }
            Action::Submit { last } => {
                assert!(self.card.started, "submit before start");
                if self.card.halted {
                    // QEMU: a new last valid index resumes a halted card.
                    assert_eq!(
                        last,
                        (self.card.last + 1) % PERIODS,
                        "resume skips a period"
                    );
                    self.card.halted = false;
                    self.card.current = last;
                    self.resumes += 1;
                }
                self.card.last = last;
            }
        }
    }

    /// Write up to `wanted` bytes, as `/dev/dsp`'s write does.
    fn write(&mut self, wanted: usize, padding: bool) -> usize {
        let (offset, room) = if padding {
            match self.ring.padding() {
                Some(space) => space,
                None => return 0,
            }
        } else {
            match self.ring.space() {
                Some(space) => space,
                None => {
                    self.full += 1;
                    return 0;
                }
            }
        };
        let count = if padding { room } else { wanted.min(room) };
        for byte in offset..offset + count {
            assert!(
                !self.card.owns(byte / PERIOD_BYTES),
                "write into period {} which the card owns",
                byte / PERIOD_BYTES
            );
            let value = if padding {
                0
            } else {
                self.next_byte = self.next_byte.wrapping_add(1).max(1);
                self.next_byte
            };
            self.memory[byte] = value;
            self.written.push(value);
        }
        let action = self.ring.commit(count);
        self.apply(action);
        count
    }

    /// Write exactly `count` bytes, which must fit without the card
    /// playing.
    fn write_all(&mut self, mut count: usize) {
        while count > 0 {
            let written = self.write(count, false);
            assert!(written > 0, "the ring is full");
            count -= written;
        }
    }

    /// The card plays one period.
    fn play(&mut self) {
        if !self.card.started || self.card.halted {
            return;
        }
        let start = self.card.current * PERIOD_BYTES;
        self.played
            .extend_from_slice(&self.memory[start..start + PERIOD_BYTES]);
        if self.card.current == self.card.last {
            self.card.halted = true;
        } else {
            self.card.current = (self.card.current + 1) % PERIODS;
        }
    }

    fn poll(&mut self) {
        let update = self.ring.update(self.card.status());
        if let Some((from, length)) = update.relocate {
            self.memory.copy_within(from..from + length, 0);
            self.relocations += 1;
        }
    }

    /// `SNDCTL_DSP_SYNC`: pad, start, then play until the card stops.
    fn drain(&mut self) {
        for _ in 0..10 * PERIODS {
            self.poll();
            self.write(0, true);
            let action = self.ring.start();
            self.apply(action);
            if !self.ring.is_running() && self.ring.queued() == 0 && self.ring.pending() == 0 {
                return;
            }
            self.play();
        }
        panic!("drain did not finish");
    }
}

#[test]
fn ring_never_touches_queued_periods_and_plays_everything_in_order() {
    let seeds: &[u64] = if cfg!(miri) {
        &[7]
    } else {
        &[1, 2, 3, 0x5eed, 0xdead_beef]
    };
    let steps = if cfg!(miri) { 80 } else { 40_000 };
    for &seed in seeds {
        let mut rng = Rng(seed);
        let mut world = World::new();
        let mut underruns = 0;
        for step in 0..steps {
            // Phases where the writer outruns the card (the ring fills)
            // alternate with ones where the card outruns it (underruns).
            let writes = if (step / (steps / 8)) % 2 == 0 {
                70
            } else {
                30
            };
            match rng.below(100) {
                // Writes of every size, from a byte to several periods.
                roll if roll < writes => {
                    let size = match rng.below(4) {
                        0 => 1 + rng.below(4) as usize,
                        1 => PERIOD_BYTES,
                        _ => 1 + rng.below(3 * PERIOD_BYTES as u64) as usize,
                    };
                    world.write(size, false);
                }
                // The card plays, sometimes several periods between polls.
                roll if roll < 85 => {
                    for _ in 0..1 + rng.below(3) {
                        world.play();
                    }
                }
                85..=97 => {
                    let was_running = world.ring.is_running();
                    world.poll();
                    if was_running && !world.ring.is_running() {
                        underruns += 1;
                    }
                }
                _ => world.drain(),
            }
            assert!(world.ring.queued() < PERIODS);
        }
        world.drain();
        assert_eq!(world.written.len(), world.played.len(), "seed {seed}");
        assert!(
            world.written == world.played,
            "seed {seed}: played out of order"
        );
        // Miri's short run checks for undefined behaviour; the full run
        // also proves the workload reaches every path.
        assert!(
            cfg!(miri)
                || (underruns > 0 && world.resumes > 0 && world.full > 0 && world.relocations > 0),
            "seed {seed}: paths not reached: underruns {underruns}, resumes {}, full {}, relocations {}",
            world.resumes,
            world.full,
            world.relocations
        );
        assert_eq!(
            world.ring.played() as usize * PERIOD_BYTES,
            world.played.len()
        );
    }
}

#[test]
fn a_full_ring_waits_and_the_fill_period_is_never_queued() {
    let mut world = World::new();
    // Fill everything the ring offers without the card playing.
    while world.write(PERIOD_BYTES, false) > 0 {}
    assert_eq!(world.ring.queued(), PERIODS - 1);
    assert_eq!(world.ring.space(), None);
    // One period played and noticed: exactly one period of room.
    world.play();
    world.poll();
    assert_eq!(world.ring.space().map(|(_, room)| room), Some(PERIOD_BYTES));
}

#[test]
fn the_card_starts_once_enough_is_queued() {
    let mut world = World::new();
    world.write_all(PERIOD_BYTES * (START_PERIODS - 1));
    assert!(!world.ring.is_running() && !world.card.started);
    world.write_all(PERIOD_BYTES);
    assert!(world.ring.is_running() && world.card.started);
    assert_eq!(world.card.last, START_PERIODS - 1);
    // Less than the threshold still plays on a drain.
    let mut world = World::new();
    world.write_all(PERIOD_BYTES + 1);
    world.drain();
    assert_eq!(world.played.len(), 2 * PERIOD_BYTES);
}

#[test]
fn underrun_restarts_from_period_zero_with_the_partial_period() {
    let mut world = World::new();
    world.write_all(PERIOD_BYTES * START_PERIODS + 100);
    assert!(world.ring.is_running());
    // The card plays every full period and halts; 100 bytes wait in the
    // next one.
    for _ in 0..START_PERIODS {
        world.play();
    }
    world.poll();
    assert!(!world.ring.is_running());
    assert_eq!(world.ring.pending(), 100);
    assert_eq!(world.ring.space(), Some((100, PERIOD_BYTES - 100)));
    // The relocated bytes are the ones that were waiting.
    assert!(world.memory[..100] == world.written[START_PERIODS * PERIOD_BYTES..]);
    world.write_all(PERIOD_BYTES * START_PERIODS);
    assert!(world.card.started && !world.card.halted);
    assert_eq!(world.card.current, 0, "restarted at period 0");
    world.drain();
    assert!(world.written == world.played);
}

#[test]
fn drain_pads_with_silence() {
    let mut world = World::new();
    world.write(10, false);
    world.drain();
    assert_eq!(world.played.len(), PERIOD_BYTES);
    assert!(world.played[10..].iter().all(|&byte| byte == 0));
    assert_eq!(world.ring.padding(), None);
    // Draining an idle ring does nothing.
    world.drain();
    assert_eq!(world.played.len(), PERIOD_BYTES);
}

fn convert_all(converter: &mut Converter, input: &[u8]) -> Vec<u8> {
    let mut output = vec![0u8; input.len() * 4 + 16];
    let (consumed, produced) = converter.convert(input, &mut output);
    assert_eq!(consumed, input.len());
    output.truncate(produced);
    output
}

fn frames(samples: &[(i16, i16)]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&(left, right)| [left.to_le_bytes(), right.to_le_bytes()].concat())
        .collect()
}

#[test]
fn formats_convert_to_16_bit_stereo() {
    let stereo = Format::DEFAULT;
    let input = frames(&[(1, -1), (i16::MAX, i16::MIN)]);
    assert_eq!(convert_all(&mut Converter::new(stereo), &input), input);

    let mono16 = Format {
        channels: 1,
        ..stereo
    };
    let input: Vec<u8> = [100i16, -200]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    assert_eq!(
        convert_all(&mut Converter::new(mono16), &input),
        frames(&[(100, 100), (-200, -200)])
    );

    let mono8 = Format {
        sample: Sample::U8,
        channels: 1,
        ..stereo
    };
    assert_eq!(
        convert_all(&mut Converter::new(mono8), &[128, 0, 255]),
        frames(&[(0, 0), (-32768, -32768), (127 << 8, 127 << 8)])
    );
    let stereo8 = Format {
        sample: Sample::U8,
        ..stereo
    };
    assert_eq!(
        convert_all(&mut Converter::new(stereo8), &[129, 127]),
        frames(&[(256, -256)])
    );
    assert_eq!(Sample::from_oss(AFMT_U8), Some(Sample::U8));
    assert_eq!(Sample::from_oss(AFMT_S16_LE), Some(Sample::S16));
    assert_eq!(Sample::from_oss(0x1000), None);
}

#[test]
fn frames_split_across_writes() {
    let input = frames(&[(1, 2), (3, 4), (5, 6)]);
    // Every split point, and byte by byte.
    for split in 0..=input.len() {
        let mut converter = Converter::new(Format::DEFAULT);
        let mut output = convert_all(&mut converter, &input[..split]);
        output.extend(convert_all(&mut converter, &input[split..]));
        assert_eq!(output, input, "split at {split}");
        assert_eq!(converter.carried(), 0);
    }
    let mut converter = Converter::new(Format::DEFAULT);
    let output: Vec<u8> = input
        .iter()
        .flat_map(|&byte| convert_all(&mut converter, &[byte]))
        .collect();
    assert_eq!(output, input);
}

#[test]
fn output_room_limits_conversion() {
    let input = frames(&[(1, 1), (2, 2), (3, 3)]);
    let mut converter = Converter::new(Format::DEFAULT);
    let mut output = [0u8; 6];
    // Room for one whole frame only.
    assert_eq!(converter.convert(&input, &mut output), (4, 4));
    // A carried partial frame waits when there is no room at all.
    let mut converter = Converter::new(Format::DEFAULT);
    assert_eq!(converter.convert(&input[..2], &mut []), (2, 0));
    assert_eq!(converter.convert(&input[2..], &mut []), (0, 0));
    let mut output = [0u8; 4];
    assert_eq!(converter.convert(&input[2..], &mut output), (2, 4));
    assert_eq!(output.as_slice(), &input[..4]);
}
