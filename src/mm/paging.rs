//! Kernel-owned page tables. Table pages are ordinary physical frames.
use super::frame::{self, PAGE_SIZE, PhysAddr, PhysFrame, VirtAddr};

pub const KERNEL_VIRT_BASE: u64 = 0xffff_8000_0000_0000;
const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
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

fn table_ptr(frame: PhysFrame) -> *mut PageTable {
    // Table frames are allocated from usable memory and are present in the
    // identity transition map. Keeping this access identity-based also makes
    // page-table edits safe while a new direct-map branch is being installed.
    frame.0 as *mut PageTable
}
fn alloc_table() -> Option<PhysFrame> {
    // Until the final CR3 is active, table memory must be reachable through
    // the small writable bootstrap identity map.
    let frame = frame::alloc_frame_below(PhysAddr(4 * 1024 * 1024 * 1024))?;
    if !frame::try_reserve_range(PhysAddr(frame.0), PhysAddr(frame.0 + PAGE_SIZE)) {
        return None;
    }
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
    let map = frame::memory_map();
    unsafe {
        BIOS_BOOT = kernel_start != 0 || kernel_end != 0;
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
        for direct in [false, true] {
            if let Err(error) = map_range(
                kernel_start,
                kernel_end,
                direct,
                crate::boot::PhysicalMemoryKind::Reserved,
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
    // Preserve the writable low-memory transition window for firmware data
    // and late page-table allocations. The higher-half map remains sparse.
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
