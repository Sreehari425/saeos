//! Kernel-owned page tables. Table pages are ordinary physical frames.
use super::frame::{self, PAGE_SIZE, PhysAddr, PhysFrame, VirtAddr};
use super::layout::{self, static_phys};
use crate::boot::boot_info::KernelSections;
use crate::sync::IrqSpinMutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

pub const KERNEL_VIRT_BASE: u64 = 0xffff_8000_0000_0000;
pub const PRESENT: u64 = 1;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const HUGE: u64 = 1 << 7;
pub const GIGABYTE: u64 = 1 << 30;
pub const TWO_MIB: u64 = 1 << 21;
pub const NO_EXECUTE: u64 = 1 << 63;
pub const PWT: u64 = 1 << 3;
pub const PCD: u64 = 1 << 4;
pub const PHYS_MASK_4K: u64 = 0x000f_ffff_ffff_f000;
pub const PHYS_MASK_2M: u64 = 0x000f_ffff_ffe0_0000;
pub const PHYS_MASK_1G: u64 = 0x000f_ffff_c000_0000;
const PAT: u64 = 1 << 12;
const PAT_4K: u64 = 1 << 7;
const LEAF_ATTRIBUTES: u64 = PRESENT | WRITABLE | USER | PWT | PCD | NO_EXECUTE;
const ADDRESS_MASK: u64 = PHYS_MASK_4K;
// PML4 slots 256..511 occupy the canonical upper-half window: 128 TiB.
const DIRECT_MAP_LIMIT: u64 = 1 << 47;
const IDENTITY_MAP_LIMIT: u64 = 1 << 47;

/// TLB invalidation hooks used by address-space mutation. Inactive roots use
/// the no-op path; the scheduler can later install shootdown behavior here.
pub trait TlbInvalidation {
    fn invalidate_page(&self, address: VirtAddr);
    fn flush_address_space(&self, root: PhysFrame);
}

pub struct LocalTlb;
impl TlbInvalidation for LocalTlb {
    fn invalidate_page(&self, address: VirtAddr) {
        invalidate_page(address);
    }
    fn flush_address_space(&self, root: PhysFrame) {
        flush_address_space(root);
    }
}

pub fn invalidate_page(address: VirtAddr) {
    if ACTIVE.load(Ordering::Acquire) {
        unsafe {
            core::arch::asm!("invlpg [{}]", in(reg) address.0, options(nostack, preserves_flags));
        }
    }
}

pub fn flush_current() {
    if ACTIVE.load(Ordering::Acquire) {
        let root: u64;
        unsafe {
            core::arch::asm!("mov {}, cr3", out(reg) root, options(nostack, preserves_flags));
            core::arch::asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags));
        }
    }
}

/// Flush `root` only when it is the currently loaded address space. This is
/// deliberately a no-op for inactive roots; a scheduler can later replace
/// this with an SMP shootdown implementation.
pub fn flush_address_space(root: PhysFrame) {
    if ACTIVE.load(Ordering::Acquire) && current_root() == root {
        flush_current();
    }
}

/// The page-table root currently loaded in CR3.
pub fn current_root() -> PhysFrame {
    let root: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) root, options(nomem, nostack, preserves_flags));
    }
    PhysFrame(root & PHYS_MASK_4K)
}

#[inline]
pub const fn extract_1g_address(entry: u64) -> u64 {
    entry & PHYS_MASK_1G
}
#[inline]
pub const fn extract_2m_address(entry: u64) -> u64 {
    entry & PHYS_MASK_2M
}
#[inline]
pub const fn extract_4k_address(entry: u64) -> u64 {
    entry & PHYS_MASK_4K
}

/// Translate PAT when splitting a 1 GiB leaf into 2 MiB leaves.
#[inline]
pub const fn pat_1g_to_2m(entry: u64) -> u64 {
    entry & PAT
}

/// Translate PAT when splitting a 2 MiB leaf into 4 KiB leaves.
#[inline]
pub const fn pat_2m_to_4k(entry: u64) -> u64 {
    if entry & PAT != 0 { PAT_4K } else { 0 }
}

#[inline]
pub const fn inherited_leaf_flags(entry: u64, to_4k: bool) -> u64 {
    (entry & LEAF_ATTRIBUTES)
        | if to_4k {
            pat_2m_to_4k(entry)
        } else {
            pat_1g_to_2m(entry)
        }
}

