//! Temporary boot-time frame holds: usable memory that must not be handed
//! out yet (the UEFI firmware stack, the initramfs the bootloader loaded)
//! but is released to the allocator later.
//!
//! A hold only ever covers frames it took from the free pool itself, and a
//! permanent reservation made afterwards always wins: its frames are
//! punched out of every hold, so releasing a hold can never free the kernel
//! image, page tables, or anything else reserved for good. Pure bookkeeping
//! over frame indices; the allocator applies the result.

/// Ranges across all holds. Punching a hole in the middle of a range splits
/// it, so this is larger than the number of holds.
pub const MAX_HOLD_RANGES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldId(u32);

#[derive(Debug, Clone, Copy)]
struct Range {
    id: HoldId,
    first: usize,
    last: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct HoldTable {
    ranges: [Option<Range>; MAX_HOLD_RANGES],
    next_id: u32,
}

impl Default for HoldTable {
    fn default() -> Self {
        Self::new()
    }
}

impl HoldTable {
    pub const fn new() -> Self {
        Self {
            ranges: [None; MAX_HOLD_RANGES],
            next_id: 0,
        }
    }

    pub fn new_id(&mut self) -> HoldId {
        self.next_id += 1;
        HoldId(self.next_id)
    }

    /// Record frames `first..last` under `id`. False if the table is full
    /// (the caller then keeps the frames reserved for good).
    pub fn add(&mut self, id: HoldId, first: usize, last: usize) -> bool {
        if first >= last {
            return true;
        }
        match self.ranges.iter_mut().find(|slot| slot.is_none()) {
            Some(slot) => {
                *slot = Some(Range { id, first, last });
                true
            }
            None => false,
        }
    }

    /// Remove `first..last` from every hold: those frames now belong to a
    /// permanent reservation. False if a split did not fit; the piece that
    /// could not be recorded stays reserved (leaked, never wrongly freed).
    pub fn punch(&mut self, first: usize, last: usize) -> bool {
        let mut complete = true;
        for index in 0..MAX_HOLD_RANGES {
            let Some(range) = self.ranges[index] else {
                continue;
            };
            if range.last <= first || last <= range.first {
                continue;
            }
            let left = (range.first < first).then_some((range.first, first));
            let right = (last < range.last).then_some((last, range.last));
            self.ranges[index] = left.or(right).map(|(first, last)| Range {
                id: range.id,
                first,
                last,
            });
            if let (Some(_), Some((first, last))) = (left, right) {
                complete &= self.add(range.id, first, last);
            }
        }
        complete
    }

    /// Drop every range of `id`, passing each to `release`.
    pub fn release(&mut self, id: HoldId, mut release: impl FnMut(usize, usize)) {
        for slot in &mut self.ranges {
            if let Some(range) = *slot
                && range.id == id
            {
                *slot = None;
                release(range.first, range.last);
            }
        }
    }

    /// Frames still held, across all holds.
    pub fn held_frames(&self) -> usize {
        self.ranges
            .iter()
            .flatten()
            .map(|range| range.last - range.first)
            .sum()
    }
}
