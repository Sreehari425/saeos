//! Address-space registry used by task dispatch.
//!
//! The scheduler and process table refer to address spaces by
//! `AddressSpaceId`; this registry maps an id to its root frame for CR3
//! switches. Spaces registered with [`register_owned`] are also owned here:
//! the process that runs in one keeps its id, and when the process is
//! reaped [`release`] tears the page tables and owned frames down.

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

/// Owned spaces, by id. Separate from the root table so dispatch (which only
/// needs roots) never contends with teardown.
static OWNED: IrqSpinMutex<Vec<(AddressSpaceId, crate::mm::paging::AddressSpace)>> =
    IrqSpinMutex::new(Vec::new());

/// Register `space` and keep it until [`release`].
pub fn register_owned(space: crate::mm::paging::AddressSpace) -> AddressSpaceId {
    let id = register(space.root_frame());
    OWNED.lock().push((id, space));
    id
}

/// Run `operation` on an owned space (for example, to map more pages).
pub fn with_owned<R>(
    id: AddressSpaceId,
    operation: impl FnOnce(&crate::mm::paging::AddressSpace) -> R,
) -> Result<R, AddressSpaceError> {
    let owned = OWNED.lock();
    let (_, space) = owned
        .iter()
        .find(|(owned_id, _)| *owned_id == id)
        .ok_or(AddressSpaceError::Unknown)?;
    Ok(operation(space))
}

/// Unregister an owned space and free its page tables and owned frames.
/// It must not be the active space (its process has exited, and the caller
/// runs on another root).
pub fn release(id: AddressSpaceId) -> Result<(), AddressSpaceError> {
    assert_ne!(active(), Some(id), "releasing the active address space");
    let space = {
        let mut owned = OWNED.lock();
        let index = owned
            .iter()
            .position(|(owned_id, _)| *owned_id == id)
            .ok_or(AddressSpaceError::Unknown)?;
        owned.swap_remove(index).1
    };
    unregister(id)?;
    // Teardown takes the frame lock; no registry lock is held here.
    drop(space);
    Ok(())
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
