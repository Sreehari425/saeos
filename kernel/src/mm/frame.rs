//! Physical-frame ownership and buddy allocation.
//!
//! A two-bit-per-frame bitmap is authoritative for ownership (reserved, free,
//! allocated). Until the direct map is live (`activate_metadata`), free frames
//! cannot be touched, so allocation is a next-fit scan of the bitmap. After
//! that, `buddy::BuddyLists` keeps intrusive per-zone free lists inside the
//! free frames themselves and allocation and release are O(MAX_ORDER).
//!
//! Each tracked frame also has a 16-bit reference count once the direct map
//! is live; `free_frame` drops one reference and the last one releases.
use crate::boot::boot_info::{PhysicalMemoryKind, PhysicalMemoryMap};
use crate::mm::buddy::{self, BuddyLists, FrameStore, FreeNode, ReleaseError, State, Zone};
use crate::mm::holds::{HoldId, HoldTable};
use crate::sync::IrqSpinMutex;
use core::sync::atomic::{AtomicPtr, AtomicU16, AtomicUsize, Ordering};

pub use crate::mm::buddy::MAX_ORDER;

pub const PAGE_SIZE: u64 = 4096;

/// Physical memory tracked by the allocator. RAM above this is ignored with
/// a warning; the bitmap needs `MAX_TRACKED_PHYS / PAGE_SIZE / 4` bytes.
pub const MAX_TRACKED_PHYS: u64 = 64 << 30;

