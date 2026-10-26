#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;
#[cfg(test)]
use std::thread;

/// Kernel lock ranks, outermost first. A lock may only be taken while every
/// held lock has a lower rank; this mirrors the order documented in
/// `kernel/src/mm/mod.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// `SCHEDULER` / `PROCESSES`: may be held across heap growth, so memory
    /// management must never take them.
    Scheduler = 0,
    /// `AddressSpace.state`.
    AddressSpace = 1,
    /// The small-object heap.
    Heap = 2,
    /// `KERNEL_MAP_LOCK`, serializing the shared kernel page tables.
    KernelMap = 3,
    PhysicalAllocator = 4,
    /// Page-heap and stack-arena bitmaps: leaves, released before mapping.
    Arena = 5,
}

#[derive(Default)]
pub struct LockOrderModel(Mutex<Vec<Rank>>);

impl LockOrderModel {
    pub fn acquire(&self, rank: Rank) -> bool {
        let mut held = self.0.lock().unwrap();
        if held.last().is_some_and(|last| rank <= *last) {
            return false;
        }
        held.push(rank);
        true
    }
    pub fn release(&self, rank: Rank) {
        assert_eq!(self.0.lock().unwrap().pop(), Some(rank));
    }
}

/// Acquire `path` in order (as nested locks), then release it.
#[cfg(test)]
fn path_allowed(model: &LockOrderModel, path: &[Rank]) -> bool {
    let mut taken = Vec::new();
    let mut allowed = true;
    for rank in path {
        if !model.acquire(*rank) {
            allowed = false;
            break;
        }
        taken.push(*rank);
    }
    for rank in taken.into_iter().rev() {
        model.release(rank);
    }
    allowed
}

#[test]
fn kernel_lock_paths_follow_the_documented_order() {
    use Rank::*;
    let model = LockOrderModel::default();
    // Paths the kernel actually takes.
    for path in [
        // AddressSpace::map -> alloc_table
        &[AddressSpace, PhysicalAllocator][..],
        // AddressSpace::map_owned reserves Vec capacity under its lock.
        &[AddressSpace, Heap],
        // paging::map_page -> child -> alloc_table
        &[KernelMap, PhysicalAllocator],
        // Scheduler Vec growth -> page heap -> map_page -> frames.
        &[Scheduler, KernelMap, PhysicalAllocator],
        // Large allocation under the scheduler lock reserves page-heap pages.
        &[Scheduler, Arena],
    ] {
        assert!(path_allowed(&model, path), "{path:?} must be allowed");
    }
    // Inversions that would deadlock against the paths above.
    for path in [
        &[PhysicalAllocator, KernelMap][..],
        &[KernelMap, AddressSpace],
        &[Heap, AddressSpace],
        &[PhysicalAllocator, Scheduler],
        &[Arena, PhysicalAllocator],
        &[Arena, KernelMap],
        &[KernelMap, KernelMap],
    ] {
        assert!(!path_allowed(&model, path), "{path:?} must be rejected");
    }
}

#[test]
fn lock_order_is_tracked_per_thread_of_use() {
    let model = Arc::new(LockOrderModel::default());
    let worker = Arc::clone(&model);
    let handle = thread::spawn(move || {
        assert!(worker.acquire(Rank::AddressSpace));
        assert!(worker.acquire(Rank::PhysicalAllocator));
        worker.release(Rank::PhysicalAllocator);
        worker.release(Rank::AddressSpace);
    });
    handle.join().unwrap();
    assert!(model.acquire(Rank::Heap));
    assert!(!model.acquire(Rank::AddressSpace));
    model.release(Rank::Heap);
}
