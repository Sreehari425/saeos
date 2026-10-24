//! Host tests for the kernel's buddy core (`kernel/src/mm/buddy.rs`).
//!
//! The oracle is the bitmap itself: with eager coalescing, an allocation of
//! order `k` must succeed exactly when some aligned run of `2^k` frames is
//! entirely free. Every step also checks that live blocks never overlap and
//! that `validate()` agrees with the bitmap.

use crate::buddy::{BuddyLists, FrameStore, FreeNode, MAX_ORDER, ReleaseError, State, Zone};
use std::collections::BTreeMap;

struct VecStore {
    states: Vec<State>,
    // The first bytes of every frame. Seeded with garbage so tests cover
    // stale contents in frames that were never list heads.
    nodes: Vec<FreeNode>,
}

impl VecStore {
    fn new(frames: usize, seed: u64) -> Self {
        let mut rng = Rng(seed);
        Self {
            states: vec![State::Free; frames],
            nodes: (0..frames)
                .map(|_| FreeNode {
                    magic: rng.next(),
                    order: rng.next() as u32 % (MAX_ORDER as u32 + 1),
                    _reserved: 0,
                    prev: rng.next() as usize,
                    next: rng.next() as usize,
                })
                .collect(),
        }
    }
}

impl FrameStore for VecStore {
    fn frame_count(&self) -> usize {
        self.states.len()
    }
    fn state(&self, frame: usize) -> State {
        self.states[frame]
    }
    fn set_state(&mut self, frame: usize, state: State) {
        self.states[frame] = state;
    }
    fn node(&mut self, frame: usize) -> *mut FreeNode {
        assert!(frame < self.nodes.len());
        unsafe { self.nodes.as_mut_ptr().add(frame) }
    }
}

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

const ALL_ZONES: [Zone; 2] = [Zone::Normal, Zone::Dma32];

fn aligned_free_run_exists(store: &VecStore, order: usize) -> bool {
    let size = 1 << order;
    (0..store.frame_count() / size)
        .map(|block| block * size)
        .any(|start| (start..start + size).all(|frame| store.states[frame] == State::Free))
}

fn assert_consistent(lists: &BuddyLists, store: &mut VecStore, live: &BTreeMap<usize, usize>) {
    lists
        .validate(store)
        .expect("buddy lists disagree with bitmap");
    let mut owned = vec![false; store.frame_count()];
    for (&start, &order) in live {
        for frame in start..start + (1 << order) {
            assert!(!owned[frame], "frame {frame} handed out twice");
            owned[frame] = true;
            assert_eq!(store.states[frame], State::Allocated);
        }
    }
}

#[test]
fn randomized_alloc_free_matches_bitmap_oracle() {
    for seed in [1u64, 0x5eed, 0xdead_beef, 42, 7777] {
        let mut rng = Rng(seed);
        // Odd size so the top of memory is not a power-of-two block.
        let frames = 4096 + 37 + (seed as usize % 1000);
        let mut store = VecStore::new(frames, seed ^ 0xabcd);
        // A few reserved holes, as firmware maps have.
        for _ in 0..8 {
            let start = rng.below(frames as u64) as usize;
            let length = 1 + rng.below(64) as usize;
            for frame in start..(start + length).min(frames) {
                store.states[frame] = State::Reserved;
            }
        }
        let mut lists = BuddyLists::new();
        lists.build(&mut store);
        let mut live = BTreeMap::new();
        assert_consistent(&lists, &mut store, &live);

        for step in 0..20_000 {
            if live.is_empty() || rng.below(100) < 55 {
                let order = match rng.below(10) {
                    0..=5 => 0,
                    6..=7 => rng.below(4) as usize,
                    _ => rng.below(MAX_ORDER as u64 + 1) as usize,
                };
                let expected = aligned_free_run_exists(&store, order);
                match lists.allocate(&mut store, order, &ALL_ZONES) {
                    Some(start) => {
                        assert!(
                            expected,
                            "seed {seed} step {step}: allocated without a free run"
                        );
                        assert_eq!(start % (1 << order), 0, "misaligned block");
                        live.insert(start, order);
                    }
                    None => assert!(
                        !expected,
                        "seed {seed} step {step}: order {order} failed despite a free aligned run"
                    ),
                }
            } else {
                let index = rng.below(live.len() as u64) as usize;
                let (&start, &order) = live.iter().nth(index).unwrap();
                live.remove(&start);
                lists
                    .release(&mut store, start, order)
                    .expect("release of live block");
            }
            if step % 97 == 0 {
                assert_consistent(&lists, &mut store, &live);
            }
        }
        for (start, order) in std::mem::take(&mut live) {
            lists.release(&mut store, start, order).unwrap();
        }
        assert_consistent(&lists, &mut store, &live);
        // Everything free again: the lists must have merged back completely.
        let mut rebuilt = BuddyLists::new();
        let mut copy = VecStore {
            states: store.states.clone(),
            nodes: store.nodes.clone(),
        };
        rebuilt.build(&mut copy);
        for zone in ALL_ZONES {
            for order in 0..=MAX_ORDER {
                assert_eq!(
                    lists.count(zone, order),
                    rebuilt.count(zone, order),
                    "seed {seed}: incomplete coalescing at order {order}"
                );
            }
        }
    }
}

