//! Process ownership foundations.
//!
//! Scheduling remains attached to TCBs. This module owns only process-wide
//! identity, address-space association, children, exit status, and the list
//! of threads belonging to a process. User-mode creation is intentionally not
//! part of this layer yet.

use alloc::vec::Vec;

use crate::task::{AddressSpaceId, ExitStatus, ProcessId, TaskId};

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
        }
    }
}

pub struct ProcessTable {
    processes: Vec<ProcessControlBlock>,
    next_id: u64,
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
            next_id: 1,
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
            self.processes.swap_remove(index);
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
