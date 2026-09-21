#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;
#[cfg(test)]
use std::thread;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    AddressSpace = 1,
    PhysicalAllocator = 2,
    Heap = 3,
}

#[derive(Default)]
pub struct LockOrderModel(Mutex<Vec<Rank>>);

impl LockOrderModel {
    pub fn acquire(&self, rank: Rank) -> bool {
        let mut held = self.0.lock().unwrap();
        if held.last().is_some_and(|last| rank < *last) {
            return false;
        }
        if rank == Rank::Heap && held.iter().any(|item| *item != Rank::Heap) {
            return false;
        }
        held.push(rank);
        true
    }
    pub fn release(&self, rank: Rank) {
        assert_eq!(self.0.lock().unwrap().pop(), Some(rank));
    }
}

#[test]
fn lock_order_is_address_space_then_physical_and_heap_isolated() {
    let model = Arc::new(LockOrderModel::default());
    let worker = Arc::clone(&model);
    let handle = thread::spawn(move || {
        assert!(worker.acquire(Rank::AddressSpace));
        assert!(worker.acquire(Rank::PhysicalAllocator));
        worker.release(Rank::PhysicalAllocator);
        worker.release(Rank::AddressSpace);
    });
    handle.join().unwrap();
    assert!(model.acquire(Rank::AddressSpace));
    assert!(!model.acquire(Rank::Heap));
    model.release(Rank::AddressSpace);
    assert!(model.acquire(Rank::Heap));
    model.release(Rank::Heap);
}
