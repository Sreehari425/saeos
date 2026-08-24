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
const MAX_PHYS: u64 = 512 * 1024 * 1024 * 1024;

#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct PageTable([u64; 512]);
static mut PML4: PageTable = PageTable([0; 512]);
// UEFI may leave ordinary usable pages inaccessible or read-only in its own
// page tables. These few image-backed pages provide a writable identity map
// while allocator-backed table frames are being initialized.
static mut BOOTSTRAP_PML4: PageTable = PageTable([0; 512]);
static mut BOOTSTRAP_PDPT: PageTable = PageTable([0; 512]);
static mut BOOTSTRAP_PD: [PageTable; 4] = [PageTable([0; 512]); 4];
static mut ACTIVE: bool = false;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags(pub u64);
impl PageFlags {
    pub const KERNEL_TEXT: Self = Self(PRESENT);
    pub const READ_ONLY: Self = Self(PRESENT | NO_EXECUTE);
    pub const WRITABLE: Self = Self(PRESENT | WRITABLE | NO_EXECUTE);
    pub const MMIO: Self = Self(PRESENT | WRITABLE | NO_EXECUTE);
}

fn table_ptr(frame: PhysFrame) -> *mut PageTable {
    let address = if is_active() {
        phys_to_virt(PhysAddr(frame.0)).0
    } else {
        frame.0
    };
    address as *mut PageTable
}
fn alloc_table() -> Option<PhysFrame> {
    let frame = frame::alloc_frame_at_or_above(PhysAddr(PAGE_SIZE))?;
    frame::reserve_range(PhysAddr(frame.0), PhysAddr(frame.0 + PAGE_SIZE));
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
        for index in 0..4 {
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

pub fn init() {
    let map = frame::memory_map();
    let highest = map.regions[..map.count]
        .iter()
        .map(|r| r.end())
        .max()
        .unwrap_or(4 * 1024 * 1024 * 1024)
        .clamp(4 * 1024 * 1024 * 1024, MAX_PHYS);
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
        for index in 0..4 {
            let address = bootstrap_pd.add(index) as u64;
            frame::reserve_range(PhysAddr(address), PhysAddr(address + PAGE_SIZE));
        }
    }
    frame::reserve_range(
        PhysAddr((&raw const PML4) as u64),
        PhysAddr((&raw const PML4) as u64 + PAGE_SIZE),
    );
    for root in [0usize, 256usize] {
        let pdpt = child(&raw mut PML4, root);
        let Some(pdpt) = pdpt else { return };
        for gig in 0..highest.div_ceil(GIGABYTE) as usize {
            let physical = gig as u64 * GIGABYTE;
            if physical + GIGABYTE <= highest {
                pdpt.0[gig] = physical | PRESENT | WRITABLE | HUGE;
            } else {
                let Some(pd) = child(pdpt as *mut PageTable, gig) else {
                    return;
                };
                for two_mb in 0..512 {
                    let physical = physical + two_mb as u64 * TWO_MIB;
                    if physical >= highest {
                        break;
                    }
                    pd.0[two_mb] = physical | PRESENT | WRITABLE | HUGE;
                }
            }
        }
    }
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
    if (KERNEL_VIRT_BASE..KERNEL_VIRT_BASE + MAX_PHYS).contains(&v.0) {
        Some(PhysAddr(v.0 - KERNEL_VIRT_BASE))
    } else if v.0 < MAX_PHYS {
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
    if normalized >= MAX_PHYS {
        return Err("address outside direct map");
    }
    let pml4_index = ((normalized >> 39) & 0x1ff) as usize;
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
pub fn page_table_frame() -> PhysFrame {
    PhysFrame((&raw const PML4) as u64)
}
pub fn reserve_page_tables() {}
