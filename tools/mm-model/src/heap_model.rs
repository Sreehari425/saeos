use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

pub const PAGE_SIZE: usize = 4096;
const CLASSES: [usize; 9] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapError {
    InvalidLayout,
    OutOfMemory,
    DoubleFree,
    InvalidPointer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    start: usize,
    pages: usize,
}

pub struct HeapModel {
    small_free: [Vec<usize>; 9],
    free_runs: Vec<Run>,
    allocations: Vec<Run>,
    next: usize,
    failed: usize,
}

impl HeapModel {
    pub fn new(page_count: usize) -> Self {
        Self {
            small_free: core::array::from_fn(|_| Vec::new()),
            free_runs: vec![Run {
                start: PAGE_SIZE,
                pages: page_count,
            }],
            allocations: Vec::new(),
            next: (page_count + 1) * PAGE_SIZE,
            failed: 0,
        }
    }

    pub fn alloc(&mut self, layout: Layout) -> Result<usize, HeapError> {
        let required = layout.size().max(layout.align()).max(16);
        if let Some(class) = CLASSES.iter().position(|size| *size >= required) {
            if let Some(pointer) = self.small_free[class].pop() {
                self.allocations.push(Run {
                    start: pointer,
                    pages: 0,
                });
                return Ok(pointer);
            }
            let pointer = (self.next + CLASSES[class] - 1) & !(CLASSES[class] - 1);
            self.next = pointer
                .checked_add(CLASSES[class])
                .ok_or(HeapError::OutOfMemory)?;
            self.allocations.push(Run {
                start: pointer,
                pages: 0,
            });
            return Ok(pointer);
        }
        let pages = required
            .checked_add(PAGE_SIZE - 1)
            .ok_or(HeapError::InvalidLayout)?
            / PAGE_SIZE;
        let align = layout.align().max(PAGE_SIZE);
        let index = self
            .free_runs
            .iter()
            .position(|run| {
                let aligned = (run.start + align - 1) & !(align - 1);
                aligned
                    .checked_add(pages * PAGE_SIZE)
                    .is_some_and(|end| end <= run.start + run.pages * PAGE_SIZE)
            })
            .ok_or_else(|| {
                self.failed += 1;
                HeapError::OutOfMemory
            })?;
        let run = self.free_runs.remove(index);
        let start = (run.start + align - 1) & !(align - 1);
        let end = start + pages * PAGE_SIZE;
        if start > run.start {
            self.free_runs.push(Run {
                start: run.start,
                pages: (start - run.start) / PAGE_SIZE,
            });
        }
        if end < run.start + run.pages * PAGE_SIZE {
            self.free_runs.push(Run {
                start: end,
                pages: (run.start + run.pages * PAGE_SIZE - end) / PAGE_SIZE,
            });
        }
        self.allocations.push(Run { start, pages });
        Ok(start)
    }

    pub fn dealloc(&mut self, pointer: usize, layout: Layout) -> Result<(), HeapError> {
        let index = self
            .allocations
            .iter()
            .position(|run| run.start == pointer)
            .ok_or(HeapError::DoubleFree)?;
        let run = self.allocations.remove(index);
        if run.pages == 0 {
            let required = layout.size().max(layout.align()).max(16);
            let class = CLASSES
                .iter()
                .position(|size| *size >= required)
                .ok_or(HeapError::InvalidPointer)?;
            self.small_free[class].push(pointer);
        } else {
            self.free_runs.push(run);
            self.coalesce();
        }
        Ok(())
    }

    pub fn largest_free_extent(&self) -> usize {
        self.free_runs
            .iter()
            .map(|run| run.pages * PAGE_SIZE)
            .max()
            .unwrap_or(0)
    }

    pub fn failed_allocations(&self) -> usize {
        self.failed
    }

    fn coalesce(&mut self) {
        self.free_runs.sort_by_key(|run| run.start);
        let mut merged: Vec<Run> = Vec::with_capacity(self.free_runs.len());
        for run in self.free_runs.drain(..) {
            if let Some(previous) = merged.last_mut() {
                if previous.start + previous.pages * PAGE_SIZE == run.start {
                    previous.pages += run.pages;
                    continue;
                }
            }
            merged.push(run);
        }
        self.free_runs = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_small_classes_reuse_and_alignment() {
        let mut heap = HeapModel::new(64);
        for size in CLASSES {
            let layout = Layout::from_size_align(size, size).unwrap();
            let pointer = heap.alloc(layout).unwrap();
            assert_eq!(pointer % size, 0);
            heap.dealloc(pointer, layout).unwrap();
            assert_eq!(heap.alloc(layout).unwrap(), pointer);
        }
    }

    #[test]
    fn large_runs_reuse_and_coalesce() {
        let mut heap = HeapModel::new(16);
        let layout = Layout::from_size_align(PAGE_SIZE * 2, PAGE_SIZE).unwrap();
        let first = heap.alloc(layout).unwrap();
        let second = heap.alloc(layout).unwrap();
        heap.dealloc(first, layout).unwrap();
        assert_eq!(heap.alloc(layout).unwrap(), first);
        heap.dealloc(second, layout).unwrap();
        heap.dealloc(first, layout).unwrap();
        assert!(heap.largest_free_extent() >= PAGE_SIZE * 12);
    }
}
