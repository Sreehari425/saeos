//! The playback ring: one DMA buffer cut into [`PERIODS`] periods of
//! [`PERIOD_BYTES`], which the card plays in order.
//!
//! ```text
//!   period:  0    1    2    3    4    5   ...   31
//!            [played ][ queued (submitted)  ][fill][  free  ]
//!                     ^ first_queued: the one playing
//!                                            ^ fill_period: being written
//! ```
//!
//! Writes go only into the fill period, which is never one the card is
//! playing or has been handed. A period is submitted (the card's "last
//! valid" index moves to it) as soon as it is full, and at most
//! `PERIODS - 1` are queued at once, so the fill period always exists.
//! The card stops by itself after the last submitted period; the ring then
//! goes idle and moves any partial period to period 0.
//!
//! An idle card starts again from period 0 once [`START_PERIODS`] are
//! queued (ALSA's start threshold): starting on the first period would
//! underrun at once whenever the writer takes longer than one period
//! between writes, a click at the start of every sound. A drain or a close
//! starts it with less ([`Ring::start`]). Pure bookkeeping, no memory
//! access: the caller copies bytes and talks to the card, so the host
//! model tests it against a fake card.

pub const PERIODS: usize = 32;
/// 512 frames of 16-bit stereo: 10.7 ms at 48 kHz.
pub const PERIOD_BYTES: usize = 2048;
pub const RING_BYTES: usize = PERIODS * PERIOD_BYTES;
const MAX_QUEUED: usize = PERIODS - 1;
/// Queued periods that start an idle card: 43 ms at 48 kHz.
pub const START_PERIODS: usize = 4;

/// What the card reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// The period it is playing (or last played, once halted).
    pub current: usize,
    /// It stopped after the last submitted period (or was never started).
    pub halted: bool,
}

/// What to tell the card after a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Start from period 0 and play up to `last`.
    Start {
        last: usize,
    },
    /// `last` is the new last valid period.
    Submit {
        last: usize,
    },
}

/// What [`Ring::update`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Update {
    /// The card stopped: everything queued has been played.
    pub halted: bool,
    /// Copy `length` bytes at ring offset `from` to offset 0 (a partial
    /// period moving to period 0 for the restart).
    pub relocate: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ring {
    fill_period: usize,
    fill_bytes: usize,
    first_queued: usize,
    queued: usize,
    running: bool,
    played: u64,
}

impl Default for Ring {
    fn default() -> Self {
        Self::new()
    }
}

impl Ring {
    pub const fn new() -> Self {
        Self {
            fill_period: 0,
            fill_bytes: 0,
            first_queued: 0,
            queued: 0,
            running: false,
            played: 0,
        }
    }

    /// Whether the card is (as far as we know) playing.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Periods handed to the card and not yet known to be played.
    pub fn queued(&self) -> usize {
        self.queued
    }

    /// Bytes written but not yet in a submitted period.
    pub fn pending(&self) -> usize {
        self.fill_bytes
    }

    /// Periods the card has finished since the ring was created.
    pub fn played(&self) -> u64 {
        self.played
    }

    /// Catch up with the card.
    pub fn update(&mut self, status: Status) -> Update {
        if !self.running {
            return Update::default();
        }
        if status.halted {
            // Everything queued has been played; restart from period 0.
            self.played += self.queued as u64;
            self.queued = 0;
            self.running = false;
            self.first_queued = 0;
            let relocate = (self.fill_bytes > 0 && self.fill_period != 0)
                .then_some((self.fill_period * PERIOD_BYTES, self.fill_bytes));
            self.fill_period = 0;
            return Update {
                halted: true,
                relocate,
            };
        }
        // The card never passes the last submitted period, so `current`
        // is within the queued ones.
        let done = (status.current + PERIODS - self.first_queued) % PERIODS;
        debug_assert!(done < self.queued, "card outside the queued periods");
        let done = done.min(self.queued.saturating_sub(1));
        self.played += done as u64;
        self.queued -= done;
        self.first_queued = (self.first_queued + done) % PERIODS;
        Update::default()
    }

    /// Where the next bytes go (ring offset, room), or `None` while the
    /// ring is full: wait for the card.
    pub fn space(&self) -> Option<(usize, usize)> {
        (self.queued < MAX_QUEUED).then_some((
            self.fill_period * PERIOD_BYTES + self.fill_bytes,
            PERIOD_BYTES - self.fill_bytes,
        ))
    }

    /// `count` bytes were written at [`Ring::space`].
    pub fn commit(&mut self, count: usize) -> Action {
        assert!(
            self.queued < MAX_QUEUED && self.fill_bytes + count <= PERIOD_BYTES,
            "write past the space the ring offered"
        );
        self.fill_bytes += count;
        if self.fill_bytes < PERIOD_BYTES {
            return Action::None;
        }
        let period = self.fill_period;
        self.fill_period = (period + 1) % PERIODS;
        self.fill_bytes = 0;
        self.queued += 1;
        if self.running {
            Action::Submit { last: period }
        } else if self.queued >= START_PERIODS {
            self.start()
        } else {
            Action::None
        }
    }

    /// Start an idle card on what is queued, below the threshold too (a
    /// drain, or the last writer closing).
    pub fn start(&mut self) -> Action {
        if self.running || self.queued == 0 {
            return Action::None;
        }
        debug_assert_eq!(self.first_queued, 0, "an idle ring starts at period 0");
        self.running = true;
        Action::Start {
            last: self.queued - 1,
        }
    }

    /// For a drain: the rest of a partial period, to fill with silence and
    /// then [`Ring::commit`]. `None` if there is no partial period.
    pub fn padding(&self) -> Option<(usize, usize)> {
        (self.fill_bytes > 0).then(|| self.space()).flatten()
    }

    /// Forget everything (the card has been stopped).
    pub fn reset(&mut self) {
        *self = Self {
            played: self.played,
            ..Self::new()
        };
    }
}
