//! A process's memory map: which user address ranges exist and with what
//! protection. The bookkeeping half of `brk`, `mmap`, `munmap` and
//! `mprotect`, kept pure so it is tested in the host model.
//!
//! ```text
//! 0x10000 ── mmap floor (MIN_ADDRESS)
//!   image        (exec: R / RX / RW segments)
//!   heap         brk_start .. brk, grows up
//!   ...          free
//!   mmap regions placed top-down from mmap_top
//!   [guard]
//!   stack        exec's initial stack
//! USER_TOP
//! ```
//!
//! Every region is backed by pages for its whole extent (the kernel maps
//! them eagerly, PROT_NONE included), so each operation returns the exact
//! page ranges the kernel must map, unmap, or re-protect. Regions never
//! overlap and are kept sorted.
//!
//! A device region (`mmap` of `/dev/fb0`) maps device memory instead of
//! fresh frames: page `start + n` is physical `physical + n`. Splitting one
//! (`munmap` or `mprotect` in the middle) shifts each piece's `physical`
//! by how far its start moved ([`Region::piece`]).

use crate::fs::Errno;
use alloc::vec::Vec;

pub const PAGE_SIZE: u64 = 4096;
/// Lowest address `mmap` hands out or accepts (Linux's `mmap_min_addr`):
/// page zero and its neighbours stay unmapped so null dereferences fault.
pub const MIN_ADDRESS: u64 = 0x1_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Protection {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl Protection {
    pub const NONE: Self = Self {
        read: false,
        write: false,
        execute: false,
    };
    pub const READ: Self = Self {
        read: true,
        write: false,
        execute: false,
    };
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
        execute: false,
    };
    pub const READ_EXECUTE: Self = Self {
        read: true,
        write: false,
        execute: true,
    };

    /// From Linux `PROT_*` bits. `EINVAL` for unknown bits; `EACCES` for
    /// writable and executable at once (SaeOS never maps W+X).
    pub fn from_linux(bits: u64) -> Result<Self, Errno> {
        const PROT_READ: u64 = 1;
        const PROT_WRITE: u64 = 2;
        const PROT_EXEC: u64 = 4;
        if bits & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
            return Err(Errno::EINVAL);
        }
        let protection = Self {
            // x86 cannot make a page writable or executable but unreadable.
            read: bits != 0,
            write: bits & PROT_WRITE != 0,
            execute: bits & PROT_EXEC != 0,
        };
        if protection.write && protection.execute {
            return Err(Errno::EACCES);
        }
        Ok(protection)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Image,
    Heap,
    Stack,
    Anonymous,
    /// Device memory, shared (never copied): `physical` is the address
    /// behind the region's first page.
    Device {
        physical: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub protection: Protection,
    pub kind: RegionKind,
}

impl Region {
    /// The part of this region from `start` to `end` (inside it), with a
    /// device region's physical address moved along.
    pub fn piece(&self, start: u64, end: u64) -> Self {
        debug_assert!(self.start <= start && start <= end && end <= self.end);
        let kind = match self.kind {
            RegionKind::Device { physical } => RegionKind::Device {
                physical: physical + (start - self.start),
            },
            kind => kind,
        };
        Self {
            start,
            end,
            protection: self.protection,
            kind,
        }
    }

    /// The physical address behind `address` in a device region.
    pub fn device_address(&self, address: u64) -> Option<u64> {
        match self.kind {
            RegionKind::Device { physical } => Some(physical + (address - self.start)),
            _ => None,
        }
    }
}

/// A page range `start..end` and the protection it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub end: u64,
    pub protection: Protection,
}

/// What a successful `brk` needs the kernel to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrkChange {
    Unchanged,
    Map(Span),
    Unmap(Span),
}

/// What a successful `mmap` needs: unmap `replaced` (MAP_FIXED over
/// existing pages) and then map `new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmapChange {
    pub address: u64,
    pub replaced: Vec<Span>,
    pub new: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryMap {
    regions: Vec<Region>,
    /// The heap begins here (page-aligned, just above the image).
    brk_start: u64,
    /// The current program break (any address; pages cover up to its
    /// page-rounded value).
    brk: u64,
    /// `mmap` places regions below this (the stack guard).
    mmap_top: u64,
}

fn page_up(address: u64) -> Option<u64> {
    address.checked_next_multiple_of(PAGE_SIZE)
}

fn aligned(address: u64) -> bool {
    address.is_multiple_of(PAGE_SIZE)
}