#[inline]
pub const fn valid_wx(flags: u64) -> bool {
    !(flags & WRITABLE != 0 && flags & NO_EXECUTE == 0)
}

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct PageTable([u64; 512]);
static mut PML4: PageTable = PageTable([0; 512]);
// UEFI may leave ordinary usable pages inaccessible or read-only in its own
// page tables. These few image-backed pages provide a writable identity map
// while allocator-backed table frames are being initialized.
static mut BOOTSTRAP_PML4: PageTable = PageTable([0; 512]);
static mut BOOTSTRAP_PDPT: PageTable = PageTable([0; 512]);
// High alias of the bootstrap identity window for an image that already runs
// in the top 2 GiB (BIOS). It reuses the identity PDs below.
static mut BOOTSTRAP_PDPT_HIGH: PageTable = PageTable([0; 512]);
// UEFI may relocate the loaded image above 4 GiB while the final tables are
// being built. Cover that image in the temporary identity map.
// The temporary identity window covers the boot-reserved allocator metadata
// before the final direct map is installed. Each entry covers 1 GiB.
const BOOTSTRAP_PD_COUNT: usize = 16;
static mut BOOTSTRAP_PD: [PageTable; BOOTSTRAP_PD_COUNT] =
    [PageTable([0; 512]); BOOTSTRAP_PD_COUNT];
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Serializes every edit of the shared kernel half (PML4 256..512 and the
/// tables below it). Lock order: `AddressSpace.state` -> this -> frame
/// allocator. Nothing may allocate from the heap while holding it.
static KERNEL_MAP_LOCK: IrqSpinMutex<()> = IrqSpinMutex::new(());
/// Set once every upper-half PML4 entry exists. From then on the kernel only
/// edits tables below those entries, so roots copied by `AddressSpace::new`
/// never go stale.
static KERNEL_SLOTS_FROZEN: AtomicBool = AtomicBool::new(false);
/// Set once the boot identity map is gone; the lower half then belongs to
/// user address spaces only.
static LOWER_HALF_DROPPED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags(pub u64);
impl PageFlags {
    pub const KERNEL_TEXT: Self = Self(PRESENT);
    pub const READ_ONLY: Self = Self(PRESENT | NO_EXECUTE);
    pub const WRITABLE: Self = Self(PRESENT | WRITABLE | NO_EXECUTE);
    pub const MMIO: Self = Self(PRESENT | WRITABLE | NO_EXECUTE);
    pub const USER_READ: Self = Self(PRESENT | USER | NO_EXECUTE);
    pub const USER_READ_WRITE: Self = Self(PRESENT | USER | WRITABLE | NO_EXECUTE);
    pub const USER_READ_EXECUTE: Self = Self(PRESENT | USER);

    pub const fn with_cache(self, write_through: bool, cache_disable: bool) -> Self {
        Self(self.0 | if write_through { PWT } else { 0 } | if cache_disable { PCD } else { 0 })
    }
    pub const fn is_writable(self) -> bool {
        self.0 & WRITABLE != 0
    }
    pub const fn is_user(self) -> bool {
        self.0 & USER != 0
    }
    pub const fn is_executable(self) -> bool {
        self.0 & NO_EXECUTE == 0
    }
}

/// Permissions understood by the kernel's future process/address-space
/// layer. These are intentionally not a public userspace ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserPageFlags(u8);

impl UserPageFlags {
    pub const USER_READ: Self = Self(1 << 0);
    pub const USER_WRITE: Self = Self(1 << 1);
    pub const USER_EXECUTE: Self = Self(1 << 2);
    pub const USER_NO_EXECUTE: Self = Self(1 << 3);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn contains(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
    const fn is_valid(self) -> bool {
        self.contains(Self::USER_READ)
            && !(self.contains(Self::USER_EXECUTE) && self.contains(Self::USER_NO_EXECUTE))
    }
    const fn hardware(self) -> u64 {
        PRESENT
            | USER
            | if self.contains(Self::USER_WRITE) {
                WRITABLE
            } else {
                0
            }
            | if self.contains(Self::USER_NO_EXECUTE) || !self.contains(Self::USER_EXECUTE) {
                NO_EXECUTE
            } else {
                0
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSpaceError {
    OutOfFrames,
    Unaligned,
    Noncanonical,
    KernelAddress,
    InvalidFlags,
    AlreadyMapped,
    NotMapped,
    HugePage,
    InvalidPhysicalAddress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingOwnership {
    Borrowed,
    Owned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnmappedMapping {
    pub frame: PhysFrame,
    pub ownership: MappingOwnership,
    pub permissions: PageFlags,
    pub cache_attributes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OwnedMapping {
    virtual_address: VirtAddr,
    frame: PhysFrame,
}

struct AddressSpaceState {
    owned: Vec<OwnedMapping>,
}

/// A page-table root for a future process. Kernel higher-half entries are
/// shared, while all lower-half entries belong exclusively to this root.
pub struct AddressSpace {
    root: PhysFrame,
    state: IrqSpinMutex<AddressSpaceState>,
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        assert!(
            current_root() != self.root,
            "dropping the active address space {:#x}",
            self.root.0
        );
        // PML4 entries 0..=255 are private to this address space.  The
        // higher-half entries are copied from PML4 and must never be freed.
        unsafe {
            let root = &mut *table_ptr(self.root);
            for entry in root.0[..256].iter_mut() {
                if *entry & PRESENT != 0 {
                    free_table_tree(PhysFrame(*entry & ADDRESS_MASK), 3);
                    *entry = 0;
                }
            }
        }
        let owned = {
            let mut state = self.state.lock();
            core::mem::take(&mut state.owned)
        };
        for mapping in owned.iter() {
            frame::free_frame(mapping.frame);
        }
        frame::free_frame(self.root);
    }
}

impl AddressSpace {
    pub fn new() -> Result<Self, AddressSpaceError> {
        let root = alloc_table().ok_or(AddressSpaceError::OutOfFrames)?;
        unsafe {
            let new_root = &mut *table_ptr(root);
            let kernel_root = &raw const PML4;
            for index in 256..512 {
                new_root.0[index] = (*kernel_root).0[index];
            }
        }
        Ok(Self {
            root,
            state: IrqSpinMutex::new(AddressSpaceState { owned: Vec::new() }),
        })
    }

    pub fn root_frame(&self) -> PhysFrame {
        self.root
    }

    /// Invalidate a lower-half translation only when this root is loaded; an
    /// inactive root has no cached entries on this CPU.
    // SMP: other CPUs running this address space need a shootdown here.
    fn flush_if_active(&self, virtual_address: VirtAddr) {
        if current_root() == self.root {
            invalidate_page(virtual_address);
        }
    }

    /// Explicitly tear down the private address space. `Drop` performs the
    /// same cleanup for callers that rely on RAII.
    pub fn destroy(self) -> Result<(), AddressSpaceError> {
        drop(self);
        Ok(())
    }

    pub fn map(
        &self,
        virtual_address: VirtAddr,
        physical_frame: PhysFrame,
        flags: PageFlags,
    ) -> Result<(), AddressSpaceError> {
        let _guard = self.state.lock();
        validate_user_mapping(virtual_address, physical_frame, flags)?;
        let result =
            unsafe { map_page_in_root(self.root, virtual_address, physical_frame, flags.0) };
        if result.is_ok() {
            self.flush_if_active(virtual_address);
        }
        result
    }

    pub fn map_owned(
        &self,
        virtual_address: VirtAddr,
        physical_frame: PhysFrame,
        flags: PageFlags,
    ) -> Result<(), AddressSpaceError> {
        {
            let mut state = self.state.lock();
            state
                .owned
                .try_reserve(1)
                .map_err(|_| AddressSpaceError::OutOfFrames)?;
        }
        self.map(virtual_address, physical_frame, flags)?;
        let mut state = self.state.lock();
        state.owned.push(OwnedMapping {
            virtual_address,
            frame: physical_frame,
        });
        Ok(())
    }

    pub fn unmap_mapping(
        &self,
        virtual_address: VirtAddr,
    ) -> Result<UnmappedMapping, AddressSpaceError> {
        let (frame, entry) = {
            let _guard = self.state.lock();
            let entry = self.page_entry_inner(virtual_address)?;
            let frame = unsafe { unmap_page_in_root(self.root, virtual_address) }?;
            (frame, entry)
        };
        self.flush_if_active(virtual_address);
        let ownership = self.remove_owned(virtual_address, frame);
        Ok(UnmappedMapping {
            frame,
            ownership,
            permissions: PageFlags(entry & !ADDRESS_MASK),
            cache_attributes: entry & (PWT | PCD),
        })
    }

    fn remove_owned(&self, virtual_address: VirtAddr, frame: PhysFrame) -> MappingOwnership {
        let mut state = self.state.lock();
        if let Some(index) = state
            .owned
            .iter()
            .position(|mapping| mapping.virtual_address == virtual_address)
        {
            let mapping = state.owned.swap_remove(index);
            return if mapping.frame == frame {
                MappingOwnership::Owned
            } else {
                MappingOwnership::Borrowed
            };
        }
        MappingOwnership::Borrowed
    }

    pub fn map_user_page(
        &self,
        virtual_address: VirtAddr,
        physical_frame: PhysFrame,
        flags: UserPageFlags,
    ) -> Result<(), AddressSpaceError> {
        if !flags.is_valid() {
            return Err(AddressSpaceError::InvalidFlags);
        }
        self.map(virtual_address, physical_frame, PageFlags(flags.hardware()))
    }

    pub fn unmap(&self, virtual_address: VirtAddr) -> Result<UnmappedMapping, AddressSpaceError> {
        self.unmap_mapping(virtual_address)
    }

    pub fn unmap_user_page(
        &self,
        virtual_address: VirtAddr,
    ) -> Result<PhysFrame, AddressSpaceError> {
        self.unmap(virtual_address).map(|mapping| mapping.frame)
    }

    pub fn protect(
        &mut self,
        virtual_address: VirtAddr,
        flags: PageFlags,
    ) -> Result<(), AddressSpaceError> {
        let _guard = self.state.lock();
        validate_user_mapping(virtual_address, PhysFrame(0), flags)?;
        unsafe {
            let pml4 = &mut *table_ptr(self.root);
            let pdpt_entry = pml4.0[((virtual_address.0 >> 39) & 0x1ff) as usize];
            if pdpt_entry & PRESENT == 0 {
                return Err(AddressSpaceError::NotMapped);
            }
            let pdpt = &mut *table_ptr(PhysFrame(pdpt_entry & ADDRESS_MASK));
            let pd_entry = pdpt.0[((virtual_address.0 >> 30) & 0x1ff) as usize];
            if pd_entry & PRESENT == 0 || pd_entry & HUGE != 0 {
                return Err(if pd_entry & HUGE != 0 {
                    AddressSpaceError::HugePage
                } else {
                    AddressSpaceError::NotMapped
                });
            }
            let pd = &mut *table_ptr(PhysFrame(pd_entry & ADDRESS_MASK));
            let pt_entry = pd.0[((virtual_address.0 >> 21) & 0x1ff) as usize];
            if pt_entry & PRESENT == 0 || pt_entry & HUGE != 0 {
                return Err(if pt_entry & HUGE != 0 {
                    AddressSpaceError::HugePage
                } else {
                    AddressSpaceError::NotMapped
                });
            }
            let pt = &mut *table_ptr(PhysFrame(pt_entry & ADDRESS_MASK));
            let slot = &mut pt.0[((virtual_address.0 >> 12) & 0x1ff) as usize];
            if *slot & PRESENT == 0 {
                return Err(AddressSpaceError::NotMapped);
            }
            *slot = (*slot & ADDRESS_MASK) | flags.0;
            self.flush_if_active(virtual_address);
            Ok(())
        }
    }

    pub fn query(&self, virtual_address: VirtAddr) -> Result<u64, AddressSpaceError> {
        let _guard = self.state.lock();
        self.page_entry_inner(virtual_address)
    }

    pub fn page_entry(&self, virtual_address: VirtAddr) -> Result<u64, AddressSpaceError> {
        self.query(virtual_address)
    }

    fn page_entry_inner(&self, virtual_address: VirtAddr) -> Result<u64, AddressSpaceError> {
        if virtual_address.0 & (PAGE_SIZE - 1) != 0 {
            return Err(AddressSpaceError::Unaligned);
        }
        if virtual_address.0 >= (1 << 47) {
            return if virtual_address.0 >= KERNEL_VIRT_BASE {
                Err(AddressSpaceError::KernelAddress)
            } else {
                Err(AddressSpaceError::Noncanonical)
            };
        }
        unsafe {
            let pml4 = &*table_ptr(self.root);
            let pdpt = table_from_entry(pml4.0[((virtual_address.0 >> 39) & 0x1ff) as usize])?;
            let pd = table_from_entry(pdpt.0[((virtual_address.0 >> 30) & 0x1ff) as usize])?;
            let pt = table_from_entry(pd.0[((virtual_address.0 >> 21) & 0x1ff) as usize])?;
            let entry = pt.0[((virtual_address.0 >> 12) & 0x1ff) as usize];
            if entry & PRESENT == 0 {
                Err(AddressSpaceError::NotMapped)
            } else {
                Ok(entry)
            }
        }
    }
}

fn validate_user_mapping(
    virtual_address: VirtAddr,
    physical: PhysFrame,
    flags: PageFlags,
) -> Result<(), AddressSpaceError> {
    if virtual_address.0 & (PAGE_SIZE - 1) != 0 || physical.0 & (PAGE_SIZE - 1) != 0 {
        return Err(AddressSpaceError::Unaligned);
    }
    if virtual_address.0 >= (1 << 47) {
        return if virtual_address.0 >= KERNEL_VIRT_BASE {
            Err(AddressSpaceError::KernelAddress)
        } else {
            Err(AddressSpaceError::Noncanonical)
        };
    }
    if !flags.is_user() {
        return Err(AddressSpaceError::InvalidFlags);
    }
    if flags.is_writable() && flags.is_executable() {
        return Err(AddressSpaceError::InvalidFlags);
    }
    Ok(())
}

unsafe fn unmap_page_in_root(
    root: PhysFrame,
    virtual_address: VirtAddr,
) -> Result<PhysFrame, AddressSpaceError> {
    validate_user_mapping(virtual_address, PhysFrame(0), PageFlags::USER_READ)?;
    unsafe {
        let pml4_index = ((virtual_address.0 >> 39) & 0x1ff) as usize;
        let pdpt_index = ((virtual_address.0 >> 30) & 0x1ff) as usize;
        let pd_index = ((virtual_address.0 >> 21) & 0x1ff) as usize;
        let pml4 = &mut *table_ptr(root);
        let pdpt_frame = PhysFrame(pml4.0[pml4_index] & ADDRESS_MASK);
        let pdpt = table_from_entry(pml4.0[pml4_index])?;
        let pd_frame = PhysFrame(pdpt.0[pdpt_index] & ADDRESS_MASK);
        let pd = table_from_entry(pdpt.0[pdpt_index])?;
        let pt_frame = PhysFrame(pd.0[pd_index] & ADDRESS_MASK);
        let pt = table_from_entry(pd.0[pd_index])?;
        let slot = &mut (*(pt as *const PageTable as *mut PageTable)).0
            [((virtual_address.0 >> 12) & 0x1ff) as usize];
        if *slot & PRESENT == 0 {
            return Err(AddressSpaceError::NotMapped);
        }
        let physical = PhysFrame(*slot & ADDRESS_MASK);
        *slot = 0;
        if table_empty(pt) {
            frame::free_frame(pt_frame);
            let pd = &mut *table_ptr(pd_frame);
            pd.0[pd_index] = 0;
            if table_empty(pd) {
                frame::free_frame(pd_frame);
                let pdpt = &mut *table_ptr(pdpt_frame);
                pdpt.0[pdpt_index] = 0;
                if table_empty(pdpt) {
                    frame::free_frame(pdpt_frame);
                    pml4.0[pml4_index] = 0;
                }
            }
        }
        Ok(physical)
    }
}

fn table_empty(table: &PageTable) -> bool {
    table.0.iter().all(|entry| *entry & PRESENT == 0)
}

unsafe fn table_from_entry(entry: u64) -> Result<&'static PageTable, AddressSpaceError> {
    if entry & PRESENT == 0 {
        Err(AddressSpaceError::NotMapped)
    } else if entry & HUGE != 0 {
        Err(AddressSpaceError::HugePage)
    } else {
        Ok(unsafe { &*table_ptr(PhysFrame(entry & ADDRESS_MASK)) })
    }
}

unsafe fn free_table_tree(frame: PhysFrame, levels_below: usize) {
    if levels_below != 0 {
        let table = unsafe { &mut *table_ptr(frame) };
        for entry in table.0.iter_mut() {
            if levels_below > 1 && *entry & PRESENT != 0 && *entry & HUGE == 0 {
                unsafe { free_table_tree(PhysFrame(*entry & ADDRESS_MASK), levels_below - 1) };
            }
            *entry = 0;
        }
    }
    frame::free_frame(frame);
}

fn table_ptr(frame: PhysFrame) -> *mut PageTable {
    // Before the higher-half map is active, table frames are only reachable
    // through the bootstrap identity window. Afterwards prefer the direct map
    // so page-table edits do not depend on the mutable BIOS bootstrap PDPT.
    if ACTIVE.load(Ordering::Acquire) {
        phys_to_virt(PhysAddr(frame.0)).0 as *mut PageTable
    } else {
        frame.0 as *mut PageTable
    }
}
fn overlaps_heap(frame: PhysFrame) -> bool {
    let heap_start = crate::mm::heap::heap_phys_start();
    let heap_end = heap_start.saturating_add(crate::mm::heap::HEAP_SIZE as u64);
    let frame_start = frame.0;
    let frame_end = frame_start + PAGE_SIZE;
    frame_start < heap_end && frame_end > heap_start
}

fn alloc_table() -> Option<PhysFrame> {
    // Before CR3 holds the final tables, `table_ptr` reaches table frames
    // through the bootstrap identity window; afterwards the direct map covers
    // every tracked frame.
    let frame = if ACTIVE.load(Ordering::Acquire) {
        frame::alloc_frame()?
    } else {
        frame::alloc_frame_below(PhysAddr(BOOTSTRAP_PD_COUNT as u64 * GIGABYTE))?
    };
    // The heap is reserved in the frame bitmap at boot; a heap frame here
    // means that reservation is broken, and zeroing it would smash the heap.
    debug_assert!(
        !overlaps_heap(frame),
        "frame allocator returned heap frame {:#x}",
        frame.0
    );
    unsafe {
        (*table_ptr(frame)).0 = [0; 512];
    }
    Some(frame)
}
fn child(table: *mut PageTable, index: usize) -> Option<&'static mut PageTable> {
    unsafe {
        if (*table).0[index] & PRESENT == 0 {
            let frame = alloc_table()?;
            (*table).0[index] = frame.0 | PRESENT | WRITABLE;
        }
        Some(&mut *table_ptr(PhysFrame((*table).0[index] & ADDRESS_MASK)))
    }
}

/// Intermediate entry for user (lower-half) tables. x86 ANDs U/S and R/W
/// across every level, so intermediates are permissive and the leaf alone
/// decides the effective permission. Kernel-half intermediates never get USER.
const USER_TABLE: u64 = PRESENT | WRITABLE | USER;

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn map_page_in_root(
    root: PhysFrame,
    virtual_address: VirtAddr,
    physical: PhysFrame,
    flags: u64,
) -> Result<(), AddressSpaceError> {
    let pml4_index = ((virtual_address.0 >> 39) & 0x1ff) as usize;
    let pdpt_index = ((virtual_address.0 >> 30) & 0x1ff) as usize;
    let pd_index = ((virtual_address.0 >> 21) & 0x1ff) as usize;
    let pml4 = unsafe { &mut *table_ptr(root) };
    let mut pml4_created = false;
    let mut pdpt_created = false;
    let mut pd_created = false;
    if pml4.0[pml4_index] & PRESENT == 0 {
        let frame = alloc_table().ok_or(AddressSpaceError::OutOfFrames)?;
        pml4.0[pml4_index] = frame.0 | USER_TABLE;
        pml4_created = true;
    }
    let pdpt = &mut *table_ptr(PhysFrame(pml4.0[pml4_index] & ADDRESS_MASK));
    if pdpt.0[pdpt_index] & HUGE != 0 {
        rollback_tables(root, virtual_address, pml4_created, false, false);
        return Err(AddressSpaceError::HugePage);
    }
    if pdpt.0[pdpt_index] & PRESENT == 0 {
        let frame = match alloc_table() {
            Some(frame) => frame,
            None => {
                rollback_tables(root, virtual_address, pml4_created, false, false);
                return Err(AddressSpaceError::OutOfFrames);
            }
        };
        pdpt.0[pdpt_index] = frame.0 | USER_TABLE;
        pdpt_created = true;
    }
    let pd = &mut *table_ptr(PhysFrame(pdpt.0[pdpt_index] & ADDRESS_MASK));
    if pd.0[pd_index] & HUGE != 0 {
        rollback_tables(root, virtual_address, pml4_created, pdpt_created, false);
        return Err(AddressSpaceError::HugePage);
    }
    if pd.0[pd_index] & PRESENT == 0 {
        let frame = match alloc_table() {
            Some(frame) => frame,
            None => {
                rollback_tables(root, virtual_address, pml4_created, pdpt_created, false);
                return Err(AddressSpaceError::OutOfFrames);
            }
        };
        pd.0[pd_index] = frame.0 | USER_TABLE;
        pd_created = true;
    }
    let pt = &mut *table_ptr(PhysFrame(pd.0[pd_index] & ADDRESS_MASK));
    if pt.0[((virtual_address.0 >> 12) & 0x1ff) as usize] & PRESENT != 0 {
        rollback_tables(
            root,
            virtual_address,
            pml4_created,
            pdpt_created,
            pd_created,
        );
        return Err(AddressSpaceError::AlreadyMapped);
    }
    let slot = &mut pt.0[((virtual_address.0 >> 12) & 0x1ff) as usize];
    *slot = physical.0 | flags;
    Ok(())
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn rollback_tables(
    root: PhysFrame,
    virtual_address: VirtAddr,
    pml4_created: bool,
    pdpt_created: bool,
    pd_created: bool,
) {
    let pml4_index = ((virtual_address.0 >> 39) & 0x1ff) as usize;
    let pdpt_index = ((virtual_address.0 >> 30) & 0x1ff) as usize;
    let pd_index = ((virtual_address.0 >> 21) & 0x1ff) as usize;
    let pml4 = &mut *table_ptr(root);
    if pd_created {
        let pdpt = &mut *table_ptr(PhysFrame(pml4.0[pml4_index] & ADDRESS_MASK));
        let pd = &mut *table_ptr(PhysFrame(pdpt.0[pdpt_index] & ADDRESS_MASK));
        let frame = PhysFrame(pd.0[pd_index] & ADDRESS_MASK);
        pd.0[pd_index] = 0;
        frame::free_frame(frame);
    }
    if pdpt_created {
        let pdpt = &mut *table_ptr(PhysFrame(pml4.0[pml4_index] & ADDRESS_MASK));
        let frame = PhysFrame(pdpt.0[pdpt_index] & ADDRESS_MASK);
        pdpt.0[pdpt_index] = 0;
        frame::free_frame(frame);
    }
    if pml4_created {
        let frame = PhysFrame(pml4.0[pml4_index] & ADDRESS_MASK);
        frame::free_frame(frame);
    }
    if pml4_created {
        pml4.0[pml4_index] = 0;
    }
}

unsafe fn load_cr3(root: u64) {
    debug_assert!(root < 1 << 52, "virtual address {root:#x} loaded into CR3");
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags));
    }
}

/// Load a writable identity map of the first `BOOTSTRAP_PD_COUNT` GiB while
/// the final tables are built. If the image already runs in the high window
/// (BIOS), the GiB-sized chunks that hold it are aliased there as well.
unsafe fn install_bootstrap_identity_map(kernel_start: u64, kernel_end: u64) {
    unsafe {
        BOOTSTRAP_PML4.0 = [0; 512];
        BOOTSTRAP_PDPT.0 = [0; 512];
        BOOTSTRAP_PDPT_HIGH.0 = [0; 512];
        BOOTSTRAP_PML4.0[0] = static_phys(&raw const BOOTSTRAP_PDPT) | PRESENT | WRITABLE;
        let bootstrap_pd = &raw mut BOOTSTRAP_PD as *mut PageTable;
        for index in 0..BOOTSTRAP_PD_COUNT {
            let pd = &mut *bootstrap_pd.add(index);
            pd.0 = [0; 512];
            BOOTSTRAP_PDPT.0[index] = static_phys(pd) | PRESENT | WRITABLE;
            for entry in 0..512 {
                let physical = (index as u64 * (1 << 30)) + entry as u64 * (1 << 21);
                pd.0[entry] = physical | PRESENT | WRITABLE | HUGE;
            }
        }
        let delta = layout::image_delta();
        if delta != 0 {
            assert!(
                delta.is_multiple_of(GIGABYTE),
                "image delta must be GiB aligned"
            );
            let high_pml4 = ((kernel_start + delta) >> 39) as usize & 0x1ff;
            BOOTSTRAP_PML4.0[high_pml4] =
                static_phys(&raw const BOOTSTRAP_PDPT_HIGH) | PRESENT | WRITABLE;
            let mut gib = kernel_start / GIGABYTE;
            while gib * GIGABYTE < kernel_end {
                let index = gib as usize;
                assert!(
                    index < BOOTSTRAP_PD_COUNT,
                    "kernel image outside bootstrap window"
                );
                let high = gib * GIGABYTE + delta;
                assert_eq!(((high >> 39) & 0x1ff) as usize, high_pml4);
                BOOTSTRAP_PDPT_HIGH.0[((high >> 30) & 0x1ff) as usize] =
                    static_phys(bootstrap_pd.add(index)) | PRESENT | WRITABLE;
                gib += 1;
            }
        }
        load_cr3(static_phys(&raw const BOOTSTRAP_PML4));
    }
}

pub fn init(kernel_start: u64, kernel_end: u64) {
    init_with_mode(kernel_start, kernel_end, KernelSections::EMPTY, 0);
}

pub fn init_with_mode(
    kernel_start: u64,
    kernel_end: u64,
    sections: KernelSections,
    image_high_delta: u64,
) {
    let map = frame::memory_map();
    let _guard = KERNEL_MAP_LOCK.lock();
    crate::arch::x86_64::cpu::enable_nxe();
    unsafe {
        PML4.0 = [0; 512];
    }
    unsafe {
        install_bootstrap_identity_map(kernel_start, kernel_end);
    }
    for table in [
        static_phys(&raw const BOOTSTRAP_PML4),
        static_phys(&raw const BOOTSTRAP_PDPT),
        static_phys(&raw const BOOTSTRAP_PDPT_HIGH),
    ] {
        frame::reserve_range(PhysAddr(table), PhysAddr(table + PAGE_SIZE));
    }
    unsafe {
        let bootstrap_pd = &raw const BOOTSTRAP_PD as *const PageTable;
        for index in 0..BOOTSTRAP_PD_COUNT {
            let address = static_phys(bootstrap_pd.add(index));
            frame::reserve_range(PhysAddr(address), PhysAddr(address + PAGE_SIZE));
        }
    }
    frame::reserve_range(
        PhysAddr(static_phys(&raw const PML4)),
        PhysAddr(static_phys(&raw const PML4) + PAGE_SIZE),
    );
    let gigabyte_pages = supports_1g_pages();
    for region in map.regions[..map.count].iter() {
        if !maps_in_direct_map(region.kind) {
            continue;
        }
        if let Err(error) = map_range(
            region.start,
            region.end(),
            false,
            region.kind,
            gigabyte_pages,
        ) {
            crate::serial_println!(
                "Memory: identity-map construction failed for {:#x}..{:#x}: {}",
                region.start,
                region.end(),
                error
            );
        }
        if let Err(error) = map_range(
            region.start,
            region.end(),
            true,
            region.kind,
            gigabyte_pages,
        ) {
            crate::serial_println!(
                "Memory: direct-map construction failed for {:#x}..{:#x}: {}",
                region.start,
                region.end(),
                error
            );
        }
    }
    if kernel_start < kernel_end {
        // Keep a writable/NX fallback for image bytes which are not described
        // by section metadata, then override the known sections below.
        for direct in [false, true] {
            if let Err(error) = map_range(
                kernel_start,
                kernel_end,
                direct,
                crate::boot::PhysicalMemoryKind::Usable,
                gigabyte_pages,
            ) {
                crate::serial_println!(
                    "Memory: kernel mapping failed for {:#x}..{:#x}: {}",
                    kernel_start,
                    kernel_end,
                    error
                );
            }
        }
    }
    for (section, flags) in [
        (sections.text, PageFlags::KERNEL_TEXT),
        (sections.rodata, PageFlags::READ_ONLY),
        (sections.data, PageFlags::WRITABLE),
        (sections.bss, PageFlags::WRITABLE),
    ] {
        if !section.is_present() {
            continue;
        }
        for direct in [false, true] {
            if let Err(error) = map_pages(section.start, section.end, direct, flags) {
                crate::serial_println!(
                    "Memory: section mapping failed for {:#x}..{:#x}: {}",
                    section.start,
                    section.end,
                    error
                );
            }
        }
    }
    // The heap is embedded in the kernel .bss, so BIOS includes it inside the
    // kernel image reservation above.  It must nevertheless remain writable;
    // otherwise heap initialization writes through a read-only kernel mapping.
    let heap_start = crate::mm::heap::heap_phys_start();
    let heap_end = heap_start.saturating_add(crate::mm::heap::HEAP_SIZE as u64);
    for direct in [false, true] {
        if let Err(error) = map_range(
            heap_start,
            heap_end,
            direct,
            crate::boot::PhysicalMemoryKind::Usable,
            gigabyte_pages,
        ) {
            crate::serial_println!(
                "Memory: heap writable mapping failed for {:#x}..{:#x}: {}",
                heap_start,
                heap_end,
                error
            );
        }
    }
    if image_high_delta != 0
        && kernel_start < kernel_end
        && let Err(error) = map_image_alias(kernel_start, kernel_end, sections, image_high_delta)
    {
        crate::serial_println!("Memory: high kernel image mapping failed: {}", error);
    }
    prepopulate_kernel_slots();
    // Do not restore PML4[0] to BOOTSTRAP_PDPT. The final identity mappings
    // remain active, making the temporary permissive bootstrap aliases
    // unreachable after CR3 activation.
}

/// Give every upper-half PML4 slot a table so the kernel half never changes
/// at the PML4 level again (at most 256 frames). The caller must hold
/// `KERNEL_MAP_LOCK`.
fn prepopulate_kernel_slots() {
    unsafe {
        let root = &raw mut PML4;
        for index in layout::KERNEL_PML4_FIRST..512 {
            if (*root).0[index] & PRESENT == 0 {
                let table = alloc_table().expect("out of frames for kernel PDPTs");
                (*root).0[index] = table.0 | PRESENT | WRITABLE;
            }
        }
    }
    KERNEL_SLOTS_FROZEN.store(true, Ordering::Release);
}

/// Remove the boot identity map from the kernel root. Nothing may use a
/// lower-half address afterwards: process roots will not contain it.
pub fn drop_lower_half() {
    let _guard = KERNEL_MAP_LOCK.lock();
    unsafe {
        let root = &mut *table_ptr(page_table_frame());
        for entry in root.0[..layout::KERNEL_PML4_FIRST].iter_mut() {
            if *entry & PRESENT != 0 {
                free_table_tree(PhysFrame(*entry & ADDRESS_MASK), 3);
                *entry = 0;
            }
        }
    }
    LOWER_HALF_DROPPED.store(true, Ordering::Release);
    flush_current();
}

/// The page-table entries that translate `virtual_address` in `root`, from
/// PML4 down. The returned count stops early at a huge leaf or a non-present
/// entry (which is included).
pub fn walk_entries(root: PhysFrame, virtual_address: VirtAddr) -> ([u64; 4], usize) {
    let mut entries = [0u64; 4];
    if !is_canonical(virtual_address.0) {
        return (entries, 0);
    }
    let indices = table_indices(virtual_address.0);
    let mut table = root;
    for (level, index) in indices.iter().enumerate() {
        let entry = unsafe { (*table_ptr(table)).0[*index] };
        entries[level] = entry;
        if entry & PRESENT == 0 || (level > 0 && level < 3 && entry & HUGE != 0) || level == 3 {
            return (entries, level + 1);
        }
        table = PhysFrame(entry & ADDRESS_MASK);
    }
    (entries, 4)
}

pub fn lower_half_dropped() -> bool {
    LOWER_HALF_DROPPED.load(Ordering::Acquire)
}

/// Whether `root` shares every kernel PML4 entry with the kernel root.
pub fn kernel_half_matches(root: PhysFrame) -> bool {
    let theirs = unsafe { &*table_ptr(root) };
    let ours = unsafe { &*table_ptr(page_table_frame()) };
    (layout::KERNEL_PML4_FIRST..512)
        .all(|index| ours.0[index] & PRESENT != 0 && theirs.0[index] == ours.0[index])
}

/// Whether `virtual_address` has a present translation in `root`.
pub fn is_mapped_in(root: PhysFrame, virtual_address: VirtAddr) -> bool {
    let (entries, levels) = walk_entries(root, virtual_address);
    levels > 0 && entries[levels - 1] & PRESENT != 0
}

fn maps_in_direct_map(kind: crate::boot::PhysicalMemoryKind) -> bool {
    matches!(
        kind,
        crate::boot::PhysicalMemoryKind::Usable
            | crate::boot::PhysicalMemoryKind::AcpiReclaimable
            | crate::boot::PhysicalMemoryKind::AcpiNvs
            | crate::boot::PhysicalMemoryKind::Runtime
            | crate::boot::PhysicalMemoryKind::Mmio
    )
}

fn supports_1g_pages() -> bool {
    (core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 26)) != 0
}

fn map_range(
    start: u64,
    end: u64,
    direct: bool,
    kind: crate::boot::PhysicalMemoryKind,
    gigabyte_pages: bool,
) -> Result<(), &'static str> {
    if start >= end {
        return Ok(());
    }
    if end
        > if direct {
            DIRECT_MAP_LIMIT
        } else {
            IDENTITY_MAP_LIMIT
        }
    {
        return Err("physical range exceeds four-level canonical coverage");
    }
    let mut physical = start;
    let flags = if kind == crate::boot::PhysicalMemoryKind::Reserved {
        PageFlags::KERNEL_TEXT
    } else if kind == crate::boot::PhysicalMemoryKind::Mmio {
        PageFlags::MMIO
    } else {
        PageFlags::WRITABLE
    };
    while physical < end {
        let remaining = end - physical;
        let size = if kind != crate::boot::PhysicalMemoryKind::Mmio
            && gigabyte_pages
            && physical.is_multiple_of(GIGABYTE)
            && remaining >= GIGABYTE
        {
            GIGABYTE
        } else if kind != crate::boot::PhysicalMemoryKind::Mmio
            && physical.is_multiple_of(TWO_MIB)
            && remaining >= TWO_MIB
        {
            TWO_MIB
        } else {
            PAGE_SIZE
        };
        map_leaf(
            VirtAddr(if direct {
                KERNEL_VIRT_BASE + physical
            } else {
                physical
            }),
            PhysAddr(physical),
            size,
            flags,
        )?;
        physical += size;
    }
    Ok(())
}

fn map_pages(start: u64, end: u64, direct: bool, flags: PageFlags) -> Result<(), &'static str> {
    if end
        > if direct {
            DIRECT_MAP_LIMIT
        } else {
            IDENTITY_MAP_LIMIT
        }
    {
        return Err("section range exceeds four-level canonical coverage");
    }
    map_pages_at(start, end, if direct { KERNEL_VIRT_BASE } else { 0 }, flags)
}

