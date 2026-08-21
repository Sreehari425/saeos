#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

pub mod apic;
pub mod gdt;
pub mod interrupts;
pub mod interrupt_controller;
pub mod keyboard;
pub mod pic;
pub mod serial;
pub mod shell;
pub mod sync;
pub mod vga_buffer;

use core::panic::PanicInfo;
use interrupt_controller::ControllerKind;
use vga_buffer::Color;

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    vga_buffer::clear_screen();

    vga_buffer::set_color(Color::LightCyan, Color::Black);
    println!("========================================");
    println!("       Welcome to SaeOS (x86_64)!       ");
    println!("========================================");

    // 1. Initialize GDT & TSS (with IST for Double Fault)
    gdt::init();
    vga_buffer::set_color(Color::LightGreen, Color::Black);
    println!("[OK] GDT & TSS with IST loaded.");

    // 2. Initialize IDT (Exceptions + Hardware IRQs + Spurious)
    interrupts::init_idt();
    println!("[OK] IDT loaded (256 vectors).");

    // 3. Initialize Interrupt Controller (APIC with 8259 PIC Fallback)
    let mode = interrupt_controller::init();
    match mode {
        ControllerKind::Apic => {
            vga_buffer::set_color(Color::LightGreen, Color::Black);
            println!("[OK] APIC active (LAPIC @ 0xFEE00000, IOAPIC @ 0xFEC00000).");
            println!("[OK] 8259 Legacy PIC masked and disabled.");
            serial_println!("SaeOS initialized with APIC (LAPIC+IOAPIC).");
        }
        ControllerKind::LegacyPic => {
            vga_buffer::set_color(Color::Yellow, Color::Black);
            println!("[WARN] APIC unavailable. Falling back to 8259 Legacy PIC.");
            serial_println!("SaeOS initialized with 8259 Legacy PIC fallback.");
        }
    }

    // 4. Enable CPU Hardware Interrupts
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }
    vga_buffer::set_color(Color::LightGreen, Color::Black);
    println!("[OK] CPU Interrupts enabled (sti).\n");

    // 5. Start interactive Shell
    shell::run();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga_buffer::set_color(Color::LightRed, Color::Black);
    println!("\n[KERNEL PANIC]");
    println!("{}", info);
    serial_println!("[KERNEL PANIC] {}", info);

    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}
