use core::ptr::{read_volatile, write_volatile};

pub const DEFAULT_IOAPIC_BASE: u64 = 0xFEC0_0000;

const IOREGSEL: usize = 0x00;
const IOWIN: usize = 0x10;

// IOAPIC Register Indices
const IOAPIC_ID: u32 = 0x00;
const IOAPIC_VERSION: u32 = 0x01;
const IOAPIC_REDTBL_BASE: u32 = 0x10;

// Redirection Flags
const REDTBL_MASKED: u32 = 1 << 16;

pub struct IoApic {
    base_addr: u64,
}

impl IoApic {
    pub const fn new() -> Self {
        Self {
            base_addr: DEFAULT_IOAPIC_BASE,
        }
    }

    #[inline]
    unsafe fn read_reg(&self, reg: u32) -> u32 {
        unsafe {
            let regsel = (self.base_addr + IOREGSEL as u64) as *mut u32;
            let win = (self.base_addr + IOWIN as u64) as *const u32;
            write_volatile(regsel, reg);
            read_volatile(win)
        }
    }

    #[inline]
    unsafe fn write_reg(&self, reg: u32, val: u32) {
        unsafe {
            let regsel = (self.base_addr + IOREGSEL as u64) as *mut u32;
            let win = (self.base_addr + IOWIN as u64) as *mut u32;
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
        self.base_addr
    }

    pub fn set_redirection(&mut self, irq: u8, vector: u8, dest_apic_id: u8, masked: bool) {
        let reg_low = IOAPIC_REDTBL_BASE + (irq as u32) * 2;
        let reg_high = reg_low + 1;

        let mut low: u32 = vector as u32;
        if masked {
            low |= REDTBL_MASKED;
        }

        let high: u32 = (dest_apic_id as u32) << 24;

        unsafe {
            // Write high 32 bits (destination APIC ID) first
            self.write_reg(reg_high, high);
            // Write low 32 bits (vector, delivery mode, mask)
            self.write_reg(reg_low, low);
        }
    }

    pub fn init(&mut self, dest_apic_id: u8) -> Result<(), &'static str> {
        let max_entries = self.max_redirection_entries();
        if max_entries == 0 || max_entries > 240 {
            return Err("Invalid IOAPIC max redirection entries detected");
        }

        // 1. Mask all redirection entries initially
        for irq in 0..max_entries as u8 {
            self.set_redirection(irq, 0x20 + irq, dest_apic_id, true);
        }

        // 2. Unmask and route IRQ 0 (Timer) to Vector 0x20 (32)
        self.set_redirection(0, 0x20, dest_apic_id, false);

        // 3. Unmask and route IRQ 1 (Keyboard) to Vector 0x21 (33)
        self.set_redirection(1, 0x21, dest_apic_id, false);

        Ok(())
    }
}

impl Default for IoApic {
    fn default() -> Self {
        Self::new()
    }
}
