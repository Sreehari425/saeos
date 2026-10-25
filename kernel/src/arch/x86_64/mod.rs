pub mod acpi;
pub mod apic;
pub mod context;
pub mod cpu;
pub mod fpu;
pub mod gdt;
pub mod idt;
pub mod interrupt_controller;
pub mod irq;
pub mod pic;
pub mod qemu;
pub mod syscall;
pub mod uefi;
pub mod vectors;

pub use interrupt_controller::ControllerKind;

/// Initializes the x86_64 architectural subsystems: GDT/TSS, IDT, and Interrupt Controller.
pub fn init(topology: Option<&acpi::InterruptTopology>) -> ControllerKind {
    // 1. Initialize 64-bit GDT & TSS (with IST 0 for Double Fault stack)
    gdt::init();
    // x87/SSE on, with the clean state tasks start from.
    fpu::init();
    // SMEP/SMAP when the CPU has them.
    crate::mm::uaccess::init_protections();
    // syscall/sysret.
    syscall::init();

    // 2. Initialize 256-entry IDT
    idt::init();

    // 3. Initialize Interrupt Controller (APIC with PIC Fallback)
    interrupt_controller::init(topology)
}