#[test]
fn release_errors_are_distinguished() {
    let mut store = VecStore::new(64, 3);
    store.states[63] = State::Reserved;
    let mut lists = BuddyLists::new();
    lists.build(&mut store);
    let frame = lists.allocate(&mut store, 0, &ALL_ZONES).unwrap();
    lists.release(&mut store, frame, 0).unwrap();
    assert_eq!(
        lists.release(&mut store, frame, 0),
        Err(ReleaseError::DoubleFree)
    );
    assert_eq!(
        lists.release(&mut store, 63, 0),
        Err(ReleaseError::DuplicateRelease)
    );
    assert_eq!(
        lists.release(&mut store, 64, 0),
        Err(ReleaseError::OutOfRange)
    );
    let block = lists.allocate(&mut store, 2, &ALL_ZONES).unwrap();
    assert_eq!(
        lists.release(&mut store, block + 1, 1),
        Err(ReleaseError::Unaligned)
    );
    lists.release(&mut store, block, 2).unwrap();
    lists.validate(&mut store).unwrap();
}

#[test]
fn isolate_and_reinsert_round_trip() {
    let mut store = VecStore::new(1024, 9);
    let mut lists = BuddyLists::new();
    lists.build(&mut store);
    for frame in [0, 1, 513, 700, 1023] {
        assert!(lists.isolate(&mut store, frame));
        store.states[frame] = State::Reserved;
        lists.validate(&mut store).unwrap();
    }
    assert!(
        !lists.isolate(&mut store, 513),
        "reserved frame isolated twice"
    );
    for frame in [0, 1, 513, 700, 1023] {
        lists.insert_reserved_range(&mut store, frame, frame + 1);
        lists.validate(&mut store).unwrap();
    }
    assert_eq!(
        lists.count(Zone::Dma32, 10),
        1,
        "did not merge back to one block"
    );
}

#[test]
fn zones_split_at_four_gib() {
    // Just past the Dma32 boundary, so both zones exist.
    let frames = (1 << 20) + 4096;
    let mut store = VecStore::new(frames, 11);
    let mut lists = BuddyLists::new();
    lists.build(&mut store);
    let normal = lists.allocate(&mut store, 0, &[Zone::Normal]).unwrap();
    assert!(normal >= 1 << 20);
    let low = lists.allocate(&mut store, 0, &[Zone::Dma32]).unwrap();
    assert!(low < 1 << 20);
    // Exhaust Normal; a Normal-only request must then fail, not spill low.
    let mut taken = Vec::new();
    while let Some(frame) = lists.allocate(&mut store, 0, &[Zone::Normal]) {
        taken.push(frame);
    }
    assert_eq!(taken.len(), 4096 - 1);
    assert!(lists.allocate(&mut store, 0, &[Zone::Normal]).is_none());
    assert!(lists.allocate(&mut store, 0, &ALL_ZONES).unwrap() < 1 << 20);
    lists.validate(&mut store).unwrap();
}
