use crate::arch::x86_64::apic::lapic::SPURIOUS_INTERRUPT_VECTOR;
use crate::arch::x86_64::cpu;
use crate::arch::x86_64::gdt;
use crate::arch::x86_64::interrupt_controller;
use crate::arch::x86_64::pic;
#[cfg(feature = "keyboard")]
use crate::drivers::keyboard;
use crate::println;
use crate::serial_println;

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct InterruptStackFrame {
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct IdtEntry {
    pointer_low: u16,
    gdt_selector: u16,
    options: u16,
    pointer_middle: u16,
    pointer_high: u32,
    reserved: u32,
}

impl IdtEntry {
    pub const fn missing() -> Self {
        Self {
            pointer_low: 0,
            gdt_selector: 0,
            options: 0,
            pointer_middle: 0,
            pointer_high: 0,
            reserved: 0,
        }
    }

    pub fn set_handler_addr(&mut self, addr: u64) -> &mut Self {
        self.pointer_low = addr as u16;
        self.pointer_middle = (addr >> 16) as u16;
        self.pointer_high = (addr >> 32) as u32;
        self.gdt_selector = 0x08; // Kernel Code Segment
        self.options = 0x8E00; // Present, 64-bit Interrupt Gate
        self.reserved = 0;
        self
    }

    pub fn set_stack_index(&mut self, index: u16) -> &mut Self {
        self.options = (self.options & 0xFFF8) | ((index + 1) & 0x07);
        self
    }
}

#[repr(C, packed)]
pub struct InterruptDescriptorTable {
    pub entries: [IdtEntry; 256],
}

impl InterruptDescriptorTable {
    pub const fn new() -> Self {
        Self {
            entries: [IdtEntry::missing(); 256],
        }
    }
}

impl Default for InterruptDescriptorTable {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(C, packed)]
struct IdtDescriptor {
    limit: u16,
    base: u64,
}

static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();

pub fn init() {
    unsafe {
        // 1. CPU Exceptions
        IDT.entries[0].set_handler_addr(divide_error_handler as *const () as u64);
        IDT.entries[3].set_handler_addr(breakpoint_handler as *const () as u64);
        IDT.entries[8]
            .set_handler_addr(double_fault_handler as *const () as u64)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        IDT.entries[13].set_handler_addr(general_protection_fault_handler as *const () as u64);
        IDT.entries[14].set_handler_addr(page_fault_handler as *const () as u64);

        // 2. Hardware Interrupts (Vectors 0x20 = 32, 0x21 = 33)
        IDT.entries[pic::PIC_1_OFFSET as usize]
            .set_handler_addr(timer_interrupt_handler as *const () as u64);
        #[cfg(feature = "keyboard")]
        IDT.entries[(pic::PIC_1_OFFSET + 1) as usize]
            .set_handler_addr(keyboard_interrupt_handler as *const () as u64);

        // 3. APIC Spurious Interrupt Handler (Vector 0xFF = 255)
        IDT.entries[SPURIOUS_INTERRUPT_VECTOR as usize]
            .set_handler_addr(spurious_interrupt_handler as *const () as u64);

        let idt_descriptor = IdtDescriptor {
            limit: (core::mem::size_of::<InterruptDescriptorTable>() - 1) as u16,
            base: (&raw const IDT) as u64,
        };

        core::arch::asm!(
            "lidt [{ptr}]",
            ptr = in(reg) &idt_descriptor,
            options(readonly, nostack, preserves_flags)
        );
    }
}

// Exception Handlers
extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    println!("\n[EXCEPTION: DIVIDE BY ZERO]\n{:#?}", stack_frame);
    serial_println!(
        "[EXCEPTION: DIVIDE BY ZERO] RIP={:#x}",
        stack_frame.instruction_pointer
    );
    loop {
        cpu::pause();
    }
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("\n[EXCEPTION: BREAKPOINT]\n{:#?}", stack_frame);
    serial_println!(
        "[EXCEPTION: BREAKPOINT] RIP={:#x}",
        stack_frame.instruction_pointer
    );
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    println!(
        "\n[EXCEPTION: DOUBLE FAULT (err={:#x})]\n{:#?}",
        error_code, stack_frame
    );
    serial_println!(
        "[EXCEPTION: DOUBLE FAULT (err={:#x})] RIP={:#x}",
        error_code,
        stack_frame.instruction_pointer
    );
    loop {
        cpu::pause();
    }
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    println!(
        "\n[EXCEPTION: GENERAL PROTECTION FAULT (err={:#x})]\n{:#?}",
        error_code, stack_frame
    );
    serial_println!(
        "[EXCEPTION: GPF (err={:#x})] RIP={:#x}",
        error_code,
        stack_frame.instruction_pointer
    );
    loop {
        cpu::pause();
    }
}

extern "x86-interrupt" fn page_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    let faulting_address: u64;
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) faulting_address, options(nomem, nostack, preserves_flags));
    }
    println!(
        "\n[EXCEPTION: PAGE FAULT at {:#x} (err={:#x})]\n{:#?}",
        faulting_address, error_code, stack_frame
    );
    serial_println!(
        "[EXCEPTION: PAGE FAULT at {:#x} (err={:#x})] RIP={:#x}",
        faulting_address,
        error_code,
        stack_frame.instruction_pointer
    );
    loop {
        cpu::pause();
    }
}

// Hardware Interrupt Handlers
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::time::on_timer_interrupt();
    interrupt_controller::notify_end_of_interrupt(pic::PIC_1_OFFSET);
}

#[cfg(feature = "keyboard")]
extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let scancode = unsafe { cpu::inb(0x60) };

    keyboard::handle_scancode(scancode);

    interrupt_controller::notify_end_of_interrupt(pic::PIC_1_OFFSET + 1);
}

// APIC Spurious Interrupt Handler (Intel specification states no EOI should be generated)
extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {}
