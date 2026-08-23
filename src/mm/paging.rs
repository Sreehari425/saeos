//! The kernel's first page-table owner.  The boot assembler still supplies an
//! identity map, but this module installs a page table owned by the kernel and
//! aliases the low 4 GiB at the higher-half base.

use super::frame::{PAGE_SIZE, PhysAddr, PhysFrame, VirtAddr};

pub const KERNEL_VIRT_BASE: u64 = 0xffff_8000_0000_0000;
pub const MAX_DIRECT_PHYS: u64 = 128 * 1024 * 1024 * 1024;
const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const HUGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1 << 63;

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct PageTable([u64; 512]);
static mut PML4: PageTable = PageTable([0; 512]);
static mut LOW_PDPT: PageTable = PageTable([0; 512]);
static mut HIGH_PDPT: PageTable = PageTable([0; 512]);
static mut LOW_PD: [PageTable; 4] = [PageTable([0; 512]); 4];
static mut DIRECT_PD: [PageTable; 128] = [PageTable([0; 512]); 128];
static mut TABLE_POOL: [PageTable; 16] = [PageTable([0; 512]); 16];
static mut NEXT_TABLE: usize = 0;
static mut ACTIVE: bool = false;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags(pub u64);
impl PageFlags {
    pub const KERNEL_TEXT: Self = Self(PRESENT);
    pub const READ_ONLY: Self = Self(PRESENT | NO_EXECUTE);
    pub const WRITABLE: Self = Self(PRESENT | WRITABLE | NO_EXECUTE);
    pub const MMIO: Self = Self(PRESENT | WRITABLE | NO_EXECUTE);
}

pub fn init() {
    unsafe {
        PML4.0 = [0; 512];
        LOW_PDPT.0 = [0; 512];
        HIGH_PDPT.0 = [0; 512];
        NEXT_TABLE = 0;
        let tables = (&raw mut TABLE_POOL) as *mut PageTable;
        for i in 0..16 {
            (*tables.add(i)).0 = [0; 512];
        }
        let low = (&raw mut LOW_PD) as *mut PageTable;
        let direct = (&raw mut DIRECT_PD) as *mut PageTable;
        for i in 0..4 {
            (*low.add(i)).0 = [0; 512];
        }
        for i in 0..128 {
            (*direct.add(i)).0 = [0; 512];
        }
        // Keep identity mappings through the expanded physical window during
        // transition. UEFI is allowed to load the image itself above 4 GiB.
        PML4.0[0] = (&raw const HIGH_PDPT) as u64 | PRESENT | WRITABLE;
        PML4.0[256] = (&raw const HIGH_PDPT) as u64 | PRESENT | WRITABLE;
        for i in 0..4 {
            let pd = &mut *low.add(i);
            LOW_PDPT.0[i] = (pd as *mut PageTable as u64) | PRESENT | WRITABLE;
            for j in 0..512 {
                let physical = ((i as u64) << 30) + (j as u64 * 2 * 1024 * 1024);
                // The bootstrap identity/direct map must remain executable:
                // the CPU continues fetching the current transition code from
                // this map immediately after CR3 is loaded. Fine-grained NX
                // permissions are applied when individual mappings are split.
                pd.0[j] = physical | PRESENT | WRITABLE | HUGE;
            }
        }
        for i in 0..128 {
            let pd = &mut *direct.add(i);
            HIGH_PDPT.0[i] = (pd as *mut PageTable as u64) | PRESENT | WRITABLE;
            for j in 0..512 {
                let physical = ((i as u64) << 30) + (j as u64 * 2 * 1024 * 1024);
                pd.0[j] = physical | PRESENT | WRITABLE | HUGE;
            }
        }
    }
}

#[inline]
pub fn higher_half(physical: PhysAddr) -> VirtAddr {
    VirtAddr(KERNEL_VIRT_BASE + physical.0)
}
#[inline]
pub fn phys_to_virt(physical: PhysAddr) -> VirtAddr {
    higher_half(physical)
}
#[inline]
pub fn virt_to_phys(virtual_address: VirtAddr) -> Option<PhysAddr> {
    let value = virtual_address.0;
    if (KERNEL_VIRT_BASE..KERNEL_VIRT_BASE + MAX_DIRECT_PHYS).contains(&value) {
        Some(PhysAddr(value - KERNEL_VIRT_BASE))
    } else if value < MAX_DIRECT_PHYS {
        Some(PhysAddr(value))
    } else {
        None
    }
}
#[inline]
pub fn identity(physical: PhysAddr) -> VirtAddr {
    VirtAddr(physical.0)
}

