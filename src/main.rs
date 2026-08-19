#![no_std]
#![no_main]

pub mod serial;
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

    vga_buffer::set_color(Color::LightGreen, Color::Black);
    println!("[OK] 64-bit Long Mode initialized.");
    println!("[OK] VGA Text Buffer driver active (80x25).");
    println!("[OK] Spinlock-synchronized formatted printing.");

    vga_buffer::set_color(Color::Yellow, Color::Black);
    for i in 1..=5 {
        println!("  -> Testing formatted line #{}: val={:#x}", i, i * 0x1000);
    }

    vga_buffer::set_color(Color::White, Color::Black);
    println!("\nSystem ready and spinning.");

    // Mirror to serial port COM1 for terminal logs
    serial_println!("SaeOS initialized successfully.");

    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga_buffer::set_color(Color::LightRed, Color::Black);
    println!("\n[KERNEL PANIC]");
    println!("{}", info);
    serial_println!("[KERNEL PANIC] {}", info);

    loop {
        core::hint::spin_loop();
    }
}
