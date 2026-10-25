//! Applying `brk`/`mmap`/`munmap`/`mprotect` to a process's page tables.
//! The memory map (`vma.rs`, host-tested) decides which pages change; this
//! maps, unmaps, or re-protects them, eagerly (every page of a region is
//! backed by a zeroed frame from the start).
//!
//! Every operation takes a copy of the memory map first, so a failure part
//! way through (out of frames) puts the map and the pages back to a
//! consistent state instead of leaving half a mapping behind.

use crate::fs::Errno;
use crate::mm::address_space::{self, OwnedSpace};
use crate::mm::paging::{AddressSpace, MappingOwnership, PageFlags};
use crate::mm::vma::{BrkChange, PAGE_SIZE, Protection, Span};
use crate::mm::{VirtAddr, frame};
use crate::task::AddressSpaceId;

fn page_flags(protection: Protection) -> PageFlags {
    if protection.execute {
        PageFlags::USER_READ_EXECUTE
    } else if protection.write {
        PageFlags::USER_READ_WRITE
    } else {
        PageFlags::USER_READ
    }
}

fn pages(span: &Span) -> impl Iterator<Item = u64> {
    (span.start..span.end).step_by(PAGE_SIZE as usize)
}

/// Unmap every mapped page of `span` and drop the frames it owned.
fn unmap_span(space: &AddressSpace, span: &Span) {
    for page in pages(span) {
        if let Ok(mapping) = space.unmap_mapping(VirtAddr(page))
            && mapping.ownership == MappingOwnership::Owned
        {
            frame::free_frame(mapping.frame);
        }
    }
}

/// Back `span` with zeroed frames. On failure, the pages it did map are
/// unmapped again.
fn map_span(space: &mut AddressSpace, span: &Span) -> Result<(), Errno> {
    for page in pages(span) {
        let mapped = frame::alloc_frame_zeroed()
            .ok_or(Errno::ENOMEM)
            .and_then(|new| {
                space
                    .map_owned(VirtAddr(page), new, page_flags(span.protection))
                    .map_err(|_| {
                        frame::free_frame(new);
                        Errno::ENOMEM
                    })
            });
        let protected = mapped.and_then(|()| {
            if span.protection == Protection::NONE {
                space
                    .protect_inaccessible(VirtAddr(page))
                    .map_err(|_| Errno::ENOMEM)
            } else {
                Ok(())
            }
        });
        if let Err(error) = protected {
            unmap_span(
                space,
                &Span {
                    start: span.start,
                    end: page + PAGE_SIZE,
                    protection: span.protection,
                },
            );
            return Err(error);
        }
    }
    Ok(())
}

fn protect_span(space: &mut AddressSpace, span: &Span) -> Result<(), Errno> {
    for page in pages(span) {
        let result = if span.protection == Protection::NONE {
            space.protect_inaccessible(VirtAddr(page))
        } else {
            space.protect(VirtAddr(page), page_flags(span.protection))
        };
        result.map_err(|_| Errno::ENOMEM)?;
    }
    Ok(())
}

fn with_space<R>(
    id: AddressSpaceId,
    operation: impl FnOnce(&mut OwnedSpace) -> Result<R, Errno>,
) -> Result<R, Errno> {
    address_space::with_owned_mut(id, operation).map_err(|_| Errno::ENOMEM)?
}

/// `brk`: returns the new break, or the unchanged one if it cannot move.
pub fn brk(id: AddressSpaceId, address: u64) -> u64 {
    with_space(id, |owned| {
        let current = owned.map.brk();
        if address == 0 {
            return Ok(current);
        }
        let saved = owned.map.clone();
        let Ok(change) = owned.map.set_brk(address) else {
            return Ok(current);
        };
        match change {
            BrkChange::Unchanged => {}
            BrkChange::Unmap(span) => unmap_span(&owned.space, &span),
            BrkChange::Map(span) => {
                if map_span(&mut owned.space, &span).is_err() {
                    owned.map = saved;
                    return Ok(current);
                }
            }
        }
        Ok(owned.map.brk())
    })
    .unwrap_or(0)
}

pub fn mmap(
    id: AddressSpaceId,
    hint: u64,
    length: u64,
    protection: Protection,
    fixed: bool,
) -> Result<u64, Errno> {
    with_space(id, |owned| {
        let saved = owned.map.clone();
        let change = owned.map.mmap(hint, length, protection, fixed)?;
        for span in &change.replaced {
            unmap_span(&owned.space, span);
        }
        if let Err(error) = map_span(&mut owned.space, &change.new) {
            // The replaced pages are gone either way (as on Linux, a failed
            // MAP_FIXED leaves a hole); the map must say so.
            owned.map = saved;
            owned.map.remove(change.new.start, change.new.end);
            return Err(error);
        }
        Ok(change.address)
    })
}

pub fn munmap(id: AddressSpaceId, start: u64, length: u64) -> Result<(), Errno> {
    with_space(id, |owned| {
        for span in owned.map.munmap(start, length)? {
            unmap_span(&owned.space, &span);
        }
        Ok(())
    })
}

pub fn mprotect(
    id: AddressSpaceId,
    start: u64,
    length: u64,
    protection: Protection,
) -> Result<(), Errno> {
    with_space(id, |owned| {
        let saved = owned.map.clone();
        let changed = owned.map.mprotect(start, length, protection)?;
        for span in &changed {
            if let Err(error) = protect_span(&mut owned.space, span) {
                owned.map = saved;
                return Err(error);
            }
        }
        Ok(())
    })
}
