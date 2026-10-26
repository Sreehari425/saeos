//! Buddy free lists over an authoritative per-frame state bitmap.
//!
//! The bitmap (owned by the `FrameStore`) decides who owns a frame; the free
//! lists only make finding free blocks fast. Each list node lives in the first
//! bytes of the free block it describes, so the lists cost no metadata.
//!
//! Invariant: a frame's node carries its head tag if and only if that frame
//! heads a block on a free list. Popping or merging a block clears its magic,
//! so a frame inside a larger free block can never be mistaken for a head.
//!
//! The same code backs the kernel allocator (bitmap in the boot arena, nodes
//! through the direct map) and the host tests in `tools/host-tests`.

pub const MAX_ORDER: usize = 18;
/// Frames below 4 GiB form the `Dma32` zone. The boundary is aligned to every
/// order up to `MAX_ORDER`, so a block and its buddy never span zones.
pub const DMA32_FRAMES: usize = 1 << 20;
pub const ZONE_COUNT: usize = 2;
const NONE: usize = usize::MAX;
pub const FREE_MAGIC: u64 = 0x5341_454f_4255_4459;

/// Head tag for `frame`. Mixing in the frame number means a node image that
/// ends up in another frame (copied memory, stale data) never validates.
const fn head_tag(frame: usize) -> u64 {
    FREE_MAGIC ^ frame as u64
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Reserved = 0,
    Free = 1,
    Allocated = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Dma32 = 0,
    Normal = 1,
}

