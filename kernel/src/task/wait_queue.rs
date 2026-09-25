use super::TaskId;
use alloc::collections::VecDeque;

pub struct WaitQueue {
    pub id: u64,
    pub(super) tasks: VecDeque<TaskId>,
}

impl WaitQueue {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            tasks: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub(super) fn remove(&mut self, task: TaskId) -> bool {
        if let Some(index) = self.tasks.iter().position(|queued| *queued == task) {
            self.tasks.remove(index);
            true
        } else {
            false
        }
    }
}