/// Map `start..end` at `start + offset` with 4 KiB pages, replacing whatever
/// the range held. The caller must hold `KERNEL_MAP_LOCK`.
fn map_pages_at(start: u64, end: u64, offset: u64, flags: PageFlags) -> Result<(), &'static str> {
    let mut physical = start;
    while physical < end {
        unsafe {
            map_page_locked(
                VirtAddr(physical.wrapping_add(offset)),
                PhysAddr(physical),
                flags,
                Overwrite::Replace,
            )
        }?;
        physical += PAGE_SIZE;
    }
    Ok(())
}

/// Map the kernel image into the top-2-GiB window at `physical + delta`:
/// writable/NX for bytes outside known sections, then each section with its
/// own permissions. The caller must hold `KERNEL_MAP_LOCK`.
fn map_image_alias(
    kernel_start: u64,
    kernel_end: u64,
    sections: KernelSections,
    delta: u64,
) -> Result<(), &'static str> {
    if kernel_start.wrapping_add(delta) < layout::KERNEL_IMAGE_BASE {
        return Err("image alias below the kernel image window");
    }
    map_pages_at(kernel_start, kernel_end, delta, PageFlags::WRITABLE)?;
    for (section, flags) in [
        (sections.text, PageFlags::KERNEL_TEXT),
        (sections.rodata, PageFlags::READ_ONLY),
        (sections.data, PageFlags::WRITABLE),
        (sections.bss, PageFlags::WRITABLE),
    ] {
        if section.is_present() {
            map_pages_at(section.start, section.end, delta, flags)?;
        }
    }
    Ok(())
}

