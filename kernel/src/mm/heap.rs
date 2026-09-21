use crate::sync::IrqSpinMutex;
use core::alloc::{GlobalAlloc, Layout};
use core::mem::{align_of, size_of};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::mm::{PhysAddr, VirtAddr};

/// The kernel heap: a static byte array in .bss.
///
/// Embedding the heap here means the linker places it after all code/data sections,
/// so it is safe in both BIOS mode (where it falls after the kernel image) and
/// UEFI mode (where OVMF loads the EFI binary into low RAM and 0x400000 conflicts).
pub const HEAP_SIZE: usize = 10 * 1024 * 1024; // 10 MiB
const SMALL_CLASSES: [usize; 9] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const SMALL_REFILL_BLOCKS: usize = 32;

#[repr(C, align(4096))]
struct HeapStorage([u8; HEAP_SIZE]);

static mut HEAP_STORAGE: HeapStorage = HeapStorage([0u8; HEAP_SIZE]);
static HEAP_INITIALIZED: AtomicBool = AtomicBool::new(false);
// Keep the range in the existing higher-half PML4 slot while placing it
// above the physical-memory window covered by the boot direct map.
const PAGE_HEAP_BASE: usize = (crate::mm::paging::KERNEL_VIRT_BASE + 0x20_0000_0000) as usize;
const PAGE_HEAP_PAGES: usize = 262_144; // 1 GiB virtual extent.
static PAGE_HEAP_READY: AtomicBool = AtomicBool::new(false);
static PAGE_HEAP_LOCK: IrqSpinMutex<()> = IrqSpinMutex::new(());
static mut PAGE_HEAP_BITMAP: [u64; PAGE_HEAP_PAGES / 64] = [0; PAGE_HEAP_PAGES / 64];
static SMALL_USAGE: [core::sync::atomic::AtomicUsize; SMALL_CLASSES.len()] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];
static PAGE_COUNT: AtomicUsize = AtomicUsize::new(0);
static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static FAILED_ALLOCS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeapStats {
    pub small_class_usage: [usize; SMALL_CLASSES.len()],
    pub page_count: usize,
    pub large_allocations: usize,
    pub failed_allocations: usize,
    pub largest_free_extent: usize,
}

/// Physical / load address of the embedded heap storage (for reservations).
#[inline]
pub fn heap_phys_start() -> u64 {
    // SAFETY: we only read the address, never the contents at this point.
    core::ptr::addr_of!(HEAP_STORAGE) as u64
}

/// Virtual address used by the freelist after higher-half activate.
///
/// Before paging is active this equals the physical load address; afterwards
/// it is the direct-map alias so freelist traffic does not depend on the
/// mutable BIOS bootstrap identity map.
#[inline]
pub fn heap_virt_start() -> usize {
    let phys = heap_phys_start();
    if crate::mm::paging::is_active() {
        (crate::mm::paging::KERNEL_VIRT_BASE + phys) as usize
    } else {
        phys as usize
    }
}

/// Display / shell helper: prefer the virtual base the allocator uses.
#[inline]
pub fn heap_start() -> usize {
    heap_virt_start()
}

pub fn heap_start_display() -> usize {
    heap_start()
}

struct ListNode {
    size: usize,
    next: Option<&'static mut ListNode>,
}

impl ListNode {
    const fn new(size: usize) -> Self {
        Self { size, next: None }
    }

    fn start_addr(&self) -> usize {
        self as *const Self as usize
    }

    fn end_addr(&self) -> usize {
        self.start_addr().saturating_add(self.size)
    }
}

pub struct LinkedListAllocator {
    head: ListNode,
    // Addresses are used instead of raw pointers so the metadata remains
    // Send and can be protected by SpinMutex.
    small_heads: [usize; SMALL_CLASSES.len()],
}

impl LinkedListAllocator {
    pub const fn new() -> Self {
        Self {
            head: ListNode::new(0),
            small_heads: [0; SMALL_CLASSES.len()],
        }
    }

