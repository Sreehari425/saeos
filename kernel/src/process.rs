//! Process ownership foundations.
//!
//! Scheduling remains attached to TCBs. This module owns only process-wide
//! identity, address-space association, children, exit status, and the list
//! of threads belonging to a process. User-mode creation is intentionally not
//! part of this layer yet.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::fs::FileTable;
use crate::task::{AddressSpaceId, ExitStatus, ProcessId, TaskId};

/// The process init runs in.
pub const INIT_PID: ProcessId = ProcessId(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessError {
    UnknownProcess,
    DuplicateThread,
    AlreadyExited,
    NotExited,
    DuplicateWaiter,
    UnknownWaiter,
}

#[derive(Debug)]
pub struct ProcessControlBlock {
    pub id: ProcessId,
    pub parent: Option<ProcessId>,
    pub address_space: Option<AddressSpaceId>,
    pub children: Vec<ProcessId>,
    pub exit_status: Option<ExitStatus>,
    pub threads: Vec<TaskId>,
    pub child_waiters: Vec<TaskId>,
    pub references: usize,
    /// Open files. Shared through `Arc` so file I/O happens outside the
    /// (interrupt-disabling) process-table lock.
    pub files: Arc<FileTable>,
    /// Current directory, an absolute normalised path (`fs::path::resolve`).
    pub cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessSnapshot {
    pub id: ProcessId,
    pub address_space: Option<AddressSpaceId>,
    pub child_count: usize,
    pub thread_count: usize,
    pub exit_status: Option<ExitStatus>,
    pub parent: Option<ProcessId>,
    pub waiter_count: usize,
}

impl ProcessControlBlock {
    fn new(
        id: ProcessId,
        parent: Option<ProcessId>,
        address_space: Option<AddressSpaceId>,
        thread: TaskId,
    ) -> Self {
        Self {
            id,
            parent,
            address_space,
            children: Vec::new(),
            exit_status: None,
            threads: alloc::vec![thread],
            child_waiters: Vec::new(),
            references: 0,
            files: Arc::new(FileTable::new()),
            cwd: String::from("/"),
        }
    }
}

pub struct ProcessTable {
    processes: Vec<ProcessControlBlock>,
    next_id: u64,
    /// Address spaces of reaped processes, for the caller to free once the
    /// table lock is released (page-table teardown takes the frame lock).
    released_spaces: Vec<AddressSpaceId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_keeps_resources_separate_from_threads() {
        let mut table = ProcessTable::new();
        let process = table.create(Some(AddressSpaceId(7)), TaskId(1));
        table.add_thread(process, TaskId(2)).unwrap();
        table.exit(process, ExitStatus(3)).unwrap();
        let record = table.process(process).unwrap();
        assert_eq!(record.address_space, Some(AddressSpaceId(7)));
        assert_eq!(record.threads, [TaskId(1), TaskId(2)]);
        assert_eq!(record.exit_status, Some(ExitStatus(3)));
    }

    #[test]
    fn child_process_is_unlinked_after_final_thread_reaped() {
        let mut table = ProcessTable::new();
        let parent = table.create(None, TaskId(1));
        let child = table.create_child(parent, None, TaskId(2)).unwrap();
        table.exit(child, ExitStatus(0)).unwrap();
        assert_eq!(table.remove_thread(child, TaskId(2)).unwrap(), true);
        assert!(!table.reap_if_empty(child));
        assert_eq!(table.wait_child(parent, child), Ok(ExitStatus(0)));
        assert!(table.process(child).is_none());
        assert!(table.process(parent).unwrap().children.is_empty());
    }

    #[test]
    fn child_status_waits_for_parent_collection_and_references() {
        let mut table = ProcessTable::new();
        let parent = table.create(None, TaskId(1));
        let child = table.create_child(parent, None, TaskId(2)).unwrap();
        table.retain(child).unwrap();
        table.add_child_waiter(child, TaskId(1)).unwrap();
        table.exit(child, ExitStatus(17)).unwrap();
        table.remove_thread(child, TaskId(2)).unwrap();
        assert!(!table.reap_if_empty(child));
        assert_eq!(table.wait_child(parent, child), Ok(ExitStatus(17)));
        assert!(table.process(child).is_some());
        table.remove_child_waiter(child, TaskId(1)).unwrap();
        assert!(!table.reap_if_empty(child));
        table.release(child).unwrap();
        assert!(table.reap_if_empty(child));
    }

    #[test]
    fn reaping_releases_the_address_space_exactly_once() {
        let mut table = ProcessTable::new();
        let parent = table.create(None, TaskId(1));
        let child = table
            .create_child(parent, Some(AddressSpaceId(42)), TaskId(2))
            .unwrap();
        table.exit(child, ExitStatus(7)).unwrap();
        table.remove_thread(child, TaskId(2)).unwrap();
        assert!(
            table.take_released_spaces().is_empty(),
            "parent has not collected it"
        );
        assert_eq!(table.wait_child(parent, child), Ok(ExitStatus(7)));
        assert_eq!(table.take_released_spaces(), [AddressSpaceId(42)]);
        assert!(table.take_released_spaces().is_empty());
        assert!(!table.reap_if_empty(child), "already gone");
        assert!(table.take_released_spaces().is_empty());
        // A process without its own space releases nothing.
        let kernel = table.create(None, TaskId(3));
        table.exit(kernel, ExitStatus(0)).unwrap();
        table.remove_thread(kernel, TaskId(3)).unwrap();
        assert!(table.reap_if_empty(kernel));
        assert!(table.take_released_spaces().is_empty());
    }

    #[test]
    fn init_is_pid_one_whatever_the_creation_order() {
        let mut table = ProcessTable::new();
        let idle = table.create(None, TaskId(1));
        let init = table.create_init(TaskId(2)).unwrap();
        let other = table.create(None, TaskId(3));
        assert_eq!(init, INIT_PID);
        assert!(idle.0 > 1 && other.0 > 1 && idle != other);
        assert!(table.create_init(TaskId(4)).is_err(), "only one init");
    }

    #[test]
    fn exited_children_are_found_oldest_first() {
        let mut table = ProcessTable::new();
        let parent = table.create(None, TaskId(1));
        let first = table.create_child(parent, None, TaskId(2)).unwrap();
        let second = table.create_child(parent, None, TaskId(3)).unwrap();
        assert!(table.has_children(parent));
        assert_eq!(table.exited_child(parent), None);
        // The second exits first: it is the one wait4(-1) gets.
        table.exit(second, ExitStatus(2)).unwrap();
        assert_eq!(
            table.exited_child(parent),
            None,
            "its thread is not retired yet"
        );
        table.remove_thread(second, TaskId(3)).unwrap();
        assert_eq!(table.exited_child(parent), Some(second));
        table.exit(first, ExitStatus(1)).unwrap();
        table.remove_thread(first, TaskId(2)).unwrap();
        assert_eq!(
            table.exited_child(parent),
            Some(first),
            "creation order among exited"
        );
        assert_eq!(table.wait_child(parent, first), Ok(ExitStatus(1)));
        assert_eq!(table.exited_child(parent), Some(second));
        assert_eq!(table.wait_child(parent, second), Ok(ExitStatus(2)));
        assert!(!table.has_children(parent));
        assert_eq!(table.exited_child(parent), None);
    }

    #[test]
    fn orphans_go_to_init_and_can_be_collected_there() {
        let mut table = ProcessTable::new();
        let init = table.create_init(TaskId(1)).unwrap();
        let parent = table.create_child(init, None, TaskId(2)).unwrap();
        let running = table.create_child(parent, None, TaskId(3)).unwrap();
        let zombie = table.create_child(parent, None, TaskId(4)).unwrap();
        table.exit(zombie, ExitStatus(9)).unwrap();
        table.remove_thread(zombie, TaskId(4)).unwrap();
        table.reparent_children(parent, init);
        assert!(!table.has_children(parent));
        assert_eq!(table.process(running).unwrap().parent, Some(init));
        assert_eq!(table.exited_child(init), Some(zombie));
        assert_eq!(table.wait_child(init, zombie), Ok(ExitStatus(9)));
        assert_eq!(table.process(init).unwrap().children, [parent, running]);
        table.reparent_children(init, init);
        assert_eq!(
            table.process(init).unwrap().children.len(),
            2,
            "init keeps its own"
        );
    }

    #[test]
    fn child_waiter_registration_rejects_duplicates_and_unknown_removal() {
        let mut table = ProcessTable::new();
        let parent = table.create(None, TaskId(1));
        let child = table.create_child(parent, None, TaskId(2)).unwrap();
        assert_eq!(table.add_child_waiter(child, TaskId(1)), Ok(()));
        assert_eq!(
            table.add_child_waiter(child, TaskId(1)),
            Err(ProcessError::DuplicateWaiter)
        );
        assert_eq!(
            table.remove_child_waiter(child, TaskId(99)),
            Err(ProcessError::UnknownWaiter)
        );
        assert_eq!(table.remove_child_waiter(child, TaskId(1)), Ok(()));
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            processes: Vec::new(),
            // PID 1 is reserved for init (`create_init`).
            next_id: 2,
            released_spaces: Vec::new(),
        }
    }

    /// Create the initial one-process/one-thread kernel process.
    pub fn create(&mut self, address_space: Option<AddressSpaceId>, thread: TaskId) -> ProcessId {
        let id = ProcessId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.processes
            .push(ProcessControlBlock::new(id, None, address_space, thread));
        id
    }

    /// Create PID 1, the process init runs in. Every other process gets a
    /// higher id, whatever the creation order.
    pub fn create_init(&mut self, thread: TaskId) -> Result<ProcessId, ProcessError> {
        if self.process(INIT_PID).is_some() {
            return Err(ProcessError::DuplicateThread);
        }
        self.processes
            .push(ProcessControlBlock::new(INIT_PID, None, None, thread));
        Ok(INIT_PID)
    }

    pub fn create_child(
        &mut self,
        parent: ProcessId,
        address_space: Option<AddressSpaceId>,
        thread: TaskId,
    ) -> Result<ProcessId, ProcessError> {
        if self.process(parent).is_none() {
            return Err(ProcessError::UnknownProcess);
        }
        let child = self.create(address_space, thread);
        self.process_mut(child).expect("new process exists").parent = Some(parent);
        self.process_mut(parent)
            .ok_or(ProcessError::UnknownProcess)?
            .children
            .push(child);
        Ok(child)
    }

    pub fn process(&self, id: ProcessId) -> Option<&ProcessControlBlock> {
        self.processes.iter().find(|process| process.id == id)
    }

    pub fn process_mut(&mut self, id: ProcessId) -> Option<&mut ProcessControlBlock> {
        self.processes.iter_mut().find(|process| process.id == id)
    }

    pub fn add_child(&mut self, parent: ProcessId, child: ProcessId) -> Result<(), ProcessError> {
        let process = self
            .process_mut(parent)
            .ok_or(ProcessError::UnknownProcess)?;
        process.children.push(child);
        Ok(())
    }

    pub fn set_address_space(
        &mut self,
        process: ProcessId,
        address_space: Option<AddressSpaceId>,
    ) -> Result<(), ProcessError> {
        self.process_mut(process)
            .ok_or(ProcessError::UnknownProcess)?
            .address_space = address_space;
        Ok(())
    }

    pub fn add_thread(&mut self, process: ProcessId, thread: TaskId) -> Result<(), ProcessError> {
        let process = self
            .process_mut(process)
            .ok_or(ProcessError::UnknownProcess)?;
        if process.threads.contains(&thread) {
            return Err(ProcessError::DuplicateThread);
        }
        process.threads.push(thread);
        Ok(())
    }

    pub fn exit(&mut self, process: ProcessId, status: ExitStatus) -> Result<(), ProcessError> {
        let process = self
            .process_mut(process)
            .ok_or(ProcessError::UnknownProcess)?;
        if process.exit_status.is_some() {
            return Err(ProcessError::AlreadyExited);
        }
        process.exit_status = Some(status);
        Ok(())
    }

    pub fn remove_thread(
        &mut self,
        process: ProcessId,
        thread: TaskId,
    ) -> Result<bool, ProcessError> {
        let process = self
            .process_mut(process)
            .ok_or(ProcessError::UnknownProcess)?;
        let Some(index) = process.threads.iter().position(|id| *id == thread) else {
            return Err(ProcessError::UnknownProcess);
        };
        process.threads.swap_remove(index);
        Ok(process.threads.is_empty())
    }

    pub fn reap_if_empty(&mut self, process: ProcessId) -> bool {
        let can_reap = self.process(process).is_some_and(|record| {
            record.parent.is_none()
                && record.threads.is_empty()
                && record.exit_status.is_some()
                && record.references == 0
                && record.child_waiters.is_empty()
        });
        if !can_reap {
            return false;
        }
        if let Some(index) = self
            .processes
            .iter()
            .position(|record| record.id == process)
        {
            let record = self.processes.swap_remove(index);
            if let Some(space) = record.address_space {
                self.released_spaces.push(space);
            }
            for parent in &mut self.processes {
                parent.children.retain(|child| *child != process);
            }
            return true;
        }
        false
    }

    /// Consume a completed child status and release the parent's ownership.
    pub fn wait_child(
        &mut self,
        parent: ProcessId,
        child: ProcessId,
    ) -> Result<ExitStatus, ProcessError> {
        let record = self.process(child).ok_or(ProcessError::UnknownProcess)?;
        if record.parent != Some(parent) || !record.threads.is_empty() {
            return Err(ProcessError::UnknownProcess);
        }
        let status = record.exit_status.ok_or(ProcessError::NotExited)?;
        self.process_mut(parent)
            .ok_or(ProcessError::UnknownProcess)?
            .children
            .retain(|id| *id != child);
        self.process_mut(child)
            .ok_or(ProcessError::UnknownProcess)?
            .parent = None;
        // External references belong to their holders. Collecting the child
        // status releases only the parent's ownership; outstanding references
        // continue to keep the record alive.
        self.reap_if_empty(child);
        Ok(status)
    }

    /// A child of `parent` that has exited and whose last thread is gone
    /// (ready for `wait4`), the oldest first.
    pub fn exited_child(&self, parent: ProcessId) -> Option<ProcessId> {
        let record = self.process(parent)?;
        record.children.iter().copied().find(|&child| {
            self.process(child)
                .is_some_and(|child| child.exit_status.is_some() && child.threads.is_empty())
        })
    }

    pub fn has_children(&self, parent: ProcessId) -> bool {
        self.process(parent)
            .is_some_and(|record| !record.children.is_empty())
    }

    /// Hand `process`'s children to `new_parent` (init), as Linux does when
    /// a parent exits, so someone is left to collect them.
    pub fn reparent_children(&mut self, process: ProcessId, new_parent: ProcessId) {
        if process == new_parent {
            return;
        }
        let children = match self.process_mut(process) {
            Some(record) => core::mem::take(&mut record.children),
            None => return,
        };
        for &child in &children {
            if let Some(record) = self.process_mut(child) {
                record.parent = Some(new_parent);
            }
        }
        if let Some(parent) = self.process_mut(new_parent) {
            parent.children.extend(children);
        }
    }

    /// Address spaces whose processes were reaped since the last call. Each
    /// is returned exactly once; the caller frees it outside the lock.
    pub fn take_released_spaces(&mut self) -> Vec<AddressSpaceId> {
        core::mem::take(&mut self.released_spaces)
    }

    pub fn retain(&mut self, process: ProcessId) -> Result<(), ProcessError> {
        let record = self
            .process_mut(process)
            .ok_or(ProcessError::UnknownProcess)?;
        record.references = record.references.saturating_add(1);
        Ok(())
    }

    pub fn release(&mut self, process: ProcessId) -> Result<(), ProcessError> {
        let record = self
            .process_mut(process)
            .ok_or(ProcessError::UnknownProcess)?;
        if record.references == 0 {
            return Err(ProcessError::UnknownProcess);
        }
        record.references -= 1;
        Ok(())
    }

    pub fn add_child_waiter(
        &mut self,
        child: ProcessId,
        waiter: TaskId,
    ) -> Result<(), ProcessError> {
        let record = self
            .process_mut(child)
            .ok_or(ProcessError::UnknownProcess)?;
        if record.child_waiters.contains(&waiter) {
            return Err(ProcessError::DuplicateWaiter);
        }
        record.child_waiters.push(waiter);
        Ok(())
    }

    pub fn remove_child_waiter(
        &mut self,
        child: ProcessId,
        waiter: TaskId,
    ) -> Result<(), ProcessError> {
        let record = self
            .process_mut(child)
            .ok_or(ProcessError::UnknownProcess)?;
        let Some(index) = record.child_waiters.iter().position(|id| *id == waiter) else {
            return Err(ProcessError::UnknownWaiter);
        };
        record.child_waiters.swap_remove(index);
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = &ProcessControlBlock> {
        self.processes.iter()
    }

    pub fn snapshots(&self) -> Vec<ProcessSnapshot> {
        self.processes
            .iter()
            .map(|process| ProcessSnapshot {
                id: process.id,
                address_space: process.address_space,
                child_count: process.children.len(),
                thread_count: process.threads.len(),
                exit_status: process.exit_status,
                parent: process.parent,
                waiter_count: process.child_waiters.len(),
            })
            .collect()
    }
}
