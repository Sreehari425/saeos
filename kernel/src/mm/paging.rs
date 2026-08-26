//! Kernel-owned page tables. Table pages are ordinary physical frames.
use super::frame::{self, PAGE_SIZE, PhysAddr, PhysFrame, VirtAddr};

pub const KERNEL_VIRT_BASE: u64 = 0xffff_8000_0000_0000;
const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const HUGE: u64 = 1 << 7;
const GIGABYTE: u64 = 1 << 30;
const TWO_MIB: u64 = 1 << 21;
const NO_EXECUTE: u64 = 1 << 63;
const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
// PML4 slots 256..511 occupy the canonical upper-half window: 128 TiB.
const DIRECT_MAP_LIMIT: u64 = 1 << 47;
const IDENTITY_MAP_LIMIT: u64 = 1 << 47;

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
}

/// Permissions understood by the kernel's future process/address-space
/// layer. These are intentionally not a public userspace ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UserPageFlags(u8);

impl UserPageFlags {
    pub(crate) const USER_READ: Self = Self(1 << 0);
    pub(crate) const USER_WRITE: Self = Self(1 << 1);
    pub(crate) const USER_EXECUTE: Self = Self(1 << 2);
    pub(crate) const USER_NO_EXECUTE: Self = Self(1 << 3);

    pub(crate) const fn union(self, other: Self) -> Self {
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
pub(crate) enum AddressSpaceError {
    OutOfFrames,
    Unaligned,
    Noncanonical,
    KernelAddress,
    InvalidFlags,
    AlreadyMapped,
    NotMapped,
    HugePage,
}

/// A page-table root for a future process. Kernel higher-half entries are
/// shared, while all lower-half entries belong exclusively to this root.
pub(crate) struct AddressSpace {
    root: PhysFrame,
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
        frame::free_frame(self.root);
    }
}

impl AddressSpace {
    pub(crate) fn new() -> Result<Self, AddressSpaceError> {
        let root = alloc_table().ok_or(AddressSpaceError::OutOfFrames)?;
        unsafe {
            let new_root = &mut *table_ptr(root);
            let kernel_root = &raw const PML4;
            for index in 256..512 {
                new_root.0[index] = (*kernel_root).0[index];
            }
        }
        Ok(Self { root })
    }

    pub(crate) fn root_frame(&self) -> PhysFrame {
        self.root
    }

    pub(crate) fn map_user_page(
        &mut self,
        virtual_address: VirtAddr,
        physical_frame: PhysFrame,
        flags: UserPageFlags,
    ) -> Result<(), AddressSpaceError> {
        if virtual_address.0 & (PAGE_SIZE - 1) != 0 || physical_frame.0 & (PAGE_SIZE - 1) != 0 {
            return Err(AddressSpaceError::Unaligned);
        }
        if virtual_address.0 >= (1 << 47) {
            return if virtual_address.0 >= KERNEL_VIRT_BASE {
                Err(AddressSpaceError::KernelAddress)
            } else {
                Err(AddressSpaceError::Noncanonical)
            };
        }
        if !flags.is_valid() {
            return Err(AddressSpaceError::InvalidFlags);
        }
        unsafe { map_page_in_root(self.root, virtual_address, physical_frame, flags.hardware()) }
    }

    pub(crate) fn unmap_user_page(
        &mut self,
        virtual_address: VirtAddr,
    ) -> Result<PhysFrame, AddressSpaceError> {
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
            let physical = PhysFrame(*slot & ADDRESS_MASK);
            *slot = 0;
            Ok(physical)
        }
    }