    /// Initializes the linked list allocator with a memory region.
    ///
    /// # Safety
    /// The caller must ensure that the given memory region is valid, writable,
    /// and not used elsewhere.
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) -> bool {
        // Do not depend on firmware/bootloader BSS semantics. BIOS startup
        // clears the image BSS, but the allocator must also be self-contained
        // when initialized and must never retain a stale sentinel link.
        self.head = ListNode::new(0);
        self.small_heads = [0; SMALL_CLASSES.len()];
        unsafe { self.add_free_region(heap_start, heap_size) }
    }

    /// Adds a free memory chunk to the allocator free list, merging adjacent regions.
    unsafe fn add_free_region(&mut self, addr: usize, size: usize) -> bool {
        unsafe {
            if align_up(addr, align_of::<ListNode>()) != Some(addr) || size < size_of::<ListNode>()
            {
                return false;
            }
            let Some(end) = addr.checked_add(size) else {
                return false;
            };
            let mut previous: *mut ListNode = &mut self.head;
            while let Some(node) = (*previous).next.as_deref_mut() {
                if end <= node.start_addr() {
                    break;
                }
                if addr < node.end_addr() && end > node.start_addr() {
                    return false;
                }
                previous = node as *mut ListNode;
            }
            let next = (*previous).next.take();
            let node_ptr = addr as *mut ListNode;
            node_ptr.write(ListNode { size, next });
            (*previous).next = Some(&mut *node_ptr);

            let inserted = &mut **(*previous).next.as_mut().unwrap() as *mut ListNode;
            let merged = if !core::ptr::eq(previous, &mut self.head as *mut ListNode)
                && (*previous).end_addr() == addr
            {
                (*previous).size += size;
                (*previous).next = (*inserted).next.take();
                previous
            } else {
                inserted
            };
            if let Some(successor) = (*merged).next.as_mut()
                && (*merged).end_addr() == successor.start_addr()
            {
                (*merged).size += successor.size;
                (*merged).next = successor.next.take();
            }
            true
        }
    }

    /// Finds a free region and removes it from the free list.
    fn find_region(&mut self, size: usize, align: usize) -> Option<(usize, usize, usize)> {
        let mut current = &mut self.head;

        while let Some(ref mut region) = current.next {
            if let Ok(alloc_start) = Self::alloc_from_region(region, size, align) {
                let start = region.start_addr();
                let end = region.end_addr();
                let next = region.next.take();
                let ret = Some((start, end, alloc_start));
                current.next = next;
                return ret;
            } else {
                current = current.next.as_mut().unwrap();
            }
        }

        None
    }

    /// Attempts to allocate from a single free region.
    fn alloc_from_region(region: &ListNode, size: usize, align: usize) -> Result<usize, ()> {
        let alloc_start = align_up(region.start_addr(), align).ok_or(())?;
        let prefix_size = alloc_start.checked_sub(region.start_addr()).ok_or(())?;
        if prefix_size != 0 && prefix_size < size_of::<ListNode>() {
            return Err(());
        }
        let alloc_end = alloc_start.checked_add(size).ok_or(())?;

        if alloc_end > region.end_addr() {
            return Err(());
        }

        let excess_size = region.end_addr() - alloc_end;
        if excess_size > 0 && excess_size < size_of::<ListNode>() {
            // Cannot hold a ListNode in the remaining chunk
            return Err(());
        }

        Ok(alloc_start)
    }

    /// Calculates size adjustment for ListNode alignment.
    fn size_align(layout: Layout) -> Option<(usize, usize)> {
        let layout = layout.align_to(align_of::<ListNode>()).ok()?.pad_to_align();
        if layout.size() > SMALL_CLASSES[SMALL_CLASSES.len() - 1] {
            let size = layout
                .size()
                .checked_add(4095)?
                .checked_div(4096)?
                .checked_mul(4096)?;
            Some((size, layout.align().max(4096)))
        } else {
            Some((layout.size().max(size_of::<ListNode>()), layout.align()))
        }
    }

    fn small_class(layout: Layout) -> Option<usize> {
        let required = layout.size().max(layout.align()).max(16);
        SMALL_CLASSES.iter().position(|size| *size >= required)
    }

    unsafe fn refill_small(&mut self, class: usize) -> Option<usize> {
        let block = SMALL_CLASSES[class];
        let total = block.checked_mul(SMALL_REFILL_BLOCKS)?;
        let (region_start, region_end, alloc_start) = self.find_region(total, block)?;
        let prefix = alloc_start - region_start;
        let end = alloc_start.checked_add(total)?;
        let suffix = region_end - end;
        if prefix != 0 {
            unsafe {
                self.add_free_region(region_start, prefix);
            }
        }
        if suffix != 0 {
            unsafe {
                self.add_free_region(end, suffix);
            }
        }
        for offset in (block..total).step_by(block) {
            let address = alloc_start + offset;
            unsafe {
                *(address as *mut usize) = self.small_heads[class];
            }
            self.small_heads[class] = address;
        }
        Some(alloc_start)
    }

    fn has_small_free(&self) -> bool {
        self.small_heads.iter().any(|head| *head != 0)
    }
}

