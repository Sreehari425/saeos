//! Non-owning address-space registry used by task dispatch.
//!
//! The registry owns IDs and root-frame bindings only. `paging::AddressSpace`
//! remains owned by the future process layer; the scheduler stores an
//! `AddressSpaceId` and never frees an address space through this table.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::mm::frame::PhysFrame;
use crate::sync::IrqSpinMutex;
use crate::task::AddressSpaceId;

pub static ADDRESS_SPACES: IrqSpinMutex<AddressSpaceRegistry> =
    IrqSpinMutex::new(AddressSpaceRegistry::new());
static ACTIVE_ADDRESS_SPACE: AtomicU64 = AtomicU64::new(u64::MAX);
static ACTIVATION_COUNT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSpaceError {
    Unknown,
    AlreadyBound,
}

pub struct AddressSpaceRegistry {
    entries: Vec<(AddressSpaceId, PhysFrame)>,
    next_id: u64,
}

impl Default for AddressSpaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AddressSpaceRegistry {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
        }
    }

    pub fn register(&mut self, root: PhysFrame) -> AddressSpaceId {
        let id = AddressSpaceId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.entries.push((id, root));
        id
    }

    pub fn bind(&mut self, id: AddressSpaceId, root: PhysFrame) -> Result<(), AddressSpaceError> {
        if self.entries.iter().any(|(existing, _)| *existing == id) {
            return Err(AddressSpaceError::AlreadyBound);
        }
        self.entries.push((id, root));
        Ok(())
    }

    pub fn unregister(&mut self, id: AddressSpaceId) -> Result<PhysFrame, AddressSpaceError> {
        let index = self
            .entries
            .iter()
            .position(|(existing, _)| *existing == id)
            .ok_or(AddressSpaceError::Unknown)?;
        Ok(self.entries.swap_remove(index).1)
    }

    pub fn root(&self, id: AddressSpaceId) -> Result<PhysFrame, AddressSpaceError> {
        self.entries
            .iter()
            .find(|(existing, _)| *existing == id)
            .map(|(_, root)| *root)
            .ok_or(AddressSpaceError::Unknown)
    }

    /// Activate a registered root. The caller must invoke this after
    /// releasing scheduler and registry locks.
    pub fn activate(&self, id: AddressSpaceId) -> Result<(), AddressSpaceError> {
        let root = self.root(id)?;
        crate::mm::paging::activate_root(root);
        Ok(())
    }
}

pub fn register(root: PhysFrame) -> AddressSpaceId {
    ADDRESS_SPACES.lock().register(root)
}

pub fn unregister(id: AddressSpaceId) -> Result<PhysFrame, AddressSpaceError> {
    ADDRESS_SPACES.lock().unregister(id)
}

/// Activate only when the selected identity differs from the current CPU
/// identity. `None` denotes the shared kernel address space.
pub fn activate_if_changed(id: Option<AddressSpaceId>) -> Result<bool, AddressSpaceError> {
    let encoded = id.map_or(u64::MAX, |value| value.0);
    if ACTIVE_ADDRESS_SPACE.load(Ordering::Acquire) == encoded {
        return Ok(false);
    }
    if let Some(id) = id {
        let root = ADDRESS_SPACES.lock().root(id)?;
        crate::mm::paging::activate_root(root);
    } else {
        crate::mm::paging::activate();
    }
    ACTIVE_ADDRESS_SPACE.store(encoded, Ordering::Release);
    ACTIVATION_COUNT.fetch_add(1, Ordering::Relaxed);
    Ok(true)
}

pub fn active() -> Option<AddressSpaceId> {
    match ACTIVE_ADDRESS_SPACE.load(Ordering::Acquire) {
        u64::MAX => None,
        value => Some(AddressSpaceId(value)),
    }
}

pub fn activation_count() -> u64 {
    ACTIVATION_COUNT.load(Ordering::Acquire)
}
