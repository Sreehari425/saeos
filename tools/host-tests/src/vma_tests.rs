//! Process memory maps: placement, splitting, brk, and a random model test.

use crate::fs::Errno;
use crate::vma::{
    BrkChange, MIN_ADDRESS, MemoryMap, PAGE_SIZE, Protection, Region, RegionKind, Span,
};
use std::collections::BTreeMap;

const PAGE: u64 = PAGE_SIZE;
const IMAGE: u64 = 0x40_0000;
const BRK_START: u64 = 0x40_3000;
const MMAP_TOP: u64 = 0x7fff_0000;

/// A typical exec'd program: three image segments and a stack above.
fn program() -> MemoryMap {
    let region = |start, end, protection, kind| Region {
        start,
        end,
        protection,
        kind,
    };
    MemoryMap::new(
        &[
            region(IMAGE, IMAGE + PAGE, Protection::READ, RegionKind::Image),
            region(
                IMAGE + PAGE,
                IMAGE + 2 * PAGE,
                Protection::READ_EXECUTE,
                RegionKind::Image,
            ),
            region(
                IMAGE + 2 * PAGE,
                BRK_START,
                Protection::READ_WRITE,
                RegionKind::Image,
            ),
            region(
                MMAP_TOP + PAGE,
                MMAP_TOP + 33 * PAGE,
                Protection::READ_WRITE,
                RegionKind::Stack,
            ),
        ],
        BRK_START,
        MMAP_TOP,
    )
    .unwrap()
}

fn assert_sorted_and_disjoint(map: &MemoryMap) {
    for pair in map.regions().windows(2) {
        assert!(pair[0].end <= pair[1].start, "{:x?}", map.regions());
    }
    for region in map.regions() {
        assert!(region.start < region.end);
        assert_eq!(region.start % PAGE, 0);
        assert_eq!(region.end % PAGE, 0);
    }
}

#[test]
fn protection_bits_follow_linux_and_forbid_wx() {
    assert_eq!(Protection::from_linux(0), Ok(Protection::NONE));
    assert_eq!(Protection::from_linux(1), Ok(Protection::READ));
    assert_eq!(Protection::from_linux(3), Ok(Protection::READ_WRITE));
    assert_eq!(Protection::from_linux(5), Ok(Protection::READ_EXECUTE));
    // Write-only is readable too on x86.
    assert_eq!(Protection::from_linux(2), Ok(Protection::READ_WRITE));
    assert_eq!(Protection::from_linux(7), Err(Errno::EACCES));
    assert_eq!(Protection::from_linux(6), Err(Errno::EACCES));
    assert_eq!(Protection::from_linux(8), Err(Errno::EINVAL));
}

#[test]
fn mmap_places_top_down_and_respects_hints() {
    let mut map = program();
    let first = map.mmap(0, 10, Protection::READ_WRITE, false).unwrap();
    assert_eq!(
        first.address,
        MMAP_TOP - PAGE,
        "rounded to a page, just below the top"
    );
    assert_eq!(
        first.new,
        Span {
            start: MMAP_TOP - PAGE,
            end: MMAP_TOP,
            protection: Protection::READ_WRITE
        }
    );
    let second = map.mmap(0, 3 * PAGE, Protection::READ, false).unwrap();
    assert_eq!(second.address, MMAP_TOP - 4 * PAGE);
    // A free, aligned hint is honoured; a taken one is not.
    let hinted = map
        .mmap(0x1000_0000, PAGE, Protection::READ, false)
        .unwrap();
    assert_eq!(hinted.address, 0x1000_0000);
    let moved = map
        .mmap(0x1000_0000, PAGE, Protection::READ, false)
        .unwrap();
    assert_ne!(moved.address, 0x1000_0000);
    assert!(map.mmap(0, 0, Protection::READ, false).is_err());
    assert_eq!(
        map.mmap(0, u64::MAX, Protection::READ, false).err(),
        Some(Errno::ENOMEM)
    );
    assert_eq!(
        map.mmap(0, MMAP_TOP, Protection::READ, false).err(),
        Some(Errno::ENOMEM),
        "no room"
    );
    assert_sorted_and_disjoint(&map);
}