impl Default for LinkedListAllocator {
    fn default() -> Self {
        Self::new()
    }
}

pub struct LockedHeap(IrqSpinMutex<LinkedListAllocator>);

impl LockedHeap {
    pub const fn empty() -> Self {
        Self(IrqSpinMutex::new(LinkedListAllocator::new()))
    }

    /// Initializes the heap with start address and size.
    ///
    /// # Safety
    /// Must only be called once with valid, mapped physical memory.
    pub unsafe fn init(&self, heap_start: usize, heap_size: usize) -> bool {
        unsafe { self.0.lock().init(heap_start, heap_size) }
    }
}

unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() > SMALL_CLASSES[SMALL_CLASSES.len() - 1]
            && PAGE_HEAP_READY.load(Ordering::Acquire)
        {
            return page_alloc(layout).map_or(ptr::null_mut(), |address| address as *mut u8);
        }
        let Some((size, align)) = LinkedListAllocator::size_align(layout) else {
            FAILED_ALLOCS.fetch_add(1, Ordering::Relaxed);
            return ptr::null_mut();
        };
        let mut allocator = self.0.lock();

        if let Some(class) = LinkedListAllocator::small_class(layout) {
            let address = if allocator.small_heads[class] != 0 {
                let address = allocator.small_heads[class];
                allocator.small_heads[class] = unsafe { *(address as *mut usize) };
                address
            } else {
                unsafe { allocator.refill_small(class) }.unwrap_or(0)
            };
            if address != 0 {
                SMALL_USAGE[class].fetch_add(1, Ordering::Relaxed);
                return address as *mut u8;
            }
        }

        if let Some((region_start, region_end, alloc_start)) = allocator.find_region(size, align) {
            let Some(alloc_end) = alloc_start.checked_add(size) else {
                return ptr::null_mut();
            };
            let prefix_size = alloc_start - region_start;
            let excess_size = region_end - alloc_end;
            if prefix_size > 0 {
                let _ = unsafe { allocator.add_free_region(region_start, prefix_size) };
            }

            if excess_size > 0 {
                unsafe {
                    allocator.add_free_region(alloc_end, excess_size);
                }
            }

            LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
            PAGE_COUNT.fetch_add(size.div_ceil(4096), Ordering::Relaxed);
            alloc_start as *mut u8
        } else {
            FAILED_ALLOCS.fetch_add(1, Ordering::Relaxed);
            ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if (ptr as usize) >= PAGE_HEAP_BASE
            && (ptr as usize) < PAGE_HEAP_BASE + PAGE_HEAP_PAGES * 4096
        {
            page_dealloc(ptr as usize, layout);
            return;
        }
        let Some((size, _)) = LinkedListAllocator::size_align(layout) else {
            return;
        };
        let mut allocator = self.0.lock();
        if let Some(class) = LinkedListAllocator::small_class(layout) {
            SMALL_USAGE[class].fetch_sub(1, Ordering::Relaxed);
            let address = ptr as usize;
            unsafe {
                *(address as *mut usize) = allocator.small_heads[class];
            }
            allocator.small_heads[class] = address;
        } else {
            LARGE_ALLOCS.fetch_sub(1, Ordering::Relaxed);
            PAGE_COUNT.fetch_sub(size.div_ceil(4096), Ordering::Relaxed);
            unsafe {
                let _ = allocator.add_free_region(ptr as usize, size);
            }
        }
    }
}

