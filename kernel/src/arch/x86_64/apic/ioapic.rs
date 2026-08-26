use core::ptr::{read_volatile, write_volatile};

const IOREGSEL: usize = 0x00;
const IOWIN: usize = 0x10;

// IOAPIC Register Indices
const IOAPIC_ID: u32 = 0x00;
const IOAPIC_VERSION: u32 = 0x01;
const IOAPIC_REDTBL_BASE: u32 = 0x10;

// Redirection Flags
const REDTBL_MASKED: u32 = 1 << 16;

pub struct IoApic {
    physical_base_addr: u64,
    mapped_base_addr: u64,
}

impl IoApic {
    pub const fn new() -> Self {
        Self {
            physical_base_addr: 0,
            mapped_base_addr: 0,
        }
    }

    #[inline]
    unsafe fn read_reg(&self, reg: u32) -> u32 {
        unsafe {
            let regsel = (self.mapped_base_addr + IOREGSEL as u64) as *mut u32;
            let win = (self.mapped_base_addr + IOWIN as u64) as *const u32;
            write_volatile(regsel, reg);
            read_volatile(win)
        }
    }

    #[inline]
    unsafe fn write_reg(&self, reg: u32, val: u32) {
        unsafe {
            let regsel = (self.mapped_base_addr + IOREGSEL as u64) as *mut u32;
            let win = (self.mapped_base_addr + IOWIN as u64) as *mut u32;
            write_volatile(regsel, reg);
            write_volatile(win, val);
        }
    }

    pub fn id(&self) -> u32 {
        unsafe { (self.read_reg(IOAPIC_ID) >> 24) & 0xFF }
    }

    pub fn version(&self) -> u32 {
        unsafe { self.read_reg(IOAPIC_VERSION) & 0xFF }
    }

    pub fn max_redirection_entries(&self) -> u32 {
        unsafe { ((self.read_reg(IOAPIC_VERSION) >> 16) & 0xFF) + 1 }
    }

    pub fn base_address(&self) -> u64 {
        self.physical_base_addr
    }

    pub fn set_redirection(&mut self, irq: u8, vector: u8, dest_apic_id: u8, masked: bool) {
        self.set_redirection_with_flags(irq, vector, dest_apic_id, 0, masked);
    }

    fn set_redirection_with_flags(
        &mut self,
        irq: u8,
        vector: u8,
        dest_apic_id: u8,
        flags: u16,
        masked: bool,
    ) {
        let reg_low = IOAPIC_REDTBL_BASE + (irq as u32) * 2;
        let reg_high = reg_low + 1;

        let mut low: u32 = vector as u32;
        // MADT flags: bits 0..1 polarity, bits 2..3 trigger mode.
        if flags & 0x3 == 0x3 {
            low |= 1 << 13; // active low
        }
        if flags & 0xC == 0xC {
            low |= 1 << 15; // level triggered
        }
        if masked {
            low |= REDTBL_MASKED;
        }

        let high: u32 = (dest_apic_id as u32) << 24;

        unsafe {
            self.write_reg(reg_high, high);
            self.write_reg(reg_low, low);
        }
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

    pub fn init(
        &mut self,
        dest_apic_id: u8,
        gsi_base: u32,
        timer_gsi: u32,
        timer_flags: u16,
        keyboard_gsi: u32,
        keyboard_flags: u16,
    ) -> Result<(), &'static str> {
        let max_entries = self.max_redirection_entries();
        if max_entries == 0 || max_entries > 240 {
            return Err("Invalid IOAPIC max redirection entries detected");
        }

        let timer_index = timer_gsi.checked_sub(gsi_base).ok_or("Invalid timer GSI")?;
        let keyboard_index = keyboard_gsi
            .checked_sub(gsi_base)
            .ok_or("Invalid keyboard GSI")?;
        if timer_index >= max_entries || keyboard_index >= max_entries {
            return Err("MADT GSI is outside the selected IOAPIC range");
        }

        // 1. Mask all redirection entries initially
        for irq in 0..max_entries as u8 {
            self.set_redirection(irq, 0xFF, dest_apic_id, true);
        }

        // 2. Unmask and route IRQ 0 (Timer) to Vector 0x20 (32)
        self.set_redirection_with_flags(timer_index as u8, 0x20, dest_apic_id, timer_flags, false);

        // 3. Unmask and route IRQ 1 (Keyboard) to Vector 0x21 (33)
        self.set_redirection_with_flags(
            keyboard_index as u8,
            0x21,
            dest_apic_id,
            keyboard_flags,
            false,
        );

        Ok(())
    }

    pub fn init_single(
        &mut self,
        dest_apic_id: u8,
        gsi_base: u32,
        gsi: u32,
        vector: u8,
        flags: u16,
    ) -> Result<(), &'static str> {
        let max_entries = self.max_redirection_entries();
        if max_entries == 0 || max_entries > 240 {
            return Err("Invalid IOAPIC max redirection entries detected");
        }
        let index = gsi.checked_sub(gsi_base).ok_or("Invalid GSI")?;
        if index >= max_entries {
            return Err("MADT GSI is outside the selected IOAPIC range");
        }
        for irq in 0..max_entries as u8 {
            self.set_redirection(irq, 0xFF, dest_apic_id, true);
        }
        self.set_redirection_with_flags(index as u8, vector, dest_apic_id, flags, false);
        Ok(())
    }
}

impl Default for IoApic {
    fn default() -> Self {
        Self::new()
    }
}