// UEFI keeps the loaded image mapped before the kernel installs its own
// tables. This image-backed arena holds the compact ownership bitmap during
// that transition; the ordinary heap remains independent.
const BOOT_METADATA_ARENA_SIZE: usize = (MAX_TRACKED_PHYS / PAGE_SIZE / 4) as usize;
#[repr(C, align(4096))]
struct BootMetadataArena([u8; BOOT_METADATA_ARENA_SIZE]);
static mut BOOT_METADATA_ARENA: BootMetadataArena =
    BootMetadataArena([0; BOOT_METADATA_ARENA_SIZE]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct PhysAddr(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct VirtAddr(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct PhysFrame(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocationOrder(pub usize);
impl AllocationOrder {
    pub const ZERO: Self = Self(0);
    pub const fn pages(self) -> u64 {
        1u64 << self.0
    }
    pub const fn bytes(self) -> u64 {
        self.pages() * PAGE_SIZE
    }
    pub const fn is_valid(self) -> bool {
        self.0 <= MAX_ORDER
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocationConstraints {
    pub minimum: Option<PhysAddr>,
    pub maximum: Option<PhysAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AllocatorStats {
    pub total_frames: u64,
    pub tracked_frames: u64,
    pub unsupported_frames: u64,
    pub usable_frames: u64,
    pub allocated_frames: u64,
    pub reserved_frames: u64,
    pub free_frames: u64,
    pub largest_contiguous_block: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    Unaligned,
    Overflow,
    InvalidRange,
    Overlap,
    DuplicateRelease,
    DoubleFree,
    OutOfMemory,
    InvalidOrder,
    UnsupportedAddress,
    /// The frame is not allocated or its reference count is saturated.
    NotShareable,
}

impl From<ReleaseError> for FrameError {
    fn from(error: ReleaseError) -> Self {
        match error {
            ReleaseError::OutOfRange => FrameError::UnsupportedAddress,
            ReleaseError::Unaligned => FrameError::Unaligned,
            ReleaseError::DuplicateRelease => FrameError::DuplicateRelease,
            ReleaseError::DoubleFree => FrameError::DoubleFree,
            ReleaseError::InvalidRange => FrameError::InvalidRange,
        }
    }
}

#[derive(Clone, Copy)]
struct Metadata {
    physical_start: u64,
    mapped_start: u64,
    frame_count: usize,
    pages: usize,
}

impl Metadata {
    const EMPTY: Self = Self {
        physical_start: 0,
        mapped_start: 0,
        frame_count: 0,
        pages: 0,
    };

    fn address(&self, offset: usize) -> *mut u8 {
        (self.mapped_start + offset as u64) as *mut u8
    }

    fn clear(&self) {
        unsafe { core::ptr::write_bytes(self.address(0), 0, self.pages * PAGE_SIZE as usize) }
    }

    fn state(&self, frame: usize) -> State {
        let shift = (frame & 3) * 2;
        let bitmap = unsafe { *self.address(frame / 4) };
        match (bitmap >> shift) & 3 {
            1 => State::Free,
            2 => State::Allocated,
            _ => State::Reserved,
        }
    }

    fn set_state(&self, frame: usize, state: State) {
        let shift = (frame & 3) * 2;
        let byte = self.address(frame / 4);
        unsafe {
            *byte = (*byte & !(3 << shift)) | ((state as u8) << shift);
        }
    }
}

/// Bitmap plus direct-map access to frame contents, for the buddy core.
struct ArenaStore(Metadata);

impl FrameStore for ArenaStore {
    fn frame_count(&self) -> usize {
        self.0.frame_count
    }
    fn state(&self, frame: usize) -> State {
        self.0.state(frame)
    }
    fn set_state(&mut self, frame: usize, state: State) {
        self.0.set_state(frame, state)
    }
    fn node(&mut self, frame: usize) -> *mut FreeNode {
        crate::mm::paging::phys_to_virt(PhysAddr(frame as u64 * PAGE_SIZE)).0 as *mut FreeNode
    }
}

struct Allocator {
    map: PhysicalMemoryMap,
    metadata: Metadata,
    /// Next-fit cursor for the pre-buddy bitmap scan, as a frame index.
    scan_cursor: usize,
    lists: BuddyLists,
    buddy_active: bool,
    /// Usable memory pinned during boot (the UEFI firmware stack, the
    /// initramfs) and handed back once it is no longer needed.
    holds: HoldTable,
}

impl Allocator {
    const fn empty() -> Self {
        Self {
            map: PhysicalMemoryMap::empty(),
            metadata: Metadata::EMPTY,
            scan_cursor: 0,
            lists: BuddyLists::new(),
            buddy_active: false,
            holds: HoldTable::new(),
        }
    }
    fn store(&self) -> ArenaStore {
        ArenaStore(self.metadata)
    }
    fn index(&self, address: u64) -> Result<usize, FrameError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(FrameError::Unaligned);
        }
        let index =
            usize::try_from(address / PAGE_SIZE).map_err(|_| FrameError::UnsupportedAddress)?;
        (index < self.metadata.frame_count)
            .then_some(index)
            .ok_or(FrameError::UnsupportedAddress)
    }
    fn endpoint(&self, address: u64) -> Result<usize, FrameError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(FrameError::Unaligned);
        }
        usize::try_from(address / PAGE_SIZE)
            .map_err(|_| FrameError::UnsupportedAddress)
            .and_then(|index| {
                (index <= self.metadata.frame_count)
                    .then_some(index)
                    .ok_or(FrameError::UnsupportedAddress)
            })
    }
    fn state(&self, frame: usize) -> State {
        self.metadata.state(frame)
    }
    fn set_state(&self, frame: usize, state: State) {
        self.metadata.set_state(frame, state)
    }
    fn range_state(&self, start: usize, count: usize, expected: State) -> bool {
        start
            .checked_add(count)
            .is_some_and(|end| end <= self.metadata.frame_count)
            && (start..start + count).all(|frame| self.state(frame) == expected)
    }
    fn reserve(&mut self, start: u64, end: u64) -> Result<(), FrameError> {
        if start >= end || !start.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) {
            return Err(FrameError::InvalidRange);
        }
        debug_assert!(
            end <= 1 << 52,
            "virtual address {start:#x} reserved as physical"
        );
        let first = self.index(start)?;
        let last = self.endpoint(end)?;
        if first >= last {
            return Err(FrameError::InvalidRange);
        }
        // Boot code may reserve the same image/table range more than once.
        // An already-permanent range is idempotent; allocated/free overlap is
        // still rejected.
        if (first..last).any(|frame| self.state(frame) == State::Allocated) {
            return Err(FrameError::Overlap);
        }
        let mut store = self.store();
        for frame in first..last {
            if self.state(frame) == State::Free {
                if self.buddy_active {
                    self.lists.isolate(&mut store, frame);
                }
                self.set_state(frame, State::Reserved);
            }
        }
        Ok(())
    }
    fn allocate(
        &mut self,
        order: AllocationOrder,
        min: u64,
        max: u64,
    ) -> Result<PhysFrame, FrameError> {
        if !order.is_valid() {
            return Err(FrameError::InvalidOrder);
        }
        let bytes = order.bytes();
        let fits = |frame: usize| {
            let address = frame as u64 * PAGE_SIZE;
            address >= min && address.checked_add(bytes).is_some_and(|end| end <= max)
        };
        let frame = if self.buddy_active {
            let mut store = self.store();
            let zones = zones_for(min, max);
            match self.lists.allocate(&mut store, order.0, zones) {
                Some(frame) if fits(frame) => frame,
                Some(frame) => {
                    // The zone matched but the exact bounds did not (rare,
                    // non-4-GiB constraints): give it back and search.
                    self.lists
                        .release(&mut store, frame, order.0)
                        .expect("returning an unused block");
                    self.allocate_scan(order, min, max)?
                }
                None => return Err(FrameError::OutOfMemory),
            }
        } else {
            self.allocate_scan(order, min, max)?
        };
        set_refcounts(frame, 1 << order.0, 1);
        Ok(PhysFrame(frame as u64 * PAGE_SIZE))
    }
    /// Next-fit bitmap search for an aligned free run within bounds. Used
    /// before the free lists exist and for constraints the zones can't serve.
    fn allocate_scan(
        &mut self,
        order: AllocationOrder,
        min: u64,
        max: u64,
    ) -> Result<usize, FrameError> {
        let count = 1usize << order.0;
        let frame_count = self.metadata.frame_count;
        let low = usize::try_from(min.div_ceil(PAGE_SIZE))
            .unwrap_or(usize::MAX)
            .next_multiple_of(count);
        let high = usize::try_from(max / PAGE_SIZE)
            .unwrap_or(usize::MAX)
            .min(frame_count);
        if low >= high || high - low < count {
            return Err(FrameError::OutOfMemory);
        }
        let start = self.scan_cursor.clamp(low, high).next_multiple_of(count);
        let found = (start..high)
            .step_by(count)
            .chain((low..start).step_by(count))
            .find(|&frame| frame + count <= high && self.range_state(frame, count, State::Free))
            .ok_or(FrameError::OutOfMemory)?;
        let mut store = self.store();
        for frame in found..found + count {
            if self.buddy_active {
                self.lists.isolate(&mut store, frame);
            }
            self.set_state(frame, State::Allocated);
        }
        self.scan_cursor = found + count;
        Ok(found)
    }
    fn release(&mut self, frame: PhysFrame, order: AllocationOrder) -> Result<(), FrameError> {
        if !order.is_valid() {
            return Err(FrameError::InvalidOrder);
        }
        let start = self.index(frame.0)?;
        if self.buddy_active {
            let mut store = self.store();
            return Ok(self.lists.release(&mut store, start, order.0)?);
        }
        let count = 1usize << order.0;
        if !start.is_multiple_of(count) {
            return Err(FrameError::Unaligned);
        }
        if self.range_state(start, count, State::Reserved) {
            return Err(FrameError::DuplicateRelease);
        }
        if self.range_state(start, count, State::Free) {
            return Err(FrameError::DoubleFree);
        }
        if !self.range_state(start, count, State::Allocated) {
            return Err(FrameError::InvalidRange);
        }
        for index in start..start + count {
            self.set_state(index, State::Free);
        }
        Ok(())
    }
    /// Hand reserved frames back to the free pool.
    fn unreserve(&mut self, first: usize, last: usize) {
        if self.buddy_active {
            let mut store = self.store();
            self.lists.insert_reserved_range(&mut store, first, last);
        } else {
            for frame in first..last.min(self.metadata.frame_count) {
                if self.state(frame) == State::Reserved {
                    self.set_state(frame, State::Free);
                }
            }
        }
    }
}

/// Zones that can satisfy `min..max`. Normal is preferred so the scarcer
/// Dma32 frames stay available for devices that need them.
fn zones_for(min: u64, max: u64) -> &'static [Zone] {
    const BOUNDARY: u64 = buddy::DMA32_FRAMES as u64 * PAGE_SIZE;
    if max <= BOUNDARY {
        &[Zone::Dma32]
    } else if min >= BOUNDARY {
        &[Zone::Normal]
    } else {
        &[Zone::Normal, Zone::Dma32]
    }
}

static ALLOCATOR: IrqSpinMutex<Allocator> = IrqSpinMutex::new(Allocator::empty());

pub fn init(map: PhysicalMemoryMap) -> Result<(), FrameError> {
    let mut allocator = ALLOCATOR.lock();
    // Ignore sparse high MMIO/reserved descriptors; only usable RAM needs
    // ownership metadata and UEFI commonly reports those descriptors above
    // the actual RAM ceiling.
    let highest = map
        .usable()
        .map(|region| region.end())
        .max()
        .ok_or(FrameError::InvalidRange)?;
    if highest > MAX_TRACKED_PHYS {
        crate::serial_println!(
            "Memory: tracking first {} GiB of {} GiB; memory above that is ignored.",
            MAX_TRACKED_PHYS >> 30,
            highest.div_ceil(1 << 30)
        );
    }
    let frame_count = usize::try_from(highest.min(MAX_TRACKED_PHYS).div_ceil(PAGE_SIZE))
        .map_err(|_| FrameError::UnsupportedAddress)?;
    if frame_count == 0 {
        return Err(FrameError::InvalidRange);
    }
    let bitmap_bytes = frame_count
        .checked_mul(2)
        .ok_or(FrameError::Overflow)?
        .div_ceil(8);
    let pages = bitmap_bytes.div_ceil(PAGE_SIZE as usize);
    let metadata_bytes = pages
        .checked_mul(PAGE_SIZE as usize)
        .ok_or(FrameError::Overflow)?;
    // The arena is reached through the image mapping until the direct map is
    // live; its frames are tracked by physical address.
    let metadata_virtual = core::ptr::addr_of!(BOOT_METADATA_ARENA) as u64;
    let metadata_start = crate::mm::layout::image_virt_to_phys(metadata_virtual);
    if !metadata_start.is_multiple_of(PAGE_SIZE) || metadata_bytes > BOOT_METADATA_ARENA_SIZE {
        crate::serial_println!(
            "Memory: metadata range invalid start={:#x} bytes={} arena={}",
            metadata_start,
            metadata_bytes,
            BOOT_METADATA_ARENA_SIZE
        );
        return Err(FrameError::OutOfMemory);
    }

    allocator.map = map;
    allocator.metadata = Metadata {
        physical_start: metadata_start,
        mapped_start: metadata_virtual,
        frame_count,
        pages,
    };
    allocator.metadata.clear();
    for region in map.usable() {
        let start = region.start.max(PAGE_SIZE).div_ceil(PAGE_SIZE);
        let end = region.end() / PAGE_SIZE;
        for frame in start..end {
            if let Ok(frame) = usize::try_from(frame)
                && frame < allocator.metadata.frame_count
            {
                allocator.set_state(frame, State::Free);
            }
        }
    }
    let metadata_first =
        usize::try_from(metadata_start / PAGE_SIZE).map_err(|_| FrameError::UnsupportedAddress)?;
    for frame in metadata_first..metadata_first.saturating_add(pages) {
        if frame < allocator.metadata.frame_count {
            allocator.set_state(frame, State::Reserved);
        }
    }
    allocator.scan_cursor = 0;
    Ok(())
}

/// Hold the currently free frames of `start..end` (rounded out to pages)
/// until [`release_hold`]. Frames that are already reserved or allocated
/// are not part of the hold, and later permanent reservations are punched
/// out of it, so releasing it only ever frees what it took.
pub fn hold_range(start: u64, end: u64) -> Option<HoldId> {
    let mut allocator = ALLOCATOR.lock();
    let first = (start / PAGE_SIZE) as usize;
    let last = (end.div_ceil(PAGE_SIZE) as usize).min(allocator.metadata.frame_count);
    if first >= last {
        return None;
    }
    let id = allocator.holds.new_id();
    let mut frame = first;
    while frame < last {
        if allocator.state(frame) != State::Free {
            frame += 1;
            continue;
        }
        let run_start = frame;
        while frame < last && allocator.state(frame) == State::Free {
            frame += 1;
        }
        let (run_first, run_last) = (run_start as u64 * PAGE_SIZE, frame as u64 * PAGE_SIZE);
        if allocator.reserve(run_first, run_last).is_ok()
            && !allocator.holds.add(id, run_start, frame)
        {
            crate::serial_println!(
                "Memory: hold table full; {:#x}..{:#x} stays reserved",
                run_first,
                run_last
            );
        }
    }
    Some(id)
}

/// Return the frames of a [`hold_range`] hold to the allocator.
pub fn release_hold(id: HoldId) {
    let mut allocator = ALLOCATOR.lock();
    let mut holds = allocator.holds;
    holds.release(id, |first, last| allocator.unreserve(first, last));
    allocator.holds = holds;
}

/// Frames still pinned by boot-time holds.
pub fn held_frames() -> usize {
    ALLOCATOR.lock().holds.held_frames()
}

/// Pin the usable memory-map region holding `stack_pointer` until
/// [`release_boot_stack`]. The UEFI firmware stack is boot-services memory,
/// which the map reports as usable while the kernel is still running on it.
pub fn hold_boot_stack(stack_pointer: u64) {
    let region = ALLOCATOR
        .lock()
        .map
        .usable()
        .find(|region| region.start <= stack_pointer && stack_pointer < region.end())
        .copied();
    if let Some(region) = region {
        *BOOT_STACK_HOLD.lock() = hold_range(region.start, region.end());
    }
}

static BOOT_STACK_HOLD: IrqSpinMutex<Option<HoldId>> = IrqSpinMutex::new(None);

/// Return the region pinned by [`hold_boot_stack`] to the allocator.
pub fn release_boot_stack() {
    if let Some(id) = BOOT_STACK_HOLD.lock().take() {
        release_hold(id);
    }
}

/// Switch metadata access from the bootstrap alias to the direct map once
/// higher-half paging is active, and build the buddy free lists.
pub fn activate_metadata() {
    let mut allocator = ALLOCATOR.lock();
    allocator.metadata.mapped_start =
        crate::mm::paging::KERNEL_VIRT_BASE.saturating_add(allocator.metadata.physical_start);
    if allocator.buddy_active {
        return;
    }
    let mut store = allocator.store();
    allocator.lists.build(&mut store);
    allocator.buddy_active = true;
    init_refcounts(&mut allocator);
}

pub fn alloc_frame() -> Option<PhysFrame> {
    ALLOCATOR
        .lock()
        .allocate(AllocationOrder::ZERO, 0, u64::MAX)
        .ok()
}
/// Allocate a frame and clear it, so no previous owner's data leaks.
pub fn alloc_frame_zeroed() -> Option<PhysFrame> {
    let frame = alloc_frame()?;
    unsafe { core::ptr::write_bytes(frame_pointer(frame), 0, PAGE_SIZE as usize) };
    Some(frame)
}
pub fn alloc_frame_at_or_above(minimum: PhysAddr) -> Option<PhysFrame> {
    ALLOCATOR
        .lock()
        .allocate(AllocationOrder::ZERO, minimum.0, u64::MAX)
        .ok()
}
pub(crate) fn alloc_frame_below(maximum: PhysAddr) -> Option<PhysFrame> {
    ALLOCATOR
        .lock()
        .allocate(AllocationOrder::ZERO, 0, maximum.0)
        .ok()
}

/// Drop one reference to `frame`; the last reference returns it to the
/// allocator. A rejected release means another owner still believes it holds
/// this frame, so continuing would corrupt memory far from the cause.
pub fn free_frame(frame: PhysFrame) {
    if let Some(count) = refcount_slot(frame) {
        match count.fetch_sub(1, Ordering::AcqRel) {
            0 => panic!(
                "free_frame({:#x}): frame holds no reference (double free or never allocated)",
                frame.0
            ),
            1 => {}
            _ => return,
        }
    }
    poison(frame, 1);
    if let Err(error) = ALLOCATOR.lock().release(frame, AllocationOrder::ZERO) {
        panic!("free_frame({:#x}): {:?}", frame.0, error);
    }
}

/// Take an additional reference to an allocated frame (for shared or
/// copy-on-write mappings). The caller must already hold a reference.
pub fn frame_get(frame: PhysFrame) -> Result<(), FrameError> {
    let count = refcount_slot(frame).ok_or(FrameError::NotShareable)?;
    count
        .try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            (value != 0 && value != u16::MAX).then(|| value + 1)
        })
        .map(|_| ())
        .map_err(|_| FrameError::NotShareable)
}