#[test]
fn mmap_fixed_replaces_and_reports_what_it_replaced() {
    let mut map = program();
    let base = 0x2000_0000;
    map.mmap(base, 4 * PAGE, Protection::READ_WRITE, true)
        .unwrap();
    // Over the middle two pages.
    let change = map
        .mmap(base + PAGE, 2 * PAGE, Protection::READ, true)
        .unwrap();
    assert_eq!(change.address, base + PAGE);
    assert_eq!(
        change.replaced,
        [Span {
            start: base + PAGE,
            end: base + 3 * PAGE,
            protection: Protection::READ_WRITE
        }]
    );
    let summary: Vec<_> = map
        .regions()
        .iter()
        .filter(|region| region.kind == RegionKind::Anonymous)
        .map(|region| (region.start, region.end, region.protection))
        .collect();
    assert_eq!(
        summary,
        [
            (base, base + PAGE, Protection::READ_WRITE),
            (base + PAGE, base + 3 * PAGE, Protection::READ),
            (base + 3 * PAGE, base + 4 * PAGE, Protection::READ_WRITE),
        ]
    );
    assert_eq!(
        map.mmap(base + 1, PAGE, Protection::READ, true).err(),
        Some(Errno::EINVAL)
    );
    assert_eq!(
        map.mmap(0x1000, PAGE, Protection::READ, true).err(),
        Some(Errno::ENOMEM),
        "below MIN_ADDRESS"
    );
    assert_eq!(
        map.mmap(MMAP_TOP, PAGE, Protection::READ, true).err(),
        Some(Errno::ENOMEM),
        "into the stack"
    );
    assert_sorted_and_disjoint(&map);
}

#[test]
fn munmap_and_mprotect_split_regions() {
    let mut map = program();
    let base = 0x2000_0000;
    map.mmap(base, 8 * PAGE, Protection::READ_WRITE, true)
        .unwrap();
    assert_eq!(
        map.munmap(base + 2 * PAGE, 2 * PAGE).unwrap(),
        [Span {
            start: base + 2 * PAGE,
            end: base + 4 * PAGE,
            protection: Protection::READ_WRITE
        }]
    );
    assert!(map.region_at(base + 2 * PAGE).is_none());
    assert!(map.region_at(base + PAGE).is_some() && map.region_at(base + 4 * PAGE).is_some());
    assert_eq!(
        map.munmap(base + 2 * PAGE, PAGE).unwrap(),
        [],
        "unmapping a hole is fine"
    );
    assert_eq!(map.munmap(base + 1, PAGE).err(), Some(Errno::EINVAL));
    assert_eq!(map.munmap(base, 0).err(), Some(Errno::EINVAL));
    // mprotect needs the whole range mapped.
    assert_eq!(
        map.mprotect(base, 8 * PAGE, Protection::READ).err(),
        Some(Errno::ENOMEM)
    );
    let changed = map
        .mprotect(base + 4 * PAGE, 3 * PAGE, Protection::READ)
        .unwrap();
    assert_eq!(
        changed,
        [Span {
            start: base + 4 * PAGE,
            end: base + 7 * PAGE,
            protection: Protection::READ
        }]
    );
    assert_eq!(
        map.region_at(base + 7 * PAGE).unwrap().protection,
        Protection::READ_WRITE
    );
    assert_eq!(
        map.region_at(base + 6 * PAGE).unwrap().protection,
        Protection::READ
    );
    // Across region boundaries, keeping each piece's kind.
    let image = map.mprotect(IMAGE, 2 * PAGE, Protection::READ).unwrap();
    assert_eq!(image.len(), 2);
    assert_eq!(map.region_at(IMAGE + PAGE).unwrap().kind, RegionKind::Image);
    assert_eq!(map.mprotect(base, 0, Protection::READ).unwrap(), []);
    assert_sorted_and_disjoint(&map);
}

#[test]
fn brk_grows_shrinks_and_refuses_collisions() {
    let mut map = program();
    assert_eq!(map.brk(), BRK_START);
    assert_eq!(map.set_brk(BRK_START), Ok(BrkChange::Unchanged));
    assert_eq!(
        map.set_brk(BRK_START + 100),
        Ok(BrkChange::Map(Span {
            start: BRK_START,
            end: BRK_START + PAGE,
            protection: Protection::READ_WRITE
        }))
    );
    assert_eq!(map.brk(), BRK_START + 100, "the exact break is kept");
    assert_eq!(
        map.set_brk(BRK_START + 200),
        Ok(BrkChange::Unchanged),
        "same page"
    );
    assert_eq!(
        map.set_brk(BRK_START + 3 * PAGE),
        Ok(BrkChange::Map(Span {
            start: BRK_START + PAGE,
            end: BRK_START + 3 * PAGE,
            protection: Protection::READ_WRITE
        }))
    );
    assert_eq!(
        map.set_brk(BRK_START + PAGE),
        Ok(BrkChange::Unmap(Span {
            start: BRK_START + PAGE,
            end: BRK_START + 3 * PAGE,
            protection: Protection::READ_WRITE
        }))
    );
    let heap: Vec<_> = map
        .regions()
        .iter()
        .filter(|region| region.kind == RegionKind::Heap)
        .collect();
    assert_eq!(heap.len(), 1);
    assert_eq!((heap[0].start, heap[0].end), (BRK_START, BRK_START + PAGE));
    assert_eq!(
        map.set_brk(BRK_START - 1),
        Err(Errno::ENOMEM),
        "below the start"
    );
    // A mapping in the way stops growth; the break stays.
    map.mmap(BRK_START + 4 * PAGE, PAGE, Protection::READ, true)
        .unwrap();
    assert_eq!(map.set_brk(BRK_START + 5 * PAGE), Err(Errno::ENOMEM));
    assert_eq!(map.brk(), BRK_START + PAGE);
    assert_eq!(map.set_brk(u64::MAX), Err(Errno::ENOMEM));
    assert_eq!(
        map.set_brk(BRK_START),
        Ok(BrkChange::Unmap(Span {
            start: BRK_START,
            end: BRK_START + PAGE,
            protection: Protection::READ_WRITE
        }))
    );
    assert!(
        map.regions()
            .iter()
            .all(|region| region.kind != RegionKind::Heap)
    );
    assert_sorted_and_disjoint(&map);
}