fn map_leaf(
    virtual_address: VirtAddr,
    physical: PhysAddr,
    size: u64,
    flags: PageFlags,
) -> Result<(), &'static str> {
    if size == PAGE_SIZE {
        return unsafe { map_page_locked(virtual_address, physical, flags, Overwrite::Replace) };
    }
    if !is_canonical(virtual_address.0) {
        return Err("physical range exceeds four-level canonical coverage");
    }
    let [root, pdpt_index, pd_index, _] = table_indices(virtual_address.0);
    let pdpt = child(&raw mut PML4, root).ok_or("out of page-table frames")?;
    let slot = if size == GIGABYTE {
        &mut pdpt.0[pdpt_index]
    } else {
        let pd = child(pdpt as *mut PageTable, pdpt_index).ok_or("out of page-table frames")?;
        &mut pd.0[pd_index]
    };
    if *slot & PRESENT != 0 && *slot & HUGE == 0 {
        // A finer-grained table already covers this range (for example a
        // section that was mapped with 4 KiB permissions). Overwriting it
        // with a huge leaf would leak the table and discard those
        // permissions, so fill it page by page instead.
        let mut offset = 0;
        while offset < size {
            unsafe {
                map_page_locked(
                    VirtAddr(virtual_address.0 + offset),
                    PhysAddr(physical.0 + offset),
                    flags,
                    Overwrite::Replace,
                )
            }?;
            offset += PAGE_SIZE;
        }
        return Ok(());
    }
    *slot = physical.0 | flags.0 | HUGE;
    Ok(())
}