/// Current reference count of `frame`, once reference counting is active.
pub fn frame_refcount(frame: PhysFrame) -> Option<u16> {
    refcount_slot(frame).map(|count| count.load(Ordering::Acquire))
}

pub fn alloc_contiguous(
    order: AllocationOrder,
    below: Option<PhysAddr>,
) -> Result<PhysFrame, FrameError> {
    alloc_contiguous_in_range(
        order,
        AllocationConstraints {
            minimum: None,
            maximum: below,
        },
    )
}
pub fn alloc_contiguous_in_range(
    order: AllocationOrder,
    constraints: AllocationConstraints,
) -> Result<PhysFrame, FrameError> {
    let minimum = constraints.minimum.map_or(0, |address| address.0);
    let maximum = constraints.maximum.map_or(u64::MAX, |address| address.0);
    if minimum >= maximum {
        return Err(FrameError::InvalidRange);
    }
    ALLOCATOR.lock().allocate(order, minimum, maximum)
}
/// Release a block from [`alloc_contiguous`]. Contiguous blocks are never
/// shared, so every frame must still hold exactly one reference.
pub fn release_contiguous(frame: PhysFrame, order: AllocationOrder) {
    let first = (frame.0 / PAGE_SIZE) as usize;
    for index in first..first + (1usize << order.0) {
        if let Some(count) = refcount_slot(PhysFrame(index as u64 * PAGE_SIZE)) {
            let previous = count.swap(0, Ordering::AcqRel);
            assert_eq!(
                previous, 1,
                "release_contiguous({:#x}, order {}): frame {:#x} has {} references",
                frame.0, order.0, index, previous
            );
        }
    }
    poison(frame, 1 << order.0);
    if let Err(error) = ALLOCATOR.lock().release(frame, order) {
        panic!(
            "release_contiguous({:#x}, order {}): {:?}",
            frame.0, order.0, error
        );
    }
}

