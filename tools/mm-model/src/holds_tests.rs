//! Boot-time frame holds: a permanent reservation must always win.

use crate::holds::{HoldTable, MAX_HOLD_RANGES};

fn released(table: &mut HoldTable, id: crate::holds::HoldId) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    table.release(id, |first, last| ranges.push((first, last)));
    ranges.sort();
    ranges
}

#[test]
fn release_returns_only_the_holds_own_frames() {
    let mut table = HoldTable::new();
    let stack = table.new_id();
    let initramfs = table.new_id();
    assert!(table.add(stack, 0, 100));
    assert!(table.add(initramfs, 200, 210));
    assert!(table.add(initramfs, 220, 230));
    assert_eq!(table.held_frames(), 120);
    assert_eq!(released(&mut table, initramfs), [(200, 210), (220, 230)]);
    assert_eq!(released(&mut table, initramfs), [], "released once");
    assert_eq!(table.held_frames(), 100);
}

#[test]
fn permanent_reservations_punch_holes() {
    let mut table = HoldTable::new();
    let id = table.new_id();
    table.add(id, 0, 100);
    // The kernel image lands in the middle of the held region, then a page
    // table at its start and a range overlapping its end.
    assert!(table.punch(40, 60));
    assert!(table.punch(0, 1));
    assert!(table.punch(90, 150));
    assert!(table.punch(500, 600), "no overlap");
    assert_eq!(table.held_frames(), 39 + 30);
    assert_eq!(released(&mut table, id), [(1, 40), (60, 90)]);
    assert_eq!(table.held_frames(), 0);
}

#[test]
fn a_full_table_leaks_instead_of_freeing_reserved_frames() {
    let mut table = HoldTable::new();
    let id = table.new_id();
    table.add(id, 0, 10_000);
    // Each punch in the middle needs one more slot.
    for index in 1..MAX_HOLD_RANGES {
        assert!(table.punch(index * 100, index * 100 + 1));
    }
    assert!(!table.add(id, 20_000, 20_001), "table is full");
    assert!(!table.punch(9_000, 9_001), "split did not fit");
    // Nothing returned overlaps a punched frame.
    let ranges = released(&mut table, id);
    let punched: Vec<usize> = (1..MAX_HOLD_RANGES)
        .map(|index| index * 100)
        .chain([9_000])
        .collect();
    for (first, last) in ranges {
        assert!(punched.iter().all(|&frame| !(first..last).contains(&frame)));
    }
}