#[inline]
pub fn higher_half(p: PhysAddr) -> VirtAddr {
    VirtAddr(KERNEL_VIRT_BASE + p.0)
}
#[inline]
pub fn phys_to_virt(p: PhysAddr) -> VirtAddr {
    higher_half(p)
}
#[inline]
pub fn virt_to_phys(v: VirtAddr) -> Option<PhysAddr> {
    if v.0 >= KERNEL_VIRT_BASE && v.0 - KERNEL_VIRT_BASE < DIRECT_MAP_LIMIT {
        Some(PhysAddr(v.0 - KERNEL_VIRT_BASE))
    } else if v.0 < IDENTITY_MAP_LIMIT {
        Some(PhysAddr(v.0))
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overwrite {
    /// Fail if a different translation is already present.
    Reject,
    /// Replace whatever is present (boot-time permission overrides).
    Replace,
}

/// Map one kernel-owned 4 KiB page in the shared kernel tables. An identical
/// existing mapping is accepted; a different one is an error, so accidental
/// double-mappings surface instead of silently retargeting a page.
pub fn map_page(
    virtual_address: VirtAddr,
    physical: PhysAddr,
    flags: PageFlags,
) -> Result<(), &'static str> {
    let _guard = KERNEL_MAP_LOCK.lock();
    unsafe { map_page_locked(virtual_address, physical, flags, Overwrite::Reject) }
}

/// Like [`map_page`], but deliberately replaces an existing translation.
pub fn remap_page(
    virtual_address: VirtAddr,
    physical: PhysAddr,
    flags: PageFlags,
) -> Result<(), &'static str> {
    let _guard = KERNEL_MAP_LOCK.lock();
    unsafe { map_page_locked(virtual_address, physical, flags, Overwrite::Replace) }
}

