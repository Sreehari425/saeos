use crate::arch::x86_64::cpu;

pub const PIC_1_OFFSET: u8 = 32; // 0x20
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8; // 0x28

const CMD_PIC1: u16 = 0x20;
const DATA_PIC1: u16 = 0x21;
const CMD_PIC2: u16 = 0xA0;
const DATA_PIC2: u16 = 0xA1;

const PIC_EOI: u8 = 0x20;
const ICW1_INIT: u8 = 0x11;
const ICW4_8086: u8 = 0x01;

pub fn init() {
    unsafe {
        // Save current interrupt masks
        let mask1 = cpu::inb(DATA_PIC1);
        let mask2 = cpu::inb(DATA_PIC2);

        // Start initialization sequence (in cascade mode)
        cpu::outb(CMD_PIC1, ICW1_INIT);
        cpu::io_wait();
        cpu::outb(CMD_PIC2, ICW1_INIT);
        cpu::io_wait();

        // ICW2: Set Master and Slave vector offsets
        cpu::outb(DATA_PIC1, PIC_1_OFFSET);
        cpu::io_wait();
        cpu::outb(DATA_PIC2, PIC_2_OFFSET);
        cpu::io_wait();

        // ICW3: Tell Master PIC there is a Slave PIC at IRQ2 (0000 0100)
        cpu::outb(DATA_PIC1, 4);
        cpu::io_wait();
        // Tell Slave PIC its cascade identity (0000 0010)
        cpu::outb(DATA_PIC2, 2);
        cpu::io_wait();

        // ICW4: Set 8086 mode
        cpu::outb(DATA_PIC1, ICW4_8086);
        cpu::io_wait();
        cpu::outb(DATA_PIC2, ICW4_8086);
        cpu::io_wait();

        // Keep both PICs masked until the interrupt-controller selection is
        // complete. APIC mode must never have a live legacy IRQ0 source.
        cpu::outb(DATA_PIC1, 0xFF);
        cpu::outb(DATA_PIC2, 0xFF);
        let _ = (mask1, mask2);
    }
}

pub fn unmask_legacy_irqs() {
    unsafe {
        // Unmask the timer, and only unmask the keyboard when it is enabled.
        let master_mask = if cfg!(feature = "keyboard") {
            0xFC
        } else {
            0xFE
        };
        cpu::outb(DATA_PIC1, master_mask);
        cpu::outb(DATA_PIC2, 0xFF);
    }
}

pub fn mask_all() {
    unsafe {
        cpu::outb(DATA_PIC1, 0xFF);
        cpu::outb(DATA_PIC2, 0xFF);
    }
}

pub fn notify_end_of_interrupt(interrupt_id: u8) {
    unsafe {
        if interrupt_id >= PIC_2_OFFSET {
            cpu::outb(CMD_PIC2, PIC_EOI);
        }
        cpu::outb(CMD_PIC1, PIC_EOI);
    }
}