pub fn stats() -> HeapStats {
    let mut result = HeapStats {
        page_count: PAGE_COUNT.load(Ordering::Relaxed),
        large_allocations: LARGE_ALLOCS.load(Ordering::Relaxed),
        failed_allocations: FAILED_ALLOCS.load(Ordering::Relaxed),
        ..HeapStats::default()
    };
    for (slot, usage) in result.small_class_usage.iter_mut().zip(SMALL_USAGE.iter()) {
        *slot = usage.load(Ordering::Relaxed);
    }
    if let Some(allocator) = ALLOCATOR.0.try_lock() {
        result.largest_free_extent = allocator
            .head
            .next
            .as_deref()
            .map(|node| {
                let mut largest = node.size;
                let mut current = node.next.as_deref();
                while let Some(next) = current {
                    largest = largest.max(next.size);
                    current = next.next.as_deref();
                }
                largest
            })
            .unwrap_or(0);
    }
    result
}

fn page_size_align(layout: Layout) -> Option<(usize, usize)> {
    let size = layout
        .size()
        .checked_add(4095)?
        .checked_div(4096)?
        .checked_mul(4096)?;
    Some((size, layout.align().max(4096)))
}

fn page_alloc(layout: Layout) -> Option<usize> {
    let (bytes, align) = page_size_align(layout)?;
    let pages = bytes / 4096;
    let align_pages = align.div_ceil(4096).next_power_of_two();
    let start = {
        let _guard = PAGE_HEAP_LOCK.lock();
        let mut candidate = 0usize;
        let mut found = None;
        while candidate.checked_add(pages)? <= PAGE_HEAP_PAGES {
            candidate = (candidate + align_pages - 1) & !(align_pages - 1);
            if candidate.checked_add(pages)? > PAGE_HEAP_PAGES {
                break;
            }
            let free = (candidate..candidate + pages)
                .all(|page| unsafe { PAGE_HEAP_BITMAP[page / 64] & (1u64 << (page % 64)) == 0 });
            if free {
                for page in candidate..candidate + pages {
                    unsafe {
                        PAGE_HEAP_BITMAP[page / 64] |= 1u64 << (page % 64);
                    }
                }
                found = Some(candidate);
                break;
            }
            candidate += align_pages;
        }
        found?
    };
    let mut mapped = 0usize;
    while mapped < pages {
        let Some(frame) = crate::mm::frame::alloc_frame() else {
            break;
        };
        let virtual_address = VirtAddr((PAGE_HEAP_BASE + (start + mapped) * 4096) as u64);
        if crate::mm::paging::map_page(
            virtual_address,
            PhysAddr(frame.0),
            crate::mm::paging::PageFlags::WRITABLE,
        )
        .is_err()
        {
            crate::mm::frame::free_frame(frame);
            break;
        }
        mapped += 1;
    }
    if mapped != pages {
        for page in 0..mapped {
            let address = VirtAddr((PAGE_HEAP_BASE + (start + page) * 4096) as u64);
            if let Ok(physical) = crate::mm::paging::unmap_page(address) {
                crate::mm::frame::free_frame(crate::mm::PhysFrame(physical.0));
            }
        }
        let _guard = PAGE_HEAP_LOCK.lock();
        for page in start..start + pages {
            unsafe {
                PAGE_HEAP_BITMAP[page / 64] &= !(1u64 << (page % 64));
            }
        }
        return None;
    }
    LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
    PAGE_COUNT.fetch_add(pages, Ordering::Relaxed);
    Some(PAGE_HEAP_BASE + start * 4096)
}

fn page_dealloc(pointer: usize, layout: Layout) {
    let Some((bytes, _)) = page_size_align(layout) else {
        return;
    };
    let start = (pointer - PAGE_HEAP_BASE) / 4096;
    let pages = bytes / 4096;
    let valid = {
        let _guard = PAGE_HEAP_LOCK.lock();
        if start
            .checked_add(pages)
            .is_none_or(|end| end > PAGE_HEAP_PAGES)
        {
            return;
        }
        let valid = (start..start + pages)
            .all(|page| unsafe { PAGE_HEAP_BITMAP[page / 64] & (1u64 << (page % 64)) != 0 });
        if valid {
            for page in start..start + pages {
                unsafe {
                    PAGE_HEAP_BITMAP[page / 64] &= !(1u64 << (page % 64));
                }
            }
        }
        valid
    };
    if !valid {
        return;
    }
    for page in start..start + pages {
        let address = VirtAddr((PAGE_HEAP_BASE + page * 4096) as u64);
        if let Ok(physical) = crate::mm::paging::unmap_page(address) {
            crate::mm::frame::free_frame(crate::mm::PhysFrame(physical.0));
        }
    }
    LARGE_ALLOCS.fetch_sub(1, Ordering::Relaxed);
    PAGE_COUNT.fetch_sub(pages, Ordering::Relaxed);
}