/// Reserve `start..end` for good. Frames inside a temporary hold leave it,
/// so releasing the hold will not free them.
pub fn reserve_range(start: PhysAddr, end: PhysAddr) {
    let mut allocator = ALLOCATOR.lock();
    let result = allocator.reserve(start.0, end.0);
    if result.is_ok() {
        let first = (start.0 / PAGE_SIZE) as usize;
        let last = (end.0 / PAGE_SIZE) as usize;
        if !allocator.holds.punch(first, last) {
            crate::serial_println!("Memory: hold table full; part of a hold stays reserved");
        }
    }
    drop(allocator);
    if let Err(error) = result {
        crate::serial_println!(
            "Memory: reservation failure for {:#x}..{:#x}: {:?}.",
            start.0,
            end.0,
            error
        );
    }
}
pub fn memory_map() -> PhysicalMemoryMap {
    ALLOCATOR.lock().map
}

/// Cross-check the free lists against the bitmap (diagnostics only).
pub fn validate() -> Result<(), &'static str> {
    let allocator = ALLOCATOR.lock();
    if !allocator.buddy_active {
        return Ok(());
    }
    let mut store = allocator.store();
    allocator.lists.validate(&mut store)
}

pub fn stats() -> AllocatorStats {
    let allocator = ALLOCATOR.lock();
    let mut result = AllocatorStats::default();
    for region in allocator.map.regions[..allocator.map.count].iter() {
        let frames = region.length / PAGE_SIZE;
        result.total_frames += frames;
        if region.kind == PhysicalMemoryKind::Usable {
            result.usable_frames += frames;
        }
    }
    result.tracked_frames = allocator.metadata.frame_count as u64;
    result.unsupported_frames = result.total_frames.saturating_sub(result.tracked_frames);
    for frame in 0..allocator.metadata.frame_count {
        match allocator.state(frame) {
            State::Free => result.free_frames += 1,
            State::Allocated => result.allocated_frames += 1,
            State::Reserved => result.reserved_frames += 1,
        }
    }
    result.largest_contiguous_block = if allocator.buddy_active {
        allocator
            .lists
            .largest_order()
            .map_or(0, |order| 1 << order)
    } else {
        0
    };
    result
}