const fn table_indices(virtual_address: u64) -> [usize; 4] {
    [
        ((virtual_address >> 39) & 0x1ff) as usize,
        ((virtual_address >> 30) & 0x1ff) as usize,
        ((virtual_address >> 21) & 0x1ff) as usize,
        ((virtual_address >> 12) & 0x1ff) as usize,
    ]
}

const fn is_canonical(virtual_address: u64) -> bool {
    virtual_address < (1 << 47) || virtual_address >= KERNEL_VIRT_BASE
}

/// # Safety
/// The caller must hold `KERNEL_MAP_LOCK`.
unsafe fn map_page_locked(
    virtual_address: VirtAddr,
    physical: PhysAddr,
    flags: PageFlags,
    mode: Overwrite,
) -> Result<(), &'static str> {
    if virtual_address.0 & (PAGE_SIZE - 1) != 0 || physical.0 & (PAGE_SIZE - 1) != 0 {
        return Err("unaligned page mapping");
    }
    if !is_canonical(virtual_address.0) {
        return Err("address outside canonical paging coverage");
    }
    debug_assert!(
        physical.0 < 1 << 52,
        "virtual address {:#x} used as physical",
        physical.0
    );
    let [pml4_index, pdpt_index, pd_index, pt_index] = table_indices(virtual_address.0);
    if pml4_index < layout::KERNEL_PML4_FIRST && LOWER_HALF_DROPPED.load(Ordering::Acquire) {
        return Err("lower half is reserved for user address spaces");
    }
    let mut split = false;
    unsafe {
        let root = &*table_ptr(page_table_frame());
        assert!(
            pml4_index < layout::KERNEL_PML4_FIRST
                || !KERNEL_SLOTS_FROZEN.load(Ordering::Acquire)
                || root.0[pml4_index] & PRESENT != 0,
            "kernel PML4 slot {pml4_index} created after the kernel half was frozen"
        );
        let pdpt =
            child(table_ptr(page_table_frame()), pml4_index).ok_or("out of page-table frames")?;
        if pdpt.0[pdpt_index] & HUGE != 0 {
            let old_entry = pdpt.0[pdpt_index];
            let old = extract_1g_address(old_entry);
            let inherited = inherited_leaf_flags(old_entry, false);
            let pd_frame = alloc_table().ok_or("out of page-table frames")?;
            let pd = &mut *table_ptr(pd_frame);
            for i in 0..512 {
                pd.0[i] = (old + i as u64 * TWO_MIB) | inherited | HUGE;
            }
            pdpt.0[pdpt_index] = pd_frame.0 | PRESENT | WRITABLE;
            split = true;
        }
        let pd = child(pdpt as *mut PageTable, pdpt_index).ok_or("out of page-table frames")?;
        if pd.0[pd_index] & HUGE != 0 {
            let old_entry = pd.0[pd_index];
            let old = extract_2m_address(old_entry);
            // PAT occupies bit 12 in a 2 MiB leaf and bit 7 in a 4 KiB leaf.
            let inherited = inherited_leaf_flags(old_entry, true);
            let pt_frame = alloc_table().ok_or("out of page-table frames")?;
            let pt = &mut *table_ptr(pt_frame);
            for i in 0..512 {
                pt.0[i] = (old + i as u64 * PAGE_SIZE) | inherited;
            }
            pd.0[pd_index] = pt_frame.0 | PRESENT | WRITABLE;
            split = true;
        }
        let pt = child(pd as *mut PageTable, pd_index).ok_or("out of page-table frames")?;
        let old = pt.0[pt_index];
        let new = physical.0 | flags.0;
        if old & PRESENT != 0 && old != new && mode == Overwrite::Reject {
            if split {
                flush_current();
            }
            return Err("page already mapped");
        }
        pt.0[pt_index] = new;
        // A split changes the page size backing this range; the CPU may hold
        // either size until flushed, so drop everything. Splits only happen at
        // boot and for MMIO, where a full flush is cheap.
        if split {
            flush_current();
        } else if old & PRESENT != 0 && old != new {
            invalidate_page(virtual_address);
        }
    }
    Ok(())
}