impl MemoryMap {
    /// The map of a freshly exec'd program: its image regions and stack,
    /// with the heap starting at `brk_start` and `mmap` below `mmap_top`.
    pub fn new(regions: &[Region], brk_start: u64, mmap_top: u64) -> Result<Self, Errno> {
        let mut map = Self {
            regions: Vec::new(),
            brk_start,
            brk: brk_start,
            mmap_top,
        };
        for region in regions {
            map.insert(*region)?;
        }
        Ok(map)
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    pub fn brk(&self) -> u64 {
        self.brk
    }

    pub fn region_at(&self, address: u64) -> Option<&Region> {
        self.regions
            .iter()
            .find(|region| region.start <= address && address < region.end)
    }

    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.regions
            .iter()
            .any(|region| start < region.end && region.start < end)
    }

    /// Add a region; `EINVAL` if unaligned or empty, `ENOMEM` if it
    /// overlaps an existing one.
    pub fn insert(&mut self, region: Region) -> Result<(), Errno> {
        if region.start >= region.end || !aligned(region.start) || !aligned(region.end) {
            return Err(Errno::EINVAL);
        }
        if self.overlaps(region.start, region.end) {
            return Err(Errno::ENOMEM);
        }
        let index = self
            .regions
            .partition_point(|other| other.start < region.start);
        self.regions.insert(index, region);
        Ok(())
    }

    /// Remove `start..end` from every region, splitting those that extend
    /// past it. Returns the removed pieces (to unmap), in address order.
    pub fn remove(&mut self, start: u64, end: u64) -> Vec<Span> {
        let mut removed = Vec::new();
        let mut kept = Vec::with_capacity(self.regions.len() + 1);
        for region in self.regions.drain(..) {
            if region.end <= start || end <= region.start {
                kept.push(region);
                continue;
            }
            if region.start < start {
                kept.push(region.piece(region.start, start));
            }
            removed.push(Span {
                start: region.start.max(start),
                end: region.end.min(end),
                protection: region.protection,
            });
            if end < region.end {
                kept.push(region.piece(end, region.end));
            }
        }
        self.regions = kept;
        removed
    }

    /// The highest free gap of `length` bytes below `mmap_top` and above
    /// everything else (image and heap), top-down like Linux.
    fn find_free(&self, length: u64) -> Option<u64> {
        let floor = page_up(self.brk)?.max(MIN_ADDRESS);
        let mut top = self.mmap_top;
        for region in self.regions.iter().rev() {
            if region.start >= top {
                continue;
            }
            if region.end <= top && top - region.end >= length {
                let start = top - length;
                if start >= floor {
                    return Some(start);
                }
                return None;
            }
            top = top.min(region.start);
        }
        top.checked_sub(length).filter(|&start| start >= floor)
    }

    /// `mmap` of anonymous private memory. `hint` is a preferred address
    /// (`fixed`: required, replacing anything already there).
    pub fn mmap(
        &mut self,
        hint: u64,
        length: u64,
        protection: Protection,
        fixed: bool,
    ) -> Result<MmapChange, Errno> {
        self.place(hint, length, protection, fixed, RegionKind::Anonymous)
    }

    /// `mmap` of device memory at `physical` (page-aligned): placed as
    /// [`MemoryMap::mmap`] places anonymous memory. Never executable.
    pub fn mmap_device(
        &mut self,
        hint: u64,
        length: u64,
        protection: Protection,
        physical: u64,
        fixed: bool,
    ) -> Result<MmapChange, Errno> {
        if !aligned(physical) || protection.execute {
            return Err(Errno::EINVAL);
        }
        self.place(
            hint,
            length,
            protection,
            fixed,
            RegionKind::Device { physical },
        )
    }

    fn place(
        &mut self,
        hint: u64,
        length: u64,
        protection: Protection,
        fixed: bool,
        kind: RegionKind,
    ) -> Result<MmapChange, Errno> {
        if length == 0 {
            return Err(Errno::EINVAL);
        }
        let length = page_up(length).ok_or(Errno::ENOMEM)?;
        if fixed && !aligned(hint) {
            return Err(Errno::EINVAL);
        }
        let usable = |start: u64| {
            start >= MIN_ADDRESS
                && aligned(start)
                && start
                    .checked_add(length)
                    .is_some_and(|end| end <= self.mmap_top)
        };
        let (address, replaced) = if fixed {
            if !usable(hint) {
                return Err(Errno::ENOMEM);
            }
            (hint, self.remove(hint, hint + length))
        } else if hint != 0 && usable(hint) && !self.overlaps(hint, hint + length) {
            (hint, Vec::new())
        } else {
            (self.find_free(length).ok_or(Errno::ENOMEM)?, Vec::new())
        };
        let new = Span {
            start: address,
            end: address + length,
            protection,
        };
        self.insert(Region {
            start: new.start,
            end: new.end,
            protection,
            kind,
        })?;
        Ok(MmapChange {
            address,
            replaced,
            new,
        })
    }