impl Zone {
    pub const fn of(frame: usize) -> Self {
        if frame < DMA32_FRAMES {
            Zone::Dma32
        } else {
            Zone::Normal
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseError {
    OutOfRange,
    Unaligned,
    DuplicateRelease,
    DoubleFree,
    InvalidRange,
}

/// Node stored in the first bytes of a free block.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FreeNode {
    pub magic: u64,
    pub order: u32,
    pub _reserved: u32,
    pub prev: usize,
    pub next: usize,
}

/// Storage the buddy lists operate on: the ownership bitmap plus access to
/// the first bytes of every frame.
pub trait FrameStore {
    fn frame_count(&self) -> usize;
    fn state(&self, frame: usize) -> State;
    fn set_state(&mut self, frame: usize, state: State);
    /// The node slot at the start of `frame`. Only called for frames the
    /// allocator owns as free.
    fn node(&mut self, frame: usize) -> *mut FreeNode;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuddyLists {
    heads: [[usize; MAX_ORDER + 1]; ZONE_COUNT],
    counts: [[usize; MAX_ORDER + 1]; ZONE_COUNT],
}

impl Default for BuddyLists {
    fn default() -> Self {
        Self::new()
    }
}

impl BuddyLists {
    pub const fn new() -> Self {
        Self {
            heads: [[NONE; MAX_ORDER + 1]; ZONE_COUNT],
            counts: [[0; MAX_ORDER + 1]; ZONE_COUNT],
        }
    }

    /// Number of free blocks of `order` in `zone`.
    pub fn count(&self, zone: Zone, order: usize) -> usize {
        self.counts[zone as usize][order]
    }

    pub fn free_frames(&self) -> usize {
        (0..ZONE_COUNT)
            .flat_map(|zone| (0..=MAX_ORDER).map(move |order| (zone, order)))
            .map(|(zone, order)| self.counts[zone][order] << order)
            .sum()
    }

    pub fn largest_order(&self) -> Option<usize> {
        (0..=MAX_ORDER)
            .rev()
            .find(|order| (0..ZONE_COUNT).any(|zone| self.counts[zone][*order] != 0))
    }

    /// Rebuild the lists from the bitmap: every maximal run of free frames is
    /// carved into the largest aligned blocks that fit.
    pub fn build(&mut self, store: &mut impl FrameStore) {
        *self = Self::new();
        let count = store.frame_count();
        let mut frame = 0;
        while frame < count {
            if store.state(frame) != State::Free {
                frame += 1;
                continue;
            }
            let mut end = frame;
            while end < count && store.state(end) == State::Free {
                end += 1;
            }
            self.insert_run(store, frame, end);
            frame = end;
        }
    }

    /// Allocate a block of `1 << order` frames from the first zone in `zones`
    /// that has one, marking it allocated in the bitmap.
    pub fn allocate(
        &mut self,
        store: &mut impl FrameStore,
        order: usize,
        zones: &[Zone],
    ) -> Option<usize> {
        if order > MAX_ORDER {
            return None;
        }
        for zone in zones {
            for source in order..=MAX_ORDER {
                let head = self.heads[*zone as usize][source];
                if head == NONE {
                    continue;
                }
                debug_assert_eq!(
                    store.state(head),
                    State::Free,
                    "free-list head {head} not free"
                );
                self.unlink(store, head, *zone, source);
                let mut current = source;
                while current > order {
                    current -= 1;
                    self.push(store, head + (1 << current), current);
                }
                for frame in head..head + (1 << order) {
                    store.set_state(frame, State::Allocated);
                }
                return Some(head);
            }
        }
        None
    }

    /// Release an allocated block and merge it with free buddies.
    pub fn release(
        &mut self,
        store: &mut impl FrameStore,
        frame: usize,
        order: usize,
    ) -> Result<(), ReleaseError> {
        if order > MAX_ORDER {
            return Err(ReleaseError::InvalidRange);
        }
        let count = 1usize << order;
        if frame
            .checked_add(count)
            .is_none_or(|end| end > store.frame_count())
        {
            return Err(ReleaseError::OutOfRange);
        }
        if !frame.is_multiple_of(count) {
            return Err(ReleaseError::Unaligned);
        }
        let all = |store: &mut _, state| range_is(store, frame, count, state);
        if all(store, State::Reserved) {
            return Err(ReleaseError::DuplicateRelease);
        }
        if all(store, State::Free) {
            return Err(ReleaseError::DoubleFree);
        }
        if !all(store, State::Allocated) {
            return Err(ReleaseError::InvalidRange);
        }
        for index in frame..frame + count {
            store.set_state(index, State::Free);
        }
        self.coalesce_and_push(store, frame, order);
        Ok(())
    }

    /// Turn `frame..end` free in the bitmap and add it to the lists. Used for
    /// memory that was reserved during boot and is handed back later.
    pub fn insert_reserved_range(&mut self, store: &mut impl FrameStore, frame: usize, end: usize) {
        let end = end.min(store.frame_count());
        let mut index = frame;
        while index < end {
            if store.state(index) == State::Reserved {
                let start = index;
                while index < end && store.state(index) == State::Reserved {
                    index += 1;
                }
                // Free and merge one aligned piece at a time, so pieces not
                // yet listed stay Reserved and can never look like heads.
                let mut cursor = start;
                while cursor < index {
                    let order = block_order(cursor, index);
                    for frame in cursor..cursor + (1 << order) {
                        store.set_state(frame, State::Free);
                    }
                    self.coalesce_and_push(store, cursor, order);
                    cursor += 1 << order;
                }
            } else {
                index += 1;
            }
        }
    }

    /// Take one free frame out of the lists without changing its bitmap
    /// state, splitting its containing block. Returns false if the frame is
    /// not free.
    pub fn isolate(&mut self, store: &mut impl FrameStore, frame: usize) -> bool {
        if frame >= store.frame_count() || store.state(frame) != State::Free {
            return false;
        }
        for order in 0..=MAX_ORDER {
            let mut head = frame & !((1usize << order) - 1);
            if !self.is_free_head(store, head, order) {
                continue;
            }
            self.unlink(store, head, Zone::of(head), order);
            let mut current = order;
            while current > 0 {
                current -= 1;
                let upper = head + (1 << current);
                if frame >= upper {
                    self.push(store, head, current);
                    head = upper;
                } else {
                    self.push(store, upper, current);
                }
            }
            return true;
        }
        panic!("free frame {frame} is not covered by any free block");
    }

    /// Walk every list and cross-check it against the bitmap.
    pub fn validate(&self, store: &mut impl FrameStore) -> Result<(), &'static str> {
        let mut listed = 0usize;
        for zone in 0..ZONE_COUNT {
            for order in 0..=MAX_ORDER {
                let mut previous = NONE;
                let mut current = self.heads[zone][order];
                let mut seen = 0;
                while current != NONE {
                    seen += 1;
                    if seen > store.frame_count() {
                        return Err("free list is cyclic");
                    }
                    if !current.is_multiple_of(1 << order) || Zone::of(current) as usize != zone {
                        return Err("free block misaligned or in the wrong zone");
                    }
                    if !range_is(store, current, 1 << order, State::Free) {
                        return Err("listed block not free in the bitmap");
                    }
                    let node = unsafe { *store.node(current) };
                    if node.magic != head_tag(current) || node.order as usize != order {
                        return Err("free block header corrupted");
                    }
                    if node.prev != previous {
                        return Err("free list back link broken");
                    }
                    previous = current;
                    current = node.next;
                }
                if seen != self.counts[zone][order] {
                    return Err("free list count mismatch");
                }
                listed += seen << order;
            }
        }
        let free_in_bitmap = (0..store.frame_count())
            .filter(|frame| store.state(*frame) == State::Free)
            .count();
        if listed != free_in_bitmap {
            return Err("free lists do not cover every free frame");
        }
        Ok(())
    }

    fn insert_run(&mut self, store: &mut impl FrameStore, mut frame: usize, end: usize) {
        while frame < end {
            let order = block_order(frame, end);
            self.push(store, frame, order);
            frame += 1 << order;
        }
    }

    fn coalesce_and_push(
        &mut self,
        store: &mut impl FrameStore,
        mut frame: usize,
        mut order: usize,
    ) {
        while order < MAX_ORDER {
            let buddy = frame ^ (1 << order);
            if !self.is_free_head(store, buddy, order) {
                break;
            }
            self.unlink(store, buddy, Zone::of(buddy), order);
            frame = frame.min(buddy);
            order += 1;
        }
        self.push(store, frame, order);
    }

    fn is_free_head(&self, store: &mut impl FrameStore, frame: usize, order: usize) -> bool {
        if frame
            .checked_add(1 << order)
            .is_none_or(|end| end > store.frame_count())
            || store.state(frame) != State::Free
        {
            return false;
        }
        let node = unsafe { *store.node(frame) };
        node.magic == head_tag(frame) && node.order as usize == order
    }

    fn push(&mut self, store: &mut impl FrameStore, frame: usize, order: usize) {
        let zone = Zone::of(frame) as usize;
        let head = self.heads[zone][order];
        unsafe {
            store.node(frame).write(FreeNode {
                magic: head_tag(frame),
                order: order as u32,
                _reserved: 0,
                prev: NONE,
                next: head,
            });
            if head != NONE {
                (*store.node(head)).prev = frame;
            }
        }
        self.heads[zone][order] = frame;
        self.counts[zone][order] += 1;
    }

    fn unlink(&mut self, store: &mut impl FrameStore, frame: usize, zone: Zone, order: usize) {
        let zone = zone as usize;
        let node = store.node(frame);
        let (prev, next) = unsafe { ((*node).prev, (*node).next) };
        unsafe {
            if prev == NONE {
                self.heads[zone][order] = next;
            } else {
                (*store.node(prev)).next = next;
            }
            if next != NONE {
                (*store.node(next)).prev = prev;
            }
            (*store.node(frame)).magic = 0;
        }
        self.counts[zone][order] -= 1;
    }
}

/// Largest order whose aligned block starting at `frame` fits before `end`.
fn block_order(frame: usize, end: usize) -> usize {
    let mut order = if frame == 0 {
        MAX_ORDER
    } else {
        MAX_ORDER.min(frame.trailing_zeros() as usize)
    };
    while frame + (1 << order) > end {
        order -= 1;
    }
    order
}

fn range_is(store: &mut impl FrameStore, frame: usize, count: usize, state: State) -> bool {
    (frame..frame + count).all(|index| store.state(index) == state)
}
