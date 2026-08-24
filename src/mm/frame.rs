use crate::boot::boot_info::{PhysicalMemoryKind, PhysicalMemoryMap, PhysicalMemoryRegion};

pub const PAGE_SIZE: u64 = 4096;
// A 512 GiB four-level direct map needs one page-directory frame per GiB
// (for each identity/direct-map root), in addition to ordinary reservations.
const MAX_RESERVED: usize = 4096;
const MAX_RECYCLED: usize = 256;

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
    reserved_count: usize,
    recycled: [PhysFrame; MAX_RECYCLED],
    recycled_count: usize,
    cursor: usize,
    cursor_address: u64,
}
impl Allocator {
    const fn empty() -> Self {
        Self {
            map: PhysicalMemoryMap::empty(),
            reserved: [PhysicalMemoryRegion::EMPTY; MAX_RESERVED],
            reserved_count: 0,
            recycled: [PhysFrame(0); MAX_RECYCLED],
            recycled_count: 0,
            cursor: 0,
            cursor_address: 0,
        }
    }
    fn reserve(&mut self, start: u64, end: u64) {
        if start < end && self.reserved_count < MAX_RESERVED {
            self.reserved[self.reserved_count] = PhysicalMemoryRegion {
                start,
                length: end - start,
                kind: PhysicalMemoryKind::Reserved,
            };
            self.reserved_count += 1;
        }
    }
    fn is_reserved(&self, start: u64, end: u64) -> bool {
        self.reserved[..self.reserved_count]
            .iter()
            .any(|r| start < r.end() && end > r.start)
    }
    fn alloc(&mut self) -> Option<PhysFrame> {
        if self.recycled_count != 0 {
            self.recycled_count -= 1;
            return Some(self.recycled[self.recycled_count]);
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
                    self.reserve(start, start + PAGE_SIZE);
                    return Some(PhysFrame(start));
                }
                start += PAGE_SIZE;
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
        ALLOCATOR.recycled_count = 0;
        ALLOCATOR.cursor = 0;
        ALLOCATOR.cursor_address = 0;
    }
}
pub fn alloc_frame() -> Option<PhysFrame> {
    unsafe { (&raw mut ALLOCATOR).as_mut().unwrap().alloc() }
}
pub fn alloc_frame_at_or_above(minimum: PhysAddr) -> Option<PhysFrame> {
    unsafe {
        let allocator = (&raw mut ALLOCATOR).as_mut().unwrap();
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
        if self.recycled_count < MAX_RECYCLED {
            self.recycled[self.recycled_count] = frame;
            self.recycled_count += 1;
        }
    }
}
pub fn free_frame(frame: PhysFrame) {
    unsafe {
        let allocator = (&raw mut ALLOCATOR).as_mut().unwrap();
        if allocator.recycled_count < MAX_RECYCLED {
            allocator.recycled[allocator.recycled_count] = frame;
            allocator.recycled_count += 1;
        }
    }
}
pub fn reserve_range(start: PhysAddr, end: PhysAddr) {
    unsafe {
        (&raw mut ALLOCATOR)
            .as_mut()
            .unwrap()
            .reserve(start.0, end.0);
    }
}
pub fn memory_map() -> PhysicalMemoryMap {
    unsafe { ALLOCATOR.map }
}
