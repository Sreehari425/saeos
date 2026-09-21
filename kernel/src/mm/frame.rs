//! Physical-frame ownership and buddy allocation.
//!
//! The two-bit bitmap is authoritative. Buddy lists are rebuilt from that
//! bitmap after mutations, so stale metadata can never allocate a reserved
//! frame and releases fully coalesce adjacent free blocks.
use crate::boot::boot_info::{PhysicalMemoryKind, PhysicalMemoryMap};
use crate::sync::IrqSpinMutex;

pub const PAGE_SIZE: u64 = 4096;
pub const MAX_ORDER: usize = 18;
const LOW_TABLE_MIN: u64 = 1024 * 1024;

// UEFI keeps the loaded image mapped before the kernel installs its own
// tables. This image-backed arena is used only for the compact ownership
// bitmap during that transition; the ordinary heap remains independent.
const BOOT_METADATA_ARENA_SIZE: usize = 2 * 1024 * 1024;
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
    OutOfMemory,
    InvalidOrder,
    UnsupportedAddress,
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Reserved = 0,
    Free = 1,
    Allocated = 2,
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
}

#[derive(Clone, Copy)]
struct Allocator {
    map: PhysicalMemoryMap,
    metadata: Metadata,
    low_cursor_address: u64,
}

impl Allocator {
    const fn empty() -> Self {
        Self {
            map: PhysicalMemoryMap::empty(),
            metadata: Metadata::EMPTY,
            low_cursor_address: LOW_TABLE_MIN,
        }
    }
    fn index(&self, address: u64) -> Result<usize, FrameError> {
        if address % PAGE_SIZE != 0 {
            return Err(FrameError::Unaligned);
        }
        let index =
            usize::try_from(address / PAGE_SIZE).map_err(|_| FrameError::UnsupportedAddress)?;
        (index < self.metadata.frame_count)
            .then_some(index)
            .ok_or(FrameError::UnsupportedAddress)
    }
    fn endpoint(&self, address: u64) -> Result<usize, FrameError> {
        if address % PAGE_SIZE != 0 {
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
        let shift = (frame & 3) * 2;
        let bitmap = unsafe { *self.metadata.address(frame / 4) };
        match (bitmap >> shift) & 3 {
            1 => State::Free,
            2 => State::Allocated,
            _ => State::Reserved,
        }
    }
    fn set_state(&self, frame: usize, state: State) {
        let shift = (frame & 3) * 2;
        let byte = self.metadata.address(frame / 4);
        unsafe {
            *byte = (*byte & !(3 << shift)) | ((state as u8) << shift);
        }
    }
    fn range_state(&self, start: usize, count: usize, expected: State) -> bool {
        start
            .checked_add(count)
            .is_some_and(|end| end <= self.metadata.frame_count)
            && (start..start + count).all(|frame| self.state(frame) == expected)
    }
    fn rebuild_buddy(&mut self) {
        // The bitmap is authoritative. Free blocks are discovered lazily by
        // aligned scans so fragmented firmware maps need only reserve the
        // compact ownership bitmap.
    }
    fn reserve(&mut self, start: u64, end: u64) -> Result<(), FrameError> {
        if start >= end || start % PAGE_SIZE != 0 || end % PAGE_SIZE != 0 {
            return Err(FrameError::InvalidRange);
        }
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
        for frame in first..last {
            match self.state(frame) {
                State::Allocated => return Err(FrameError::Overlap),
                State::Free => self.set_state(frame, State::Reserved),
                State::Reserved => {}
            }
        }
        self.rebuild_buddy();
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
        let count = 1usize << order.0;
        for candidate_order in order.0..=MAX_ORDER {
            let candidate_count = 1usize << candidate_order;
            let mut start = 0usize;
            while start
                .checked_add(candidate_count)
                .is_some_and(|end| end <= self.metadata.frame_count)
            {
                let address = start as u64 * PAGE_SIZE;
                let bytes = count as u64 * PAGE_SIZE;
                if address < min || address.checked_add(bytes).is_none_or(|end| end > max) {
                    start += candidate_count;
                    continue;
                }
                if start % count != 0
                    || !self.range_state(start, 1usize << candidate_order, State::Free)
                {
                    start += candidate_count;
                    continue;
                }
                for frame in start..start + count {
                    self.set_state(frame, State::Allocated);
                }
                return Ok(PhysFrame(address));
            }
        }
        Err(FrameError::OutOfMemory)
    }
    fn release(&mut self, frame: PhysFrame, order: AllocationOrder) -> Result<(), FrameError> {
        if !order.is_valid() {
            return Err(FrameError::InvalidOrder);
        }
        let start = self.index(frame.0)?;
        let count = 1usize << order.0;
        if start % count != 0 {
            return Err(FrameError::Unaligned);
        }
        if self.range_state(start, count, State::Reserved) {
            return Err(FrameError::DuplicateRelease);
        }
        if !self.range_state(start, count, State::Allocated) {
            return Err(FrameError::InvalidRange);
        }
        for index in start..start + count {
            self.set_state(index, State::Free);
        }
        self.rebuild_buddy();
        Ok(())
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
    let frame_count =
        usize::try_from(highest.div_ceil(PAGE_SIZE)).map_err(|_| FrameError::UnsupportedAddress)?;
    if frame_count == 0 {
        return Err(FrameError::InvalidRange);
    }
    let bitmap_bytes = frame_count
        .checked_mul(2)
        .ok_or(FrameError::Overflow)?
        .div_ceil(8);
    let metadata_bytes = bitmap_bytes;
    let pages = metadata_bytes.div_ceil(PAGE_SIZE as usize);
    let metadata_bytes = pages
        .checked_mul(PAGE_SIZE as usize)
        .ok_or(FrameError::Overflow)?;
    // UEFI may not identity-map arbitrary usable RAM before paging is
    // initialized. Keep the compact bootstrap bitmap inside the kernel's
    // embedded image/heap area, which both BIOS and UEFI already map.
    let metadata_start = core::ptr::addr_of!(BOOT_METADATA_ARENA) as u64;
    if metadata_start % PAGE_SIZE != 0
        || metadata_bytes > BOOT_METADATA_ARENA_SIZE
        || metadata_start.checked_add(metadata_bytes as u64).is_none()
    {
        crate::serial_println!(
            "Memory: metadata range invalid start={:#x} bytes={} heap_size={} align={}",
            metadata_start,
            metadata_bytes,
            crate::mm::heap::HEAP_SIZE,
            metadata_start % PAGE_SIZE
        );
        return Err(FrameError::OutOfMemory);
    }

    allocator.map = map;
    allocator.metadata = Metadata {
        physical_start: metadata_start,
        mapped_start: metadata_start,
        frame_count,
        pages,
    };
    allocator.metadata.clear();
    for region in map.usable() {
        let start = region.start.max(PAGE_SIZE).next_multiple_of(PAGE_SIZE) / PAGE_SIZE;
        let end = region.end() / PAGE_SIZE;
        for frame in start..end {
            if let Ok(frame) = usize::try_from(frame) {
                if frame < allocator.metadata.frame_count {
                    allocator.set_state(frame, State::Free);
                }
            }
        }
    }
    let heap_first =
        usize::try_from(metadata_start / PAGE_SIZE).map_err(|_| FrameError::UnsupportedAddress)?;
    let heap_pages = crate::mm::heap::HEAP_SIZE / PAGE_SIZE as usize;
    for frame in heap_first..heap_first.saturating_add(heap_pages) {
        if frame < allocator.metadata.frame_count {
            allocator.set_state(frame, State::Reserved);
        }
    }
    let metadata_first =
        usize::try_from(metadata_start / PAGE_SIZE).map_err(|_| FrameError::UnsupportedAddress)?;
    for frame in metadata_first..metadata_first.saturating_add(pages) {
        if frame < allocator.metadata.frame_count {
            allocator.set_state(frame, State::Reserved);
        }
    }
    allocator.low_cursor_address = LOW_TABLE_MIN;
    allocator.rebuild_buddy();
    Ok(())
}

/// Switch metadata access from the bootstrap identity alias to the final
/// direct-map alias once higher-half paging is active.
pub fn activate_metadata() {
    let mut allocator = ALLOCATOR.lock();
    allocator.metadata.mapped_start =
        crate::mm::paging::KERNEL_VIRT_BASE.saturating_add(allocator.metadata.physical_start);
}

pub fn alloc_frame() -> Option<PhysFrame> {
    ALLOCATOR
        .lock()
        .allocate(AllocationOrder::ZERO, 0, u64::MAX)
        .ok()
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
pub fn free_frame(frame: PhysFrame) {
    let _ = ALLOCATOR.lock().release(frame, AllocationOrder::ZERO);
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
pub fn release_contiguous(frame: PhysFrame, order: AllocationOrder) -> Result<(), FrameError> {
    ALLOCATOR.lock().release(frame, order)
}

pub fn reserve_range(start: PhysAddr, end: PhysAddr) {
    if let Err(error) = ALLOCATOR.lock().reserve(start.0, end.0) {
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
    let mut largest = 0u64;
    let mut run = 0u64;
    for frame in 0..allocator.metadata.frame_count {
        if allocator.state(frame) == State::Free {
            run += 1;
            largest = largest.max(run);
        } else {
            run = 0;
        }
    }
    let mut order = 0u64;
    while (1u64 << (order + 1)) <= largest {
        order += 1;
    }
    result.largest_contiguous_block = if largest == 0 { 0 } else { 1u64 << order };
    result
}

pub trait FrameAllocator {
    fn allocate_frame(&mut self, max_address: Option<PhysAddr>) -> Option<PhysFrame>;
    fn deallocate_frame(&mut self, frame: PhysFrame);
}
impl FrameAllocator for Allocator {
    fn allocate_frame(&mut self, max_address: Option<PhysAddr>) -> Option<PhysFrame> {
        self.allocate(
            AllocationOrder::ZERO,
            0,
            max_address.map_or(u64::MAX, |address| address.0),
        )
        .ok()
    }
    fn deallocate_frame(&mut self, frame: PhysFrame) {
        let _ = self.release(frame, AllocationOrder::ZERO);
    }
}
