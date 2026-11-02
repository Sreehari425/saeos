#![cfg(kani)]

#[path = "../../../kernel/src/sound/ring.rs"]
mod ring;

use ring::{Action, PERIOD_BYTES, PERIODS, RING_BYTES, Ring, START_PERIODS, Status};

fn assert_invariants(ring: &Ring) {
    assert!(ring.queued() <= PERIODS - 1);
    assert!(ring.pending() < PERIOD_BYTES);

    if let Some((offset, room)) = ring.space() {
        assert!(offset < RING_BYTES);
        assert!(room <= PERIOD_BYTES);
        assert!(offset + room <= RING_BYTES);
    }

    if let Some((offset, room)) = ring.padding() {
        assert!(offset < RING_BYTES);
        assert!(room <= PERIOD_BYTES);
        assert!(offset + room <= RING_BYTES);
        assert!(room > 0);
    }
}

#[kani::proof]
fn ring_space_and_partial_commit_stay_in_bounds() {
    let mut ring = Ring::new();
    assert_invariants(&ring);

    let count: usize = kani::any();
    kani::assume(count <= PERIOD_BYTES);
    assert_eq!(ring.space(), Some((0, PERIOD_BYTES)));
    let action = ring.commit(count);

    if count == PERIOD_BYTES {
        assert_eq!(action, Action::None);
        assert_eq!(ring.queued(), 1);
        assert_eq!(ring.pending(), 0);
    } else {
        assert_eq!(action, Action::None);
        assert_eq!(ring.queued(), 0);
        assert_eq!(ring.pending(), count);
    }
    assert_invariants(&ring);
}

#[kani::proof]
fn full_periods_start_at_threshold_and_wrap_safely() {
    let mut ring = Ring::new();

    for period in 0..(START_PERIODS + 2) {
        assert_invariants(&ring);
        let action = ring.commit(PERIOD_BYTES);
        if period + 1 < START_PERIODS {
            assert_eq!(action, Action::None);
        } else if period + 1 == START_PERIODS {
            assert_eq!(
                action,
                Action::Start {
                    last: START_PERIODS - 1
                }
            );
        } else {
            assert_eq!(action, Action::Submit { last: period });
        }
        assert_invariants(&ring);
    }

    assert!(ring.is_running());
    assert_eq!(ring.queued(), START_PERIODS + 2);
    assert_eq!(ring.pending(), 0);
}

#[kani::proof]
fn consumed_periods_allow_safe_wraparound() {
    let mut ring = Ring::new();
    for _ in 0..(PERIODS - 1) {
        ring.commit(PERIOD_BYTES);
    }
    assert_eq!(ring.queued(), PERIODS - 1);
    assert!(ring.is_running());

    // Retire periods 0..29, leaving period 30 at the head. The fill cursor
    // is at period 31, so subsequent submissions cross the end of the ring.
    let update = ring.update(Status {
        current: PERIODS - 2,
        halted: false,
    });
    assert_eq!(update.finished, Some((0, PERIODS - 2)));
    assert_eq!(ring.queued(), 1);
    assert_invariants(&ring);

    assert_eq!(
        ring.commit(PERIOD_BYTES),
        Action::Submit { last: PERIODS - 1 }
    );
    let update = ring.update(Status {
        current: PERIODS - 1,
        halted: false,
    });
    assert_eq!(update.finished, Some((PERIODS - 2, 1)));
    assert_eq!(ring.queued(), 1);
    assert_invariants(&ring);

    assert_eq!(ring.commit(PERIOD_BYTES), Action::Submit { last: 0 });
    let update = ring.update(Status {
        current: 0,
        halted: false,
    });
    assert_eq!(update.finished, Some((PERIODS - 1, 1)));
    assert_eq!(ring.queued(), 1);
    assert_eq!(ring.played(), PERIODS as u64);
    assert_invariants(&ring);
}

#[kani::proof]
fn valid_card_updates_and_halt_preserve_accounting() {
    let mut ring = Ring::new();
    for _ in 0..START_PERIODS {
        ring.commit(PERIOD_BYTES);
    }
    assert!(ring.is_running());

    let current: usize = kani::any();
    kani::assume(current < ring.queued());
    let before = ring.played();
    let update = ring.update(Status {
        current,
        halted: false,
    });
    assert!(!update.halted);
    assert_eq!(
        ring.played() - before,
        update.finished.map_or(0, |(_, count)| count as u64)
    );
    assert_invariants(&ring);

    let queued = ring.queued();
    let before = ring.played();
    let halted = ring.update(Status {
        current: 0,
        halted: true,
    });
    assert!(halted.halted);
    assert_eq!(halted.finished.map_or(0, |(_, count)| count), queued);
    assert_eq!(ring.played() - before, queued as u64);
    assert_eq!(ring.queued(), 0);
    assert!(!ring.is_running());
    assert_invariants(&ring);
}
