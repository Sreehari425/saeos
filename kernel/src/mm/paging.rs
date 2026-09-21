//! Kernel-owned page tables. Table pages are ordinary physical frames.
use super::frame::{self, PAGE_SIZE, PhysAddr, PhysFrame, VirtAddr};
use crate::boot::boot_info::KernelSections;
use crate::sync::IrqSpinMutex;
use alloc::vec::Vec;

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
    if unsafe { ACTIVE } {
        unsafe {
            core::arch::asm!("invlpg [{}]", in(reg) address.0, options(nostack, preserves_flags));
        }
    }
}

pub fn flush_current() {
    if unsafe { ACTIVE } {
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
    if unsafe { ACTIVE } && page_table_frame() == root {
        flush_current();
    }
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
// UEFI may relocate the loaded image above 4 GiB while the final tables are
// being built. Cover that image in the temporary identity map.
// The temporary identity window covers the boot-reserved allocator metadata
// before the final direct map is installed. Each entry covers 1 GiB.
const BOOTSTRAP_PD_COUNT: usize = 16;
static mut BOOTSTRAP_PD: [PageTable; BOOTSTRAP_PD_COUNT] =
    [PageTable([0; 512]); BOOTSTRAP_PD_COUNT];
static mut ACTIVE: bool = false;
static mut BIOS_BOOT: bool = false;

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
            invalidate_page(virtual_address);
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
        invalidate_page(virtual_address);
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
            invalidate_page(virtual_address);
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
    if unsafe { ACTIVE } {
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
    // Never zero a heap-overlapping frame: that corrupts the freelist. If the
    // permanent heap reservation is intact this loop finds a usable frame;
    // otherwise surface OutOfFrames instead of silently smashing the heap.
    let max_attempts = 100;
    for _ in 0..max_attempts {
        let frame = frame::alloc_frame_below(PhysAddr(4 * 1024 * 1024 * 1024))?;
        if !overlaps_heap(frame) {
            unsafe {
                (*table_ptr(frame)).0 = [0; 512];
            }
            return Some(frame);
        }
        frame::free_frame(frame);
    }
    None
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
        pml4.0[pml4_index] = frame.0 | PRESENT | WRITABLE;
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
        pdpt.0[pdpt_index] = frame.0 | PRESENT | WRITABLE;
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
        pd.0[pd_index] = frame.0 | PRESENT | WRITABLE;
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

unsafe fn load_cr3(root: *const PageTable) {
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) root as u64, options(nostack, preserves_flags));
    }
}

unsafe fn install_bootstrap_identity_map() {
    unsafe {
        BOOTSTRAP_PML4.0 = [0; 512];
        BOOTSTRAP_PDPT.0 = [0; 512];
        BOOTSTRAP_PML4.0[0] = (&raw const BOOTSTRAP_PDPT) as u64 | PRESENT | WRITABLE;
        let bootstrap_pd = &raw mut BOOTSTRAP_PD as *mut PageTable;
        for index in 0..BOOTSTRAP_PD_COUNT {
            let pd = &mut *bootstrap_pd.add(index);
            pd.0 = [0; 512];
            BOOTSTRAP_PDPT.0[index] = (pd as *mut PageTable as u64) | PRESENT | WRITABLE;
            for entry in 0..512 {
                let physical = (index as u64 * (1 << 30)) + entry as u64 * (1 << 21);
                pd.0[entry] = physical | PRESENT | WRITABLE | HUGE;
            }
        }
        load_cr3(&raw const BOOTSTRAP_PML4);
    }
}

pub fn init(kernel_start: u64, kernel_end: u64) {
    init_with_mode(
        kernel_start,
        kernel_end,
        KernelSections::EMPTY,
        kernel_start != 0 && kernel_start < 4 * 1024 * 1024 * 1024,
    );
}