/// Remove a kernel page from the shared kernel tables and return its frame.
/// This is used by the page-backed heap; user mappings use `AddressSpace`.
pub fn unmap_page(virtual_address: VirtAddr) -> Result<PhysAddr, &'static str> {
    if virtual_address.0 & (PAGE_SIZE - 1) != 0 || virtual_address.0 < KERNEL_VIRT_BASE {
        return Err("invalid kernel page address");
    }
    let [pml4_index, pdpt_index, pd_index, pt_index] = table_indices(virtual_address.0);
    let _guard = KERNEL_MAP_LOCK.lock();
    unsafe {
        let pml4 = &mut *table_ptr(page_table_frame());
        let pdpt = table_from_entry(pml4.0[pml4_index]).map_err(|_| "page not mapped")?;
        let pd = table_from_entry(pdpt.0[pdpt_index]).map_err(|_| "page not mapped")?;
        let pt = table_from_entry(pd.0[pd_index]).map_err(|_| "page not mapped")?;
        let slot = &mut (*(pt as *const PageTable as *mut PageTable)).0[pt_index];
        if *slot & PRESENT == 0 || *slot & HUGE != 0 {
            return Err("page not mapped");
        }
        let physical = PhysAddr(*slot & PHYS_MASK_4K);
        *slot = 0;
        invalidate_page(virtual_address);
        Ok(physical)
    }
}

