#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

pub mod gdt;
pub mod interrupts;
pub mod keyboard;
pub mod pic;
pub mod serial;
pub mod shell;
pub mod sync;
pub mod vga_buffer;

use core::panic::PanicInfo;
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

    // 2. Initialize IDT (Exception & Hardware IRQ handlers)
    interrupts::init_idt();
    println!("[OK] IDT loaded (Exceptions + Hardware IRQs).");

    // 3. Initialize & Remap 8259 PIC
    pic::init();
    println!("[OK] 8259 PIC remapped (IRQs 0x20..=0x2F).");

    // 4. Enable CPU Hardware Interrupts
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack, preserves_flags));
    }
    println!("[OK] CPU Hardware Interrupts enabled (sti).\n");

    serial_println!("SaeOS initialized with IDT and Keyboard. Starting shell.");

    // 5. Start interactive Echo Shell
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
