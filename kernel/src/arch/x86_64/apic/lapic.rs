use crate::arch::x86_64::cpu;
use core::ptr::{read_volatile, write_volatile};

const IA32_APIC_BASE_MSR: u32 = 0x1B;
const IA32_APIC_BASE_MSR_ENABLE: u64 = 1 << 11;
const IA32_APIC_BASE_MSR_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

// Register Offsets
const REG_ID: usize = 0x020;
const REG_VERSION: usize = 0x030;
const REG_TPR: usize = 0x080;
const REG_EOI: usize = 0x0B0;
const REG_LDR: usize = 0x0D0;
const REG_DFR: usize = 0x0E0;
const REG_SVR: usize = 0x0F0;
const REG_ESR: usize = 0x280;
const REG_LVT_TIMER: usize = 0x320;
const REG_LVT_LINT0: usize = 0x350;
const REG_LVT_LINT1: usize = 0x360;
const REG_LVT_ERROR: usize = 0x370;

// SVR Flags
const SVR_APIC_ENABLE: u32 = 1 << 8;
pub const SPURIOUS_INTERRUPT_VECTOR: u8 = 0xFF;

// LVT Flags
const LVT_MASKED: u32 = 1 << 16;

pub struct LocalApic {
    physical_base_addr: u64,
    mapped_base_addr: u64,
}

impl LocalApic {
    pub const fn new() -> Self {
        Self {
            physical_base_addr: 0,
            mapped_base_addr: 0,
        }
    }

    pub fn is_supported() -> bool {
        let cpuid_res = cpu::cpuid(1);
        (cpuid_res.edx & (1 << 9)) != 0
    }

    #[inline]
    unsafe fn read_reg(&self, offset: usize) -> u32 {
        unsafe {
            let ptr = (self.mapped_base_addr + offset as u64) as *const u32;
            read_volatile(ptr)
        }
    }

    #[inline]
    unsafe fn write_reg(&self, offset: usize, value: u32) {
        unsafe {
            let ptr = (self.mapped_base_addr + offset as u64) as *mut u32;
            write_volatile(ptr, value);
        }
    }

    pub fn init(&mut self) -> Result<(), &'static str> {
        if !Self::is_supported() {
            return Err("CPUID reports APIC is not supported");
        }

        unsafe {
            // 1. Read IA32_APIC_BASE MSR and ensure global APIC enable bit is set
            let apic_base_msr = cpu::rdmsr(IA32_APIC_BASE_MSR);
            let msr_base = apic_base_msr & IA32_APIC_BASE_MSR_ADDR_MASK;
            if self.physical_base_addr == 0 {
                return Err("APIC base address was not supplied by ACPI MADT");
            }
            if msr_base != 0 && msr_base != self.physical_base_addr {
                return Err("ACPI LAPIC base disagrees with IA32_APIC_BASE");
            }

            // Ensure APIC global enable bit (bit 11) is set
            if (apic_base_msr & IA32_APIC_BASE_MSR_ENABLE) == 0 {
                cpu::wrmsr(
                    IA32_APIC_BASE_MSR,
                    apic_base_msr | IA32_APIC_BASE_MSR_ENABLE,
                );
            }

            // 2. Set Flat Model in Destination Format Register (DFR)
            self.write_reg(REG_DFR, 0xFFFF_FFFF);

            // 3. Set Logical Destination Register (LDR) to APIC ID 1 in flat mode
            let ldr = (self.read_reg(REG_LDR) & 0x00FF_FFFF) | (1 << 24);
            self.write_reg(REG_LDR, ldr);

            // 4. Set Task Priority Register (TPR) to 0 to accept all interrupt classes
            self.write_reg(REG_TPR, 0);

            // 5. Mask LVT timer, LINT0, LINT1, and Error
            self.write_reg(REG_LVT_TIMER, LVT_MASKED);
            self.write_reg(REG_LVT_LINT0, LVT_MASKED);
            self.write_reg(REG_LVT_LINT1, LVT_MASKED);
            self.write_reg(REG_LVT_ERROR, LVT_MASKED);

            // 6. Clear Error Status Register (ESR)
            self.write_reg(REG_ESR, 0);
            self.write_reg(REG_ESR, 0);

            // 7. Software-Enable Local APIC and set spurious interrupt vector (0xFF)
            self.write_reg(
                REG_SVR,
                SVR_APIC_ENABLE | (SPURIOUS_INTERRUPT_VECTOR as u32),
            );

            // 8. Send initial EOI to clear any pending state
            self.eoi();
        }

        Ok(())
    }

    pub fn set_base_address(&mut self, address: u64) {
        self.physical_base_addr = address;
        let page = address & !0xfff;
        let _ = crate::mm::paging::map_page(
            crate::mm::paging::phys_to_virt(crate::mm::PhysAddr(page)),
            crate::mm::PhysAddr(page),
            crate::mm::paging::PageFlags::MMIO,
        );
        self.mapped_base_addr = if crate::mm::paging::prefer_identity_mmio() {
            crate::mm::paging::identity(crate::mm::PhysAddr(address)).0
        } else {
            crate::mm::paging::phys_to_virt(crate::mm::PhysAddr(address)).0
        };
    }

    pub fn eoi(&self) {
        unsafe {
            self.write_reg(REG_EOI, 0);
        }
    }

    pub fn id(&self) -> u32 {
        unsafe { (self.read_reg(REG_ID) >> 24) & 0xFF }
    }

    pub fn version(&self) -> u32 {
        unsafe { self.read_reg(REG_VERSION) & 0xFF }
    }

    pub fn base_address(&self) -> u64 {
        self.physical_base_addr
    }
}

impl Default for LocalApic {
    fn default() -> Self {
        Self::new()
    }
}
