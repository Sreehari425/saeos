use crate::sync::SpinMutex;
use core::alloc::{GlobalAlloc, Layout};
use core::mem::{align_of, size_of};
use core::ptr;

/// The kernel heap: a static byte array in .bss.
///
/// Embedding the heap here means the linker places it after all code/data sections,
/// so it is safe in both BIOS mode (where it falls after the kernel image) and
/// UEFI mode (where OVMF loads the EFI binary into low RAM and 0x400000 conflicts).
pub const HEAP_SIZE: usize = 10 * 1024 * 1024; // 10 MiB

#[repr(C, align(16))]
struct HeapStorage([u8; HEAP_SIZE]);

static mut HEAP_STORAGE: HeapStorage = HeapStorage([0u8; HEAP_SIZE]);

/// Returns the runtime start address of the embedded heap storage.
#[inline]
pub fn heap_start() -> usize {
    // SAFETY: we only read the address, never the contents at this point.
    core::ptr::addr_of!(HEAP_STORAGE) as usize
}

// Keep HEAP_START as a 0 sentinel; real address is from heap_start() above.
// Kept for backward-compat with any callers that just want "the start for display".
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
        self.start_addr() + self.size
    }
}

pub struct LinkedListAllocator {
    head: ListNode,
}

impl LinkedListAllocator {
    pub const fn new() -> Self {
        Self {
            head: ListNode::new(0),
        }
    }

    /// Initializes the linked list allocator with a memory region.
    ///
    /// # Safety
    /// The caller must ensure that the given memory region is valid, writable,
    /// and not used elsewhere.
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        unsafe {
            self.add_free_region(heap_start, heap_size);
        }
    }

    /// Adds a free memory chunk to the allocator free list, merging adjacent regions.
    unsafe fn add_free_region(&mut self, addr: usize, size: usize) {
        // Ensure that the region is large enough to hold a ListNode and is properly aligned
        assert!(align_up(addr, align_of::<ListNode>()) == addr);
        assert!(size >= size_of::<ListNode>());

        // Create new node in the freed memory region
        let mut node = ListNode::new(size);
        node.next = self.head.next.take();
        let node_ptr = addr as *mut ListNode;
        unsafe {
            node_ptr.write(node);
            self.head.next = Some(&mut *node_ptr);
        }
    }

    /// Finds a free region and removes it from the free list.
    fn find_region(&mut self, size: usize, align: usize) -> Option<(&'static mut ListNode, usize)> {
        let mut current = &mut self.head;

        while let Some(ref mut region) = current.next {
            if let Ok(alloc_start) = Self::alloc_from_region(region, size, align) {
                let next = region.next.take();
                let ret = Some((current.next.take().unwrap(), alloc_start));
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
        let alloc_start = align_up(region.start_addr(), align);
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
    fn size_align(layout: Layout) -> (usize, usize) {
        let layout = layout
            .align_to(align_of::<ListNode>())
            .expect("Aligning to ListNode failed")
            .pad_to_align();
        let size = layout.size().max(size_of::<ListNode>());
        (size, layout.align())
    }
}

impl Default for LinkedListAllocator {
    fn default() -> Self {
        Self::new()
    }
}

pub struct LockedHeap(SpinMutex<LinkedListAllocator>);

impl LockedHeap {
    pub const fn empty() -> Self {
        Self(SpinMutex::new(LinkedListAllocator::new()))
    }

    /// Initializes the heap with start address and size.
    ///
    /// # Safety
    /// Must only be called once with valid, mapped physical memory.
    pub unsafe fn init(&self, heap_start: usize, heap_size: usize) {
        unsafe {
            self.0.lock().init(heap_start, heap_size);
        }
    }
}

unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let (size, align) = LinkedListAllocator::size_align(layout);
        let mut allocator = self.0.lock();

        if let Some((region, alloc_start)) = allocator.find_region(size, align) {
            let alloc_end = alloc_start.checked_add(size).expect("Overflow");
            let excess_size = region.end_addr() - alloc_end;

            if excess_size > 0 {
                unsafe {
                    allocator.add_free_region(alloc_end, excess_size);
                }
            }

            alloc_start as *mut u8
        } else {
            ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let (size, _) = LinkedListAllocator::size_align(layout);
        unsafe {
            self.0.lock().add_free_region(ptr as usize, size);
        }
    }
}

#[inline]
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub fn init() {
    unsafe {
        ALLOCATOR.init(heap_start(), HEAP_SIZE);
    }
}