pub fn map_page(
    virtual_address: VirtAddr,
    physical: PhysAddr,
    flags: PageFlags,
) -> Result<(), &'static str> {
    let v = virtual_address.0;
    let p = physical.0;
    if (4 * 1024 * 1024 * 1024..KERNEL_VIRT_BASE).contains(&v) {
        return Err("virtual address outside bootstrap map");
    }
    if p & (PAGE_SIZE - 1) != 0 || v & (PAGE_SIZE - 1) != 0 {
        return Err("unaligned page mapping");
    }
    let normalized = if v >= KERNEL_VIRT_BASE {
        v - KERNEL_VIRT_BASE
    } else {
        v
    };
    if normalized >= MAX_DIRECT_PHYS {
        return Err("virtual address outside bootstrap map");
    }
    let pdpt_index = (normalized >> 30) as usize;
    let pd_index = ((normalized >> 21) & 0x1ff) as usize;
    let pt_index = ((normalized >> 12) & 0x1ff) as usize;
    unsafe {
        let pd_base = (&raw mut DIRECT_PD) as *mut PageTable;
        let pd = &mut *pd_base.add(pdpt_index);
        let entry = pd.0[pd_index];
        if entry & HUGE != 0 {
            let pool = (&raw mut TABLE_POOL) as *mut PageTable;
            if NEXT_TABLE >= 16 {
                return Err("page-table pool exhausted");
            }
            let pt = &mut *pool.add(NEXT_TABLE);
            NEXT_TABLE += 1;
            let base = entry & 0x000f_ffff_ffe0_0000;
            for i in 0..512 {
                pt.0[i] = (base + (i as u64 * PAGE_SIZE)) | PRESENT | WRITABLE | NO_EXECUTE;
            }
            pd.0[pd_index] = (pt as *mut PageTable as u64) | PRESENT | WRITABLE;
        }
        let pt = (pd.0[pd_index] & 0x000f_ffff_ffff_f000) as *mut PageTable;
        (*pt).0[pt_index] = p | flags.0;
    }
    Ok(())
}

pub fn activate() {
    unsafe {
        let cr3 = (&raw const PML4) as u64;
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags));
        ACTIVE = true;
    }
}
pub fn is_active() -> bool {
    unsafe { ACTIVE }
}
pub fn page_table_frame() -> PhysFrame {
    PhysFrame((&raw const PML4) as u64)
}

pub fn reserve_page_tables() {
    let pml4 = (&raw const PML4) as u64;
    let low_pdpt = (&raw const LOW_PDPT) as u64;
    let high_pdpt = (&raw const HIGH_PDPT) as u64;
    super::frame::reserve_range(PhysAddr(pml4), PhysAddr(pml4 + PAGE_SIZE));
    super::frame::reserve_range(PhysAddr(low_pdpt), PhysAddr(low_pdpt + PAGE_SIZE));
    super::frame::reserve_range(PhysAddr(high_pdpt), PhysAddr(high_pdpt + PAGE_SIZE));
    unsafe {
        let low = (&raw const LOW_PD) as *const PageTable;
        let direct = (&raw const DIRECT_PD) as *const PageTable;
        let pool = (&raw const TABLE_POOL) as *const PageTable;
        for i in 0..4 {
            let a = low.add(i) as u64;
            super::frame::reserve_range(PhysAddr(a), PhysAddr(a + PAGE_SIZE));
        }
        for i in 0..128 {
            let a = direct.add(i) as u64;
            super::frame::reserve_range(PhysAddr(a), PhysAddr(a + PAGE_SIZE));
        }
        for i in 0..16 {
            let a = pool.add(i) as u64;
            super::frame::reserve_range(PhysAddr(a), PhysAddr(a + PAGE_SIZE));
        }
    }
}
