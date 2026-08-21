pub const PIC_1_OFFSET: u8 = 32; // 0x20
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8; // 0x28

const CMD_PIC1: u16 = 0x20;
const DATA_PIC1: u16 = 0x21;
const CMD_PIC2: u16 = 0xA0;
const DATA_PIC2: u16 = 0xA1;

const PIC_EOI: u8 = 0x20;
const ICW1_INIT: u8 = 0x11;
const ICW4_8086: u8 = 0x01;

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

#[inline]
unsafe fn io_wait() {
    unsafe {
        outb(0x80, 0);
    }
}

pub fn init() {
    unsafe {
        // Save current interrupt masks
        let mask1 = inb(DATA_PIC1);
        let mask2 = inb(DATA_PIC2);

        // Start initialization sequence (in cascade mode)
        outb(CMD_PIC1, ICW1_INIT);
        io_wait();
        outb(CMD_PIC2, ICW1_INIT);
        io_wait();

        // ICW2: Set Master and Slave vector offsets
        outb(DATA_PIC1, PIC_1_OFFSET);
        io_wait();
        outb(DATA_PIC2, PIC_2_OFFSET);
        io_wait();

        // ICW3: Tell Master PIC there is a Slave PIC at IRQ2 (0000 0100)
        outb(DATA_PIC1, 4);
        io_wait();
        // Tell Slave PIC its cascade identity (0000 0010)
        outb(DATA_PIC2, 2);
        io_wait();

        // ICW4: Set 8086 mode
        outb(DATA_PIC1, ICW4_8086);
        io_wait();
        outb(DATA_PIC2, ICW4_8086);
        io_wait();

        // Unmask IRQ 0 (Timer) and IRQ 1 (Keyboard): 0b1111_1100 = 0xFC
        // Keep slave masked: 0xFF
        outb(DATA_PIC1, 0xFC);
        outb(DATA_PIC2, 0xFF);
        let _ = (mask1, mask2);
    }
}

pub fn notify_end_of_interrupt(interrupt_id: u8) {
    unsafe {
        if interrupt_id >= PIC_2_OFFSET {
            outb(CMD_PIC2, PIC_EOI);
        }
        outb(CMD_PIC1, PIC_EOI);
    }
}
