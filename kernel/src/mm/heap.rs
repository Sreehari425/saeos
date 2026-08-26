use crate::sync::SpinMutex;
use core::alloc::{GlobalAlloc, Layout};
use core::mem::{align_of, size_of};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

/// The kernel heap: a static byte array in .bss.
///
/// Embedding the heap here means the linker places it after all code/data sections,
/// so it is safe in both BIOS mode (where it falls after the kernel image) and
/// UEFI mode (where OVMF loads the EFI binary into low RAM and 0x400000 conflicts).
pub const HEAP_SIZE: usize = 10 * 1024 * 1024; // 10 MiB

#[repr(C, align(4096))]
struct HeapStorage([u8; HEAP_SIZE]);

static mut HEAP_STORAGE: HeapStorage = HeapStorage([0u8; HEAP_SIZE]);
static HEAP_INITIALIZED: AtomicBool = AtomicBool::new(false);

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
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) -> bool {
        // Do not depend on firmware/bootloader BSS semantics. BIOS startup
        // clears the image BSS, but the allocator must also be self-contained
        // when initialized and must never retain a stale sentinel link.
        self.head = ListNode::new(0);
        unsafe { self.add_free_region(heap_start, heap_size) }
    }

    /// Adds a free memory chunk to the allocator free list, merging adjacent regions.
    unsafe fn add_free_region(&mut self, addr: usize, size: usize) -> bool {
        unsafe {
            if align_up(addr, align_of::<ListNode>()) != addr || size < size_of::<ListNode>() {
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
        let alloc_start = align_up(region.start_addr(), align);
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
    pub unsafe fn init(&self, heap_start: usize, heap_size: usize) -> bool {
        unsafe { self.0.lock().init(heap_start, heap_size) }
    }
}

unsafe impl GlobalAlloc for LockedHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let (size, align) = LinkedListAllocator::size_align(layout);
        let mut allocator = self.0.lock();

        if let Some((region_start, region_end, alloc_start)) = allocator.find_region(size, align) {
            let alloc_end = alloc_start.checked_add(size).expect("Overflow");
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

            alloc_start as *mut u8
        } else {
            ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let (size, _) = LinkedListAllocator::size_align(layout);
        unsafe {
            let _ = self.0.lock().add_free_region(ptr as usize, size);
        }
    }
}

#[inline]
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

#[global_allocator]
pub static ALLOCATOR: LockedHeap = LockedHeap::empty();

pub(crate) fn validate() -> bool {
    let Some(allocator) = ALLOCATOR.0.try_lock() else {
        return false;
    };
    let start = heap_virt_start();
    let Some(end) = start.checked_add(HEAP_SIZE) else {
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
            || node_end > end
            || node_start < previous_end
        {
            return false;
        }
        previous_end = node_end;
        node = current.next.as_deref();
    }
    if HEAP_INITIALIZED.load(Ordering::Acquire) && count == 0 {
        return false;
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
    if HEAP_INITIALIZED.load(Ordering::Acquire) && count == 0 {
        return "free list empty after initialization";
    }
    "valid"
}

pub fn init() {
    let start = heap_virt_start();
    let initialized = unsafe { ALLOCATOR.init(start, HEAP_SIZE) };
    if initialized {
        HEAP_INITIALIZED.store(true, Ordering::Release);
    }
    if !initialized || !validate() {
        crate::serial_println!(
            "Memory: fatal heap initialization failure: {}.",
            validate_reason()
        );
    }
}
