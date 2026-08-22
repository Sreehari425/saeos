pub mod apic;
pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod interrupt_controller;
pub mod pic;
pub mod uefi;

pub use interrupt_controller::ControllerKind;

/// Initializes the x86_64 architectural subsystems: GDT/TSS, IDT, and Interrupt Controller.
pub fn init() -> ControllerKind {
    // 1. Initialize 64-bit GDT & TSS (with IST 0 for Double Fault stack)
    gdt::init();

    // 2. Initialize 256-entry IDT
    idt::init();

    // 3. Initialize Interrupt Controller (APIC with PIC Fallback)
    interrupt_controller::init()
}
