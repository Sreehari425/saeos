use crate::boot::boot_info::{PhysicalMemoryKind, PhysicalMemoryMap, PhysicalMemoryRegion};

pub const PAGE_SIZE: u64 = 4096;
const LOW_TABLE_MIN: u64 = 1024 * 1024;
// Sparse maps still need reservations for dynamically allocated hierarchy
// pages, plus the boot image, ACPI, framebuffer, and heap ranges.
const MAX_RESERVED: usize = 16384;
const MAX_RECYCLED: usize = 256;
const MAX_PERMANENT_RANGES: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReservationKind {
    Permanent,
    Allocated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct PhysAddr(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct VirtAddr(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct PhysFrame(pub u64);

#[derive(Clone, Copy)]
struct Allocator {
    map: PhysicalMemoryMap,
    reserved: [PhysicalMemoryRegion; MAX_RESERVED],
    reservation_kind: [ReservationKind; MAX_RESERVED],
    reserved_count: usize,
    permanent: [PhysicalMemoryRegion; MAX_PERMANENT_RANGES],
    permanent_count: usize,
    recycled: [PhysFrame; MAX_RECYCLED],
    recycled_count: usize,
    cursor: usize,
    cursor_address: u64,
    low_cursor_address: u64,
}
impl Allocator {
    const fn empty() -> Self {
        Self {
            map: PhysicalMemoryMap::empty(),
            reserved: [PhysicalMemoryRegion::EMPTY; MAX_RESERVED],
            reservation_kind: [ReservationKind::Permanent; MAX_RESERVED],
            reserved_count: 0,
            permanent: [PhysicalMemoryRegion::EMPTY; MAX_PERMANENT_RANGES],
            permanent_count: 0,
            recycled: [PhysFrame(0); MAX_RECYCLED],
            recycled_count: 0,
            cursor: 0,
            cursor_address: 0,
            low_cursor_address: LOW_TABLE_MIN,
        }
    }
    fn reserve(&mut self, start: u64, end: u64, kind: ReservationKind) -> bool {
        if kind == ReservationKind::Permanent
            && self.permanent_count < MAX_PERMANENT_RANGES
            && start < end
        {
            self.permanent[self.permanent_count] = PhysicalMemoryRegion {
                start,
                length: end - start,
                kind: PhysicalMemoryKind::Reserved,
            };
            self.permanent_count += 1;
        }
        if start < end && self.reserved_count < MAX_RESERVED {
            self.reserved[self.reserved_count] = PhysicalMemoryRegion {
                start,
                length: end - start,
                kind: PhysicalMemoryKind::Reserved,
            };
            self.reservation_kind[self.reserved_count] = kind;
            self.reserved_count += 1;
            true
        } else {
            false
        }
    }
    fn is_reserved(&self, start: u64, end: u64) -> bool {
        self.permanent[..self.permanent_count]
            .iter()
            .any(|r| start < r.end() && end > r.start)
            || self.reserved[..self.reserved_count]
                .iter()
                .any(|r| start < r.end() && end > r.start)
    }
    fn is_permanent(&self, start: u64, end: u64) -> bool {
        self.permanent[..self.permanent_count]
            .iter()
            .any(|r| start < r.end() && end > r.start)
    }
    fn release_frame_reservation(&mut self, frame: PhysFrame) {
        let start = frame.0;
        let end = start + PAGE_SIZE;
        for index in (0..self.reserved_count).rev() {
            let reservation = self.reserved[index];
            if self.reservation_kind[index] == ReservationKind::Allocated
                && reservation.start == start
                && reservation.end() == end
            {
                self.reserved_count -= 1;
                self.reserved[index] = self.reserved[self.reserved_count];
                self.reservation_kind[index] = self.reservation_kind[self.reserved_count];
                break;
            }
        }
    }
    fn alloc(&mut self) -> Option<PhysFrame> {
        while self.recycled_count != 0 {
            self.recycled_count -= 1;
            let frame = self.recycled[self.recycled_count];
            if !self.is_reserved(frame.0, frame.0 + PAGE_SIZE) {
                if self.reserve(frame.0, frame.0 + PAGE_SIZE, ReservationKind::Allocated) {
                    return Some(frame);
                }
                return None;
            }
        }
        while self.cursor < self.map.count {
            let region = self.map.regions[self.cursor];
            if region.kind != PhysicalMemoryKind::Usable {
                self.cursor += 1;
                self.cursor_address = 0;
                continue;
            }
            let minimum = if self.cursor_address == 0 {
                region.start
            } else {
                self.cursor_address
            };
            let mut start = minimum.max(PAGE_SIZE).next_multiple_of(PAGE_SIZE);
            while start.saturating_add(PAGE_SIZE) <= region.end() {
                start = start.max(PAGE_SIZE);
                if !self.is_reserved(start, start + PAGE_SIZE) {
                    self.cursor_address = start + PAGE_SIZE;
                    self.reserve(start, start + PAGE_SIZE, ReservationKind::Allocated);
                    return Some(PhysFrame(start));
                }
                start += PAGE_SIZE;
            }
            self.cursor += 1;
            self.cursor_address = 0;
        }
        None
    }

    fn alloc_with_max(&mut self, max_address: Option<PhysAddr>) -> Option<PhysFrame> {
        if max_address.is_none() {
            return self.alloc();
        }
        let maximum = max_address.unwrap().0;
        for region in self.map.regions[..self.map.count].iter() {
            if region.kind != PhysicalMemoryKind::Usable {
                continue;
            }
            let mut start = region.start.max(PAGE_SIZE).next_multiple_of(PAGE_SIZE);
            while start.saturating_add(PAGE_SIZE) <= region.end() && start < maximum {
                if !self.is_reserved(start, start + PAGE_SIZE) {
                    self.reserve(start, start + PAGE_SIZE, ReservationKind::Allocated);
                    return Some(PhysFrame(start));
                }
                start += PAGE_SIZE;
            }
        }
        None
    }

    fn take_recycled_in_range(&mut self, minimum: u64, maximum: u64) -> Option<PhysFrame> {
        for index in (0..self.recycled_count).rev() {
            let frame = self.recycled[index];
            if frame.0 >= minimum
                && frame.0 < maximum
                && !self.is_reserved(frame.0, frame.0 + PAGE_SIZE)
            {
                self.recycled_count -= 1;
                self.recycled[index] = self.recycled[self.recycled_count];
                if self.reserve(frame.0, frame.0 + PAGE_SIZE, ReservationKind::Allocated) {
                    return Some(frame);
                }
                return None;
            }
        }
        None
    }
}

static mut ALLOCATOR: Allocator = Allocator::empty();

pub fn init(map: PhysicalMemoryMap) {
    unsafe {
        // Keep initialization as a sequence of small stores. This is also
        // safe while the BIOS bootstrap identity map is still active.
        ALLOCATOR.map = map;
        ALLOCATOR.reserved_count = 0;
        ALLOCATOR.permanent_count = 0;
        ALLOCATOR.recycled_count = 0;
        ALLOCATOR.cursor = 0;
        ALLOCATOR.cursor_address = 0;
        ALLOCATOR.low_cursor_address = LOW_TABLE_MIN;
    }
}
pub fn alloc_frame() -> Option<PhysFrame> {
    unsafe { (&raw mut ALLOCATOR).as_mut().unwrap().alloc() }
}
pub fn alloc_frame_at_or_above(minimum: PhysAddr) -> Option<PhysFrame> {
    unsafe {
        let allocator = (&raw mut ALLOCATOR).as_mut().unwrap();
        if let Some(frame) = allocator.take_recycled_in_range(minimum.0, u64::MAX) {
            return Some(frame);
        }
        for region in allocator.map.regions[..allocator.map.count].iter() {
            if region.kind != PhysicalMemoryKind::Usable {
                continue;
            }
            let mut start = region
                .start
                .max(minimum.0)
                .max(PAGE_SIZE)
                .next_multiple_of(PAGE_SIZE);
            while start.saturating_add(PAGE_SIZE) <= region.end() {
                if !allocator.is_reserved(start, start + PAGE_SIZE) {
                    allocator.reserve(start, start + PAGE_SIZE, ReservationKind::Allocated);
                    return Some(PhysFrame(start));
                }
                start += PAGE_SIZE;
            }
        }
        None
    }
}

/// Allocate a frame below `maximum`. Page-table construction uses this while
/// the bootstrap identity map is active, so the table itself remains writable.
pub(crate) fn alloc_frame_below(maximum: PhysAddr) -> Option<PhysFrame> {
    unsafe {
        let allocator = (&raw mut ALLOCATOR).as_mut().unwrap();
        if let Some(frame) = allocator.take_recycled_in_range(0, maximum.0) {
            return Some(frame);
        }
        for region in allocator.map.regions[..allocator.map.count].iter() {
            if region.kind != PhysicalMemoryKind::Usable {
                continue;
            }
            let mut start = region
                .start
                .max(PAGE_SIZE)
                .max(allocator.low_cursor_address)
                .next_multiple_of(PAGE_SIZE);
            let end = region.end().min(maximum.0);
            while start.saturating_add(PAGE_SIZE) <= end {
                if !allocator.is_reserved(start, start + PAGE_SIZE) {
                    allocator.low_cursor_address = start + PAGE_SIZE;
                    allocator.reserve(start, start + PAGE_SIZE, ReservationKind::Allocated);
                    return Some(PhysFrame(start));
                }
                start += PAGE_SIZE;
            }
        }
        None
    }
}

pub trait FrameAllocator {
    fn allocate_frame(&mut self, max_address: Option<PhysAddr>) -> Option<PhysFrame>;
    fn deallocate_frame(&mut self, frame: PhysFrame);
}

impl FrameAllocator for Allocator {
    fn allocate_frame(&mut self, max_address: Option<PhysAddr>) -> Option<PhysFrame> {
        self.alloc_with_max(max_address)
    }
    fn deallocate_frame(&mut self, frame: PhysFrame) {
        self.release_frame_reservation(frame);
        if self.recycled_count < MAX_RECYCLED
            && !self.recycled[..self.recycled_count].contains(&frame)
        {
            self.recycled[self.recycled_count] = frame;
            self.recycled_count += 1;
        }
    }
}
pub fn free_frame(frame: PhysFrame) {
    unsafe {
        let allocator = (&raw mut ALLOCATOR).as_mut().unwrap();
        if allocator.is_permanent(frame.0, frame.0 + PAGE_SIZE) {
            return;
        }
        allocator.release_frame_reservation(frame);
        if allocator.recycled_count < MAX_RECYCLED
            && !allocator.recycled[..allocator.recycled_count].contains(&frame)
        {
            allocator.recycled[allocator.recycled_count] = frame;
            allocator.recycled_count += 1;
        }
    }
}
pub(crate) fn try_reserve_range(start: PhysAddr, end: PhysAddr) -> bool {
    unsafe {
        (&raw mut ALLOCATOR)
            .as_mut()
            .unwrap()
            .reserve(start.0, end.0, ReservationKind::Permanent)
    }
}
pub fn reserve_range(start: PhysAddr, end: PhysAddr) {
    if start.0 < end.0 && !try_reserve_range(start, end) {
        crate::serial_println!(
            "Memory: fatal reservation failure for {:#x}..{:#x}.",
            start.0,
            end.0
        );
    }
}
pub fn memory_map() -> PhysicalMemoryMap {
    unsafe { ALLOCATOR.map }
}