/// Reference model: one entry per mapped page.
type Pages = BTreeMap<u64, Protection>;

fn model_pages(map: &MemoryMap) -> Pages {
    let mut pages = Pages::new();
    for region in map.regions() {
        for page in (region.start..region.end).step_by(PAGE as usize) {
            pages.insert(page, region.protection);
        }
    }
    pages
}

fn span_pages(spans: &[Span]) -> Vec<u64> {
    spans
        .iter()
        .flat_map(|span| (span.start..span.end).step_by(PAGE as usize))
        .collect()
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

#[test]
fn random_operations_match_a_page_model() {
    let steps = if cfg!(miri) { 60 } else { 20_000 };
    // A small window so operations collide often.
    let window = 0x3000_0000u64;
    let pages_in_window = 64;
    for seed in [1u64, 0x5ae0, 0xdead_beef] {
        let mut rng = Rng(seed);
        let mut map = program();
        let mut model = model_pages(&map);
        let protections = [
            Protection::NONE,
            Protection::READ,
            Protection::READ_WRITE,
            Protection::READ_EXECUTE,
        ];
        for step in 0..steps {
            let start = window + rng.below(pages_in_window) * PAGE;
            let length = (1 + rng.below(8)) * PAGE;
            let protection = protections[rng.below(4) as usize];
            match rng.below(4) {
                0 => {
                    let change = map.mmap(start, length, protection, true).unwrap();
                    // Replaced pages were exactly the mapped ones in range.
                    let expected: Vec<u64> = model
                        .range(start..start + length)
                        .map(|(&page, _)| page)
                        .collect();
                    assert_eq!(
                        span_pages(&change.replaced),
                        expected,
                        "seed {seed} step {step}"
                    );
                    for page in (start..start + length).step_by(PAGE as usize) {
                        model.insert(page, protection);
                    }
                }
                1 => {
                    let removed = map.munmap(start, length).unwrap();
                    let expected: Vec<u64> = model
                        .range(start..start + length)
                        .map(|(&page, _)| page)
                        .collect();
                    assert_eq!(span_pages(&removed), expected, "seed {seed} step {step}");
                    for page in expected {
                        model.remove(&page);
                    }
                }
                2 => {
                    let covered = (start..start + length)
                        .step_by(PAGE as usize)
                        .all(|page| model.contains_key(&page));
                    match map.mprotect(start, length, protection) {
                        Ok(changed) => {
                            assert!(
                                covered,
                                "seed {seed} step {step}: mprotect over a hole succeeded"
                            );
                            assert_eq!(span_pages(&changed).len() as u64, length / PAGE);
                            for page in (start..start + length).step_by(PAGE as usize) {
                                model.insert(page, protection);
                            }
                        }
                        Err(error) => {
                            assert!(!covered, "seed {seed} step {step}: {error:?}");
                            assert_eq!(error, Errno::ENOMEM);
                        }
                    }
                }
                _ => {
                    // Non-fixed placement lands in free space (then goes
                    // again, so the map stays small).
                    let change = map.mmap(0, length, protection, false).unwrap();
                    assert!(change.replaced.is_empty());
                    for page in (change.new.start..change.new.end).step_by(PAGE as usize) {
                        assert!(
                            !model.contains_key(&page),
                            "seed {seed} step {step}: overlapped"
                        );
                    }
                    let removed = map.munmap(change.new.start, length).unwrap();
                    assert_eq!(span_pages(&removed).len() as u64, length / PAGE);
                }
            }
            assert_eq!(model_pages(&map), model, "seed {seed} step {step}");
            assert_sorted_and_disjoint(&map);
        }
        assert!(
            map.regions()
                .iter()
                .all(|region| region.start >= MIN_ADDRESS)
        );
    }
}
