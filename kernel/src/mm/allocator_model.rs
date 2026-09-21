//! Host-testable bitmap + buddy allocator model.
//!
//! The boot allocator uses the same ownership rules, but stores its metadata
//! in bounded boot-safe arrays.  This model deliberately uses `Vec` so stress
//! tests can scale the number of frames without changing kernel constants.

use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelError {
    InvalidOrder,
    OutOfMemory,
    AlreadyReserved,
    NotAllocated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelStats {
    pub total: usize,
    pub allocated: usize,
    pub free: usize,
    pub largest_order: Option<usize>,
}

pub struct BuddyBitmap {
    used: Vec<bool>,
    free: Vec<Vec<usize>>,
    max_order: usize,
}

impl BuddyBitmap {
    pub fn new(frame_count: usize) -> Self {
        let mut max_order = 0;
        while (1usize << (max_order + 1)) <= frame_count {
            max_order += 1;
        }
        let mut result = Self {
            used: vec![true; frame_count],
            free: vec![Vec::new(); max_order + 1],
            max_order,
        };
        let mut cursor = 0;
        while cursor < frame_count {
            let mut order = max_order.min(cursor.trailing_zeros() as usize);
            while (1usize << order) > frame_count - cursor {
                order -= 1;
            }
            result.free[order].push(cursor);
            for index in cursor..cursor + (1usize << order) {
                result.used[index] = false;
            }
            cursor += 1usize << order;
        }
        result
    }

    pub fn reserve(&mut self, start: usize, count: usize) -> Result<(), ModelError> {
        let end = start
            .checked_add(count)
            .ok_or(ModelError::AlreadyReserved)?;
        if count == 0 || end > self.used.len() || (start..end).any(|f| self.used[f]) {
            return Err(ModelError::AlreadyReserved);
        }
        for frame in start..end {
            self.used[frame] = true;
        }
        self.rebuild();
        Ok(())
    }

    pub fn allocate(&mut self, order: usize) -> Result<usize, ModelError> {
        if order > self.max_order {
            return Err(ModelError::InvalidOrder);
        }
        let source = (order..=self.max_order)
            .find(|&candidate| !self.free[candidate].is_empty())
            .ok_or(ModelError::OutOfMemory)?;
        let start = self.free[source].pop().unwrap();
        let mut current = source;
        while current > order {
            current -= 1;
            self.free[current].push(start + (1usize << current));
        }
        for frame in start..start + (1usize << order) {
            self.used[frame] = true;
        }
        Ok(start)
    }

    pub fn release(&mut self, start: usize, order: usize) -> Result<(), ModelError> {
        let count = 1usize
            .checked_shl(order as u32)
            .ok_or(ModelError::InvalidOrder)?;
        if order > self.max_order
            || start.checked_add(count).is_none()
            || start + count > self.used.len()
            || (start..start + count).any(|f| !self.used[f])
        {
            return Err(ModelError::NotAllocated);
        }
        for frame in start..start + count {
            self.used[frame] = false;
        }
        self.rebuild();
        Ok(())
    }

    pub fn stats(&self) -> ModelStats {
        let largest_order = self
            .free
            .iter()
            .enumerate()
            .rev()
            .find(|(_, list)| !list.is_empty())
            .map(|(order, _)| order);
        let allocated = self.used.iter().filter(|used| **used).count();
        ModelStats {
            total: self.used.len(),
            allocated,
            free: self.used.len() - allocated,
            largest_order,
        }
    }

    pub fn is_used(&self, frame: usize) -> bool {
        self.used.get(frame).copied().unwrap_or(true)
    }

    /// Check the same invariants expected from the boot allocator: every free
    /// frame is represented exactly once, and no used frame is present in a
    /// buddy block.
    pub fn validate(&self) -> bool {
        let mut seen = vec![false; self.used.len()];
        for (order, blocks) in self.free.iter().enumerate() {
            let count = 1usize << order;
            for &start in blocks {
                if start % count != 0
                    || start
                        .checked_add(count)
                        .is_none_or(|end| end > self.used.len())
                {
                    return false;
                }
                for frame in start..start + count {
                    if self.used[frame] || seen[frame] {
                        return false;
                    }
                    seen[frame] = true;
                }
            }
        }
        seen.iter()
            .enumerate()
            .all(|(frame, present)| *present == !self.used[frame])
    }

    fn rebuild(&mut self) {
        for list in &mut self.free {
            list.clear();
        }
        let mut cursor = 0;
        while cursor < self.used.len() {
            if self.used[cursor] {
                cursor += 1;
                continue;
            }
            let mut order = self.max_order.min(cursor.trailing_zeros() as usize);
            while order > 0 && (1usize << order) > self.used.len() - cursor {
                order -= 1;
            }
            while order > 0 && (cursor..cursor + (1usize << order)).any(|f| self.used[f]) {
                order -= 1;
            }
            self.free[order].push(cursor);
            cursor += 1usize << order;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_coalesce_and_bitmap_authority() {
        let mut allocator = BuddyBitmap::new(64);
        let block = allocator.allocate(3).unwrap();
        assert_eq!(allocator.stats().allocated, 8);
        assert!(allocator.release(block, 3).is_ok());
        assert_eq!(allocator.stats().free, 64);
        assert!(allocator.reserve(7, 1).is_ok());
        assert!(allocator.allocate(6).is_err());
    }

    #[test]
    fn exhaustion_release_and_duplicate_release() {
        let mut allocator = BuddyBitmap::new(8);
        let first = allocator.allocate(2).unwrap();
        let second = allocator.allocate(2).unwrap();
        assert!(allocator.allocate(0).is_err());
        assert!(allocator.release(first, 2).is_ok());
        assert!(allocator.validate());
        assert_eq!(allocator.stats().free, 4);
        assert!(allocator.release(first, 2).is_err());
        assert!(allocator.release(second, 2).is_ok());
        assert_eq!(allocator.stats().largest_order, Some(3));
    }

    #[test]
    fn reservations_are_authoritative() {
        let mut allocator = BuddyBitmap::new(32);
        assert!(allocator.reserve(8, 4).is_ok());
        assert!(allocator.reserve(8, 1).is_err());
        while let Ok(frame) = allocator.allocate(0) {
            assert!(!(8..12).contains(&frame));
        }
        assert!((8..12).all(|frame| allocator.is_used(frame)));
        assert!(allocator.validate());
    }
}