pub fn init_with_mode(
    kernel_start: u64,
    kernel_end: u64,
    sections: KernelSections,
    bios_boot: bool,
) {
    let map = frame::memory_map();
    crate::arch::x86_64::cpu::enable_nxe();
    unsafe {
        BIOS_BOOT = bios_boot;
    }
    unsafe {
        PML4.0 = [0; 512];
    }
    unsafe {
        install_bootstrap_identity_map();
    }
    for table in [
        (&raw const BOOTSTRAP_PML4) as u64,
        (&raw const BOOTSTRAP_PDPT) as u64,
    ] {
        frame::reserve_range(PhysAddr(table), PhysAddr(table + PAGE_SIZE));
    }
    unsafe {
        let bootstrap_pd = &raw const BOOTSTRAP_PD as *const PageTable;
        for index in 0..BOOTSTRAP_PD_COUNT {
            let address = bootstrap_pd.add(index) as u64;
            frame::reserve_range(PhysAddr(address), PhysAddr(address + PAGE_SIZE));
        }
    }
    frame::reserve_range(
        PhysAddr((&raw const PML4) as u64),
        PhysAddr((&raw const PML4) as u64 + PAGE_SIZE),
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
    // Do not restore PML4[0] to BOOTSTRAP_PDPT. The final identity mappings
    // remain active, making the temporary permissive bootstrap aliases
    // unreachable after CR3 activation.
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

fn pml4_index(physical: u64, direct: bool) -> Option<usize> {
    let limit = if direct {
        DIRECT_MAP_LIMIT
    } else {
        IDENTITY_MAP_LIMIT
    };
    if physical >= limit {
        return None;
    }
    Some((((physical >> 39) as usize) + if direct { 256 } else { 0 }) & 0x1ff)
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
        return Err("section range exceeds four-level canonical coverage");
    }
    let mut physical = start;
    while physical < end {
        map_page(
            VirtAddr(if direct {
                KERNEL_VIRT_BASE + physical
            } else {
                physical
            }),
            PhysAddr(physical),
            flags,
        )?;
        physical += PAGE_SIZE;
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
        return map_page(virtual_address, physical, flags);
    }
    let normalized = if virtual_address.0 >= KERNEL_VIRT_BASE {
        virtual_address.0 - KERNEL_VIRT_BASE
    } else {
        virtual_address.0
    };
    let root = pml4_index(normalized, virtual_address.0 >= KERNEL_VIRT_BASE)
        .ok_or("physical range exceeds four-level canonical coverage")?;
    let pdpt_index = ((normalized >> 30) & 0x1ff) as usize;
    let pdpt = child(&raw mut PML4, root).ok_or("out of page-table frames")?;
    if size == GIGABYTE {
        pdpt.0[pdpt_index] = physical.0 | flags.0 | HUGE;
    } else {
        let pd = child(pdpt as *mut PageTable, pdpt_index).ok_or("out of page-table frames")?;
        pd.0[((normalized >> 21) & 0x1ff) as usize] = physical.0 | flags.0 | HUGE;
    }
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
#[inline]
pub fn identity(p: PhysAddr) -> VirtAddr {
    VirtAddr(p.0)
}

pub fn map_page(
    virtual_address: VirtAddr,
    physical: PhysAddr,
    flags: PageFlags,
) -> Result<(), &'static str> {
    if virtual_address.0 & (PAGE_SIZE - 1) != 0 || physical.0 & (PAGE_SIZE - 1) != 0 {
        return Err("unaligned page mapping");
    }
    let normalized = if virtual_address.0 >= KERNEL_VIRT_BASE {
        virtual_address.0 - KERNEL_VIRT_BASE
    } else {
        virtual_address.0
    };
    let limit = if virtual_address.0 >= KERNEL_VIRT_BASE {
        DIRECT_MAP_LIMIT
    } else {
        IDENTITY_MAP_LIMIT
    };
    if normalized >= limit {
        return Err("address outside direct map");
    }
    let pml4_index = pml4_index(normalized, virtual_address.0 >= KERNEL_VIRT_BASE)
        .ok_or("address outside canonical paging coverage")?;
    let pdpt_index = ((normalized >> 30) & 0x1ff) as usize;
    let pd_index = ((normalized >> 21) & 0x1ff) as usize;
    let pt_index = ((normalized >> 12) & 0x1ff) as usize;
    unsafe {
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
        }
        let pt = child(pd as *mut PageTable, pd_index).ok_or("out of page-table frames")?;
        pt.0[pt_index] = physical.0 | flags.0;
    }
    Ok(())
}

/// Remove a kernel page from the active root and return its physical frame.
/// This is used by the page-backed heap; user mappings use `AddressSpace`.
pub fn unmap_page(virtual_address: VirtAddr) -> Result<PhysAddr, &'static str> {
    if virtual_address.0 & (PAGE_SIZE - 1) != 0 || virtual_address.0 < KERNEL_VIRT_BASE {
        return Err("invalid kernel page address");
    }
    let normalized = virtual_address.0 - KERNEL_VIRT_BASE;
    if normalized >= DIRECT_MAP_LIMIT {
        return Err("address outside direct map window");
    }
    let pml4_index = pml4_index(normalized, true).ok_or("noncanonical address")?;
    let pdpt_index = ((normalized >> 30) & 0x1ff) as usize;
    let pd_index = ((normalized >> 21) & 0x1ff) as usize;
    let pt_index = ((normalized >> 12) & 0x1ff) as usize;
    unsafe {
        let pml4 = &mut *table_ptr(PhysFrame((&raw const PML4) as u64));
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
        core::arch::asm!("mov cr3, {}", in(reg) (&raw const PML4) as u64, options(nostack, preserves_flags));
        ACTIVE = true;
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
    let direct = virtual_address >= KERNEL_VIRT_BASE;
    let normalized = if direct {
        virtual_address - KERNEL_VIRT_BASE
    } else {
        virtual_address
    };
    let pml4_index = pml4_index(normalized, direct)?;
    let pml4_ptr = core::ptr::addr_of!(PML4);
    let pml4 = unsafe { &*pml4_ptr };
    let pml4_entry = pml4.0[pml4_index];
    if pml4_entry & PRESENT == 0 {
        return None;
    }
    let pdpt = unsafe { &*table_ptr(PhysFrame(pml4_entry & ADDRESS_MASK)) };
    let pdpt_index = ((normalized >> 30) & 0x1ff) as usize;
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
    let pd_index = ((normalized >> 21) & 0x1ff) as usize;
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
    let pt_index = ((normalized >> 12) & 0x1ff) as usize;
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
        dump_mapping(name, suffix, "higher-half", KERNEL_VIRT_BASE + physical);
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
    unsafe { ACTIVE }
}
pub fn prefer_identity_mmio() -> bool {
    unsafe { BIOS_BOOT }
}
pub fn page_table_frame() -> PhysFrame {
    PhysFrame((&raw const PML4) as u64)
}
pub fn reserve_page_tables() {}