    /// `munmap`: any mapped pages in the range go (even image or stack, as
    /// on Linux); unmapped parts are fine.
    pub fn munmap(&mut self, start: u64, length: u64) -> Result<Vec<Span>, Errno> {
        if !aligned(start) || length == 0 {
            return Err(Errno::EINVAL);
        }
        let end = start
            .checked_add(page_up(length).ok_or(Errno::EINVAL)?)
            .ok_or(Errno::EINVAL)?;
        Ok(self.remove(start, end))
    }

    /// `mprotect`: the whole range must be mapped (`ENOMEM` otherwise).
    /// Returns the spans to re-protect.
    pub fn mprotect(
        &mut self,
        start: u64,
        length: u64,
        protection: Protection,
    ) -> Result<Vec<Span>, Errno> {
        if !aligned(start) {
            return Err(Errno::EINVAL);
        }
        let end = start
            .checked_add(page_up(length).ok_or(Errno::ENOMEM)?)
            .ok_or(Errno::ENOMEM)?;
        if start == end {
            return Ok(Vec::new());
        }
        // Covered without gaps?
        let mut cursor = start;
        for region in &self.regions {
            if region.end <= cursor || region.start >= end {
                continue;
            }
            if region.start > cursor {
                return Err(Errno::ENOMEM);
            }
            cursor = region.end;
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            return Err(Errno::ENOMEM);
        }
        // Device memory is never executable.
        let inside = |region: &&Region| region.start < end && start < region.end;
        if protection.execute
            && self
                .regions
                .iter()
                .filter(inside)
                .any(|region| matches!(region.kind, RegionKind::Device { .. }))
        {
            return Err(Errno::EACCES);
        }
        // Split at the boundaries, then change the pieces inside.
        let kinds: Vec<Region> = self.regions.iter().filter(inside).copied().collect();
        self.remove(start, end);
        let mut changed = Vec::new();
        for region in kinds {
            let piece = Region {
                protection,
                ..region.piece(region.start.max(start), region.end.min(end))
            };
            self.insert(piece)?;
            changed.push(Span {
                start: piece.start,
                end: piece.end,
                protection,
            });
        }
        Ok(changed)
    }

    /// `brk(address)`: move the program break. Returns the change to apply,
    /// or `ENOMEM` (the break stays where it was) if the heap would run into
    /// another region or below its start.
    pub fn set_brk(&mut self, address: u64) -> Result<BrkChange, Errno> {
        if address < self.brk_start {
            return Err(Errno::ENOMEM);
        }
        let old_end = page_up(self.brk).ok_or(Errno::ENOMEM)?;
        let new_end = page_up(address).ok_or(Errno::ENOMEM)?;
        // Another mapping inside the heap's range (MAP_FIXED over it, or in
        // the way of growth) blocks the move, so the map stays consistent.
        let heap_limit = old_end.max(new_end);
        if new_end > self.mmap_top
            || self.regions.iter().any(|region| {
                region.kind != RegionKind::Heap
                    && region.start < heap_limit
                    && self.brk_start < region.end
            })
        {
            return Err(Errno::ENOMEM);
        }
        let change = if new_end > old_end {
            BrkChange::Map(Span {
                start: old_end,
                end: new_end,
                protection: Protection::READ_WRITE,
            })
        } else if new_end < old_end {
            BrkChange::Unmap(Span {
                start: new_end,
                end: old_end,
                protection: Protection::READ_WRITE,
            })
        } else {
            BrkChange::Unchanged
        };
        // The heap region is brk_start..page_up(brk); nothing else changes
        // (a MAP_FIXED mapping that cut into the heap stays where it is).
        self.regions
            .retain(|region| region.kind != RegionKind::Heap);
        if new_end > self.brk_start {
            self.insert(Region {
                start: self.brk_start,
                end: new_end,
                protection: Protection::READ_WRITE,
                kind: RegionKind::Heap,
            })?;
        }
        self.brk = address;
        Ok(change)
    }
}
