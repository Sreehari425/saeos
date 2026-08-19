#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    let msg = b"Hello World from 64-bit Rust (x86_64)!";

    // 1. Clear the entire VGA text screen (80 cols * 25 rows = 2000 cells = 4000 bytes)
    let vga_buffer = 0xb8000 as *mut u8;
    for i in 0..(80 * 25) {
        unsafe {
            *vga_buffer.add(i * 2) = b' ';
            *vga_buffer.add(i * 2 + 1) = 0x07; // Light gray on black
        }
    }

    // 2. Write message to VGA text buffer
    for (i, &byte) in msg.iter().enumerate() {
        unsafe {
            *vga_buffer.add(i * 2) = byte;
            *vga_buffer.add(i * 2 + 1) = 0x0f; // Bright white on black
        }
    }

    // 2. Write to Serial Port COM1 (0x3F8) for terminal output in QEMU
    for &byte in msg.iter() {
        unsafe {
            core::arch::asm!("out dx, al", in("dx") 0x3f8u16, in("al") byte, options(nomem, nostack));
        }
    }
    unsafe {
        core::arch::asm!("out dx, al", in("dx") 0x3f8u16, in("al") b'\n', options(nomem, nostack));
    }

    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