    pub(crate) fn page_entry(&self, virtual_address: VirtAddr) -> Result<u64, AddressSpaceError> {
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

unsafe fn map_page_in_root(
    root: PhysFrame,
    virtual_address: VirtAddr,
    physical: PhysFrame,
    flags: u64,
) -> Result<(), AddressSpaceError> {
    let pml4 = unsafe { &mut *table_ptr(root) };
    let pdpt = child(pml4, ((virtual_address.0 >> 39) & 0x1ff) as usize)
        .ok_or(AddressSpaceError::OutOfFrames)?;
    if pdpt.0[((virtual_address.0 >> 30) & 0x1ff) as usize] & HUGE != 0 {
        return Err(AddressSpaceError::HugePage);
    }
    let pd = child(pdpt, ((virtual_address.0 >> 30) & 0x1ff) as usize)
        .ok_or(AddressSpaceError::OutOfFrames)?;
    if pd.0[((virtual_address.0 >> 21) & 0x1ff) as usize] & HUGE != 0 {
        return Err(AddressSpaceError::HugePage);
    }
    let pt = child(pd, ((virtual_address.0 >> 21) & 0x1ff) as usize)
        .ok_or(AddressSpaceError::OutOfFrames)?;
    let slot = &mut pt.0[((virtual_address.0 >> 12) & 0x1ff) as usize];
    if *slot & PRESENT != 0 {
        return Err(AddressSpaceError::AlreadyMapped);
    }
    *slot = physical.0 | flags;
    Ok(())
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
        kernel_start != 0 && kernel_start < 4 * 1024 * 1024 * 1024,
    );
}

pub fn init_with_mode(kernel_start: u64, kernel_end: u64, bios_boot: bool) {
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
        // The loaded image contains both executable code and mutable linker
        // sections (.data/.bss). Until section-aware permissions exist, map
        // the whole image writable so mutable statics outside HEAP_STORAGE
        // remain usable after switching away from firmware page tables.
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
    // Keep the bootstrap identity window for BIOS VGA/APIC MMIO holes only.
    // Heap freelist traffic and dynamic page-table edits must use the direct
    // map after activate(); do not back those with this mutable PDPT.
    unsafe {
        PML4.0[0] = (&raw const BOOTSTRAP_PDPT) as u64 | PRESENT | WRITABLE;
    }
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
        pdpt.0[pdpt_index] = physical.0 | PRESENT | WRITABLE | HUGE;
    } else {
        let pd = child(pdpt as *mut PageTable, pdpt_index).ok_or("out of page-table frames")?;
        pd.0[((normalized >> 21) & 0x1ff) as usize] = physical.0 | PRESENT | WRITABLE | HUGE;
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
        let pdpt = child(&raw mut PML4, pml4_index).ok_or("out of page-table frames")?;
        if pdpt.0[pdpt_index] & HUGE != 0 {
            let old = pdpt.0[pdpt_index] & 0x000f_ffff_c000_0000;
            let pd_frame = alloc_table().ok_or("out of page-table frames")?;
            let pd = &mut *table_ptr(pd_frame);
            for i in 0..512 {
                pd.0[i] = (old + i as u64 * TWO_MIB) | PRESENT | WRITABLE | HUGE;
            }
            pdpt.0[pdpt_index] = pd_frame.0 | PRESENT | WRITABLE;
        }
        let pd = child(pdpt as *mut PageTable, pdpt_index).ok_or("out of page-table frames")?;
        if pd.0[pd_index] & HUGE != 0 {
            let old = pd.0[pd_index] & 0x000f_ffff_ffe0_0000;
            let pt_frame = alloc_table().ok_or("out of page-table frames")?;
            let pt = &mut *table_ptr(pt_frame);
            for i in 0..512 {
                pt.0[i] = (old + i as u64 * PAGE_SIZE) | PRESENT | WRITABLE | NO_EXECUTE;
            }
            pd.0[pd_index] = pt_frame.0 | PRESENT | WRITABLE;
        }
        let pt = child(pd as *mut PageTable, pd_index).ok_or("out of page-table frames")?;
        pt.0[pt_index] = physical.0 | flags.0;
    }
    Ok(())
}

pub fn activate() {
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) (&raw const PML4) as u64, options(nostack, preserves_flags));
        ACTIVE = true;
    }
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