/// Dense per-frame reference counts, reached through the direct map. Null
/// until `activate_metadata` allocates them.
static REFCOUNTS: AtomicPtr<AtomicU16> = AtomicPtr::new(core::ptr::null_mut());
static REFCOUNT_FRAMES: AtomicUsize = AtomicUsize::new(0);

fn refcount_slot(frame: PhysFrame) -> Option<&'static AtomicU16> {
    let base = REFCOUNTS.load(Ordering::Acquire);
    let index = usize::try_from(frame.0 / PAGE_SIZE).ok()?;
    if base.is_null() || index >= REFCOUNT_FRAMES.load(Ordering::Acquire) {
        return None;
    }
    Some(unsafe { &*base.add(index) })
}

fn set_refcounts(first: usize, count: usize, value: u16) {
    for index in first..first + count {
        if let Some(slot) = refcount_slot(PhysFrame(index as u64 * PAGE_SIZE)) {
            slot.store(value, Ordering::Release);
        }
    }
}

/// Allocate the reference-count array from the buddy lists and give every
/// frame allocated so far one reference.
fn init_refcounts(allocator: &mut Allocator) {
    let frames = allocator.metadata.frame_count;
    let bytes = (frames * core::mem::size_of::<AtomicU16>()) as u64;
    let pages = bytes.div_ceil(PAGE_SIZE);
    let order = pages.next_power_of_two().trailing_zeros() as usize;
    let Ok(block) = allocator.allocate(AllocationOrder(order), 0, u64::MAX) else {
        crate::serial_println!(
            "Memory: no {} KiB block for frame reference counts.",
            bytes >> 10
        );
        return;
    };
    // Give back the tail of the power-of-two block; the pages merge back
    // into the free lists.
    for page in pages..1u64 << order {
        allocator
            .release(PhysFrame(block.0 + page * PAGE_SIZE), AllocationOrder::ZERO)
            .expect("returning unused reference-count pages");
    }
    let base = crate::mm::paging::phys_to_virt(PhysAddr(block.0)).0 as *mut AtomicU16;
    for index in 0..frames {
        let value = u16::from(allocator.state(index) == State::Allocated);
        unsafe { base.add(index).write(AtomicU16::new(value)) };
    }
    REFCOUNT_FRAMES.store(frames, Ordering::Release);
    REFCOUNTS.store(base, Ordering::Release);
}

/// Where a frame's bytes are reachable right now.
fn frame_pointer(frame: PhysFrame) -> *mut u8 {
    if crate::mm::paging::is_active() {
        crate::mm::paging::phys_to_virt(PhysAddr(frame.0)).0 as *mut u8
    } else {
        frame.0 as *mut u8
    }
}

/// Fill freed frames with a recognizable pattern so a use-after-free read
/// returns obvious garbage instead of plausible stale data.
#[cfg(feature = "mm-poison")]
fn poison(frame: PhysFrame, pages: usize) {
    if crate::mm::paging::is_active() {
        unsafe { core::ptr::write_bytes(frame_pointer(frame), 0x6b, pages * PAGE_SIZE as usize) };
    }
}

#[cfg(not(feature = "mm-poison"))]
fn poison(_frame: PhysFrame, _pages: usize) {}