#[inline]
fn align_up(addr: usize, align: usize) -> Option<usize> {
    if !align.is_power_of_two() {
        return None;
    }
    addr.checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub(crate) fn validate() -> bool {
    let Some(allocator) = ALLOCATOR.0.try_lock() else {
        return false;
    };
    let start = heap_virt_start();
    let Some(heap_end) = start.checked_add(HEAP_SIZE) else {
        return false;
    };
    let mut previous_end = start;
    let mut count = 0;
    let mut node = allocator.head.next.as_deref();
    while let Some(current) = node {
        count += 1;
        if count > HEAP_SIZE / size_of::<ListNode>() + 1 {
            return false;
        }
        let node_start = current.start_addr();
        let Some(node_end) = node_start.checked_add(current.size) else {
            return false;
        };
        if node_start % align_of::<ListNode>() != 0
            || current.size < size_of::<ListNode>()
            || node_start < start
            || node_end > heap_end
            || node_start < previous_end
        {
            return false;
        }
        previous_end = node_end;
        node = current.next.as_deref();
    }
    if HEAP_INITIALIZED.load(Ordering::Acquire) && count == 0 && !allocator.has_small_free() {
        return false;
    }
    for (class, head) in allocator.small_heads.iter().enumerate() {
        let block = SMALL_CLASSES[class];
        let mut current = *head;
        let mut seen = 0;
        while current != 0 {
            if seen > HEAP_SIZE / block + 1
                || current < start
                || current
                    .checked_add(block)
                    .is_none_or(|node_end| node_end > heap_end)
                || current % block != 0
            {
                return false;
            }
            current = unsafe { *(current as *const usize) };
            seen += 1;
        }
    }
    true
}

pub(crate) fn validate_reason() -> &'static str {
    let Some(allocator) = ALLOCATOR.0.try_lock() else {
        return "allocator lock held";
    };
    let start = heap_virt_start();
    let Some(end) = start.checked_add(HEAP_SIZE) else {
        return "heap range overflow";
    };
    let mut previous_end = start;
    let mut count = 0;
    let mut node = allocator.head.next.as_deref();
    while let Some(current) = node {
        count += 1;
        if count > HEAP_SIZE / size_of::<ListNode>() + 1 {
            return "free list too long or cyclic";
        }
        let node_start = current.start_addr();
        let Some(node_end) = node_start.checked_add(current.size) else {
            return "free node range overflow";
        };
        if node_start % align_of::<ListNode>() != 0 {
            return "free node misaligned";
        }
        if current.size < size_of::<ListNode>() {
            return "free node too small";
        }
        if node_start < start || node_end > end {
            return "free node outside heap";
        }
        if node_start < previous_end {
            return "free nodes overlap or are unsorted";
        }
        previous_end = node_end;
        node = current.next.as_deref();
    }
    if HEAP_INITIALIZED.load(Ordering::Acquire) && count == 0 && !allocator.has_small_free() {
        return "free list empty after initialization";
    }
    "valid"
}

pub fn init() {
    let start = heap_virt_start();
    let initialized = unsafe { ALLOCATOR.init(start, HEAP_SIZE) };
    if initialized {
        HEAP_INITIALIZED.store(true, Ordering::Release);
        // Keep the bootstrap allocator active until the active-root mapping
        // path is installed by the scheduler-facing paging layer.
        PAGE_HEAP_READY.store(false, Ordering::Release);
    }
    if !initialized || !validate() {
        crate::serial_println!(
            "Memory: fatal heap initialization failure: {}.",
            validate_reason()
        );
    }
}