pub fn activate() {
    unsafe {
        load_cr3(static_phys(&raw const PML4));
        ACTIVE.store(true, Ordering::Release);
    }
    frame::activate_metadata();
}

/// Load an externally owned address-space root. The address-space manager owns
/// the root frame; this function only changes the active CPU page-table root.
pub fn activate_root(root: PhysFrame) {
    if cfg!(debug_assertions) && ACTIVE.load(Ordering::Acquire) {
        let (ip, sp): (u64, u64);
        unsafe {
            core::arch::asm!("lea {}, [rip]", "mov {}, rsp", out(reg) ip, out(reg) sp,
                options(nomem, nostack, preserves_flags));
        }
        assert!(
            is_mapped_in(root, VirtAddr(ip & !(PAGE_SIZE - 1)))
                && is_mapped_in(root, VirtAddr(sp & !(PAGE_SIZE - 1))),
            "root {:#x} does not map the running code {ip:#x} or stack {sp:#x}",
            root.0
        );
    }
    unsafe {
        load_cr3(root.0);
        ACTIVE.store(true, Ordering::Release);
    }
    frame::activate_metadata();
}

#[cfg(feature = "paging-debug")]
#[derive(Clone, Copy)]
struct MappingInfo {
    physical: u64,
    size: u64,
    flags: u64,
}

#[cfg(feature = "paging-debug")]
unsafe fn inspect_mapping(virtual_address: u64) -> Option<MappingInfo> {
    if !is_canonical(virtual_address) {
        return None;
    }
    let [pml4_index, pdpt_index, pd_index, pt_index] = table_indices(virtual_address);
    let normalized = virtual_address;
    let pml4_ptr = core::ptr::addr_of!(PML4);
    let pml4 = unsafe { &*pml4_ptr };
    let pml4_entry = pml4.0[pml4_index];
    if pml4_entry & PRESENT == 0 {
        return None;
    }
    let pdpt = unsafe { &*table_ptr(PhysFrame(pml4_entry & ADDRESS_MASK)) };
    let pdpt_entry = pdpt.0[pdpt_index];
    if pdpt_entry & PRESENT == 0 {
        return None;
    }
    if pdpt_entry & HUGE != 0 {
        return Some(MappingInfo {
            physical: extract_1g_address(pdpt_entry) + (normalized & (GIGABYTE - 1)),
            size: GIGABYTE,
            flags: pdpt_entry,
        });
    }
    let pd = unsafe { &*table_ptr(PhysFrame(pdpt_entry & ADDRESS_MASK)) };
    let pd_entry = pd.0[pd_index];
    if pd_entry & PRESENT == 0 {
        return None;
    }
    if pd_entry & HUGE != 0 {
        return Some(MappingInfo {
            physical: extract_2m_address(pd_entry) + (normalized & (TWO_MIB - 1)),
            size: TWO_MIB,
            flags: pd_entry,
        });
    }
    let pt = unsafe { &*table_ptr(PhysFrame(pd_entry & ADDRESS_MASK)) };
    let pt_entry = pt.0[pt_index];
    if pt_entry & PRESENT == 0 {
        return None;
    }
    Some(MappingInfo {
        physical: (pt_entry & ADDRESS_MASK) + (normalized & (PAGE_SIZE - 1)),
        size: PAGE_SIZE,
        flags: pt_entry,
    })
}

#[cfg(feature = "paging-debug")]
fn permission_label(flags: u64) -> &'static str {
    match (flags & WRITABLE != 0, flags & NO_EXECUTE != 0) {
        (true, true) => "RW- NX",
        (true, false) => "RWX",
        (false, true) => "R-- NX",
        (false, false) => "R-X",
    }
}

#[cfg(feature = "paging-debug")]
fn dump_mapping(section: &str, point: &str, alias: &str, virtual_address: u64) {
    let result = unsafe { inspect_mapping(virtual_address) };
    match result {
        Some(info) => crate::serial_println!(
            "[PAGING] {} {} {} virt={:#x} phys={:#x} leaf={} flags={}",
            section,
            point,
            alias,
            virtual_address,
            info.physical,
            info.size,
            permission_label(info.flags)
        ),
        None => crate::serial_println!(
            "[PAGING] {} {} {} virt={:#x} UNMAPPED",
            section,
            point,
            alias,
            virtual_address
        ),
    }
}

#[cfg(feature = "paging-debug")]
fn dump_section(name: &str, section: crate::boot::boot_info::KernelSection) {
    if !section.is_present() {
        return;
    }
    crate::serial_println!(
        "[PAGING] {} [{:#x}, {:#x})",
        name,
        section.start,
        section.end
    );
    for (suffix, physical) in [("start", section.start), ("end", section.end - 1)] {
        dump_mapping(name, suffix, "identity", physical);
        dump_mapping(name, suffix, "direct-map", KERNEL_VIRT_BASE + physical);
        if layout::image_delta() != 0 {
            dump_mapping(name, suffix, "image", layout::image_phys_to_virt(physical));
        }
    }
}

#[cfg(feature = "paging-debug")]
pub(crate) fn inspect_final_kernel_mappings(sections: KernelSections) {
    crate::serial_println!("[PAGING] final CR3={:#x}", page_table_frame().0);
    dump_section(".text", sections.text);
    dump_section(".rodata", sections.rodata);
    dump_section(".data", sections.data);
    dump_section(".bss", sections.bss);
}
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}
pub fn page_table_frame() -> PhysFrame {
    PhysFrame(static_phys(&raw const PML4))
}
pub fn reserve_page_tables() {}
