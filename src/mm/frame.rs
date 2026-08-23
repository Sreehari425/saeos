use crate::boot::boot_info::{PhysicalMemoryMap, PhysicalMemoryRegion};

pub const PAGE_SIZE: u64 = 4096;
const MAX_RESERVED: usize = 128;

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
    cursor: usize,
}
impl Allocator {
    const fn empty() -> Self {
        Self {
            map: PhysicalMemoryMap::empty(),
            reserved: [PhysicalMemoryRegion::EMPTY; MAX_RESERVED],
            reserved_count: 0,
            cursor: 0,
        }
    }
    fn reserve(&mut self, start: u64, end: u64) {
        if start < end && self.reserved_count < MAX_RESERVED {
            self.reserved[self.reserved_count] = PhysicalMemoryRegion {
                start,
                length: end - start,
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
        while self.cursor < self.map.count {
            let region = self.map.regions[self.cursor];
            let mut start = region.start.max(PAGE_SIZE).next_multiple_of(PAGE_SIZE);
            while start.saturating_add(PAGE_SIZE) <= region.end() {
                start = start.max(PAGE_SIZE);
                if !self.is_reserved(start, start + PAGE_SIZE) {
                    self.reserve(start, start + PAGE_SIZE);
                    return Some(PhysFrame(start));
                }
                start += PAGE_SIZE;
            }
            self.cursor += 1;
        }
        None
    }
}

static mut ALLOCATOR: Allocator = Allocator::empty();

pub fn init(map: PhysicalMemoryMap) {
    unsafe {
        ALLOCATOR = Allocator {
            map,
            ..Allocator::empty()
        };
    }
}
pub fn alloc_frame() -> Option<PhysFrame> {
    unsafe { (&raw mut ALLOCATOR).as_mut().unwrap().alloc() }
}
pub fn free_frame(frame: PhysFrame) {
    unsafe {
        let allocator = (&raw mut ALLOCATOR).as_mut().unwrap();
        for i in 0..allocator.reserved_count {
            if allocator.reserved[i].start == frame.0 {
                allocator.reserved[i].length = 0;
                break;
            }
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
