#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

pub mod arch;
pub mod collections;
pub mod drivers;
pub mod mm;
pub mod shell;
pub mod sync;

use arch::x86_64::cpu;
use arch::x86_64::interrupt_controller::ControllerKind;
use core::panic::PanicInfo;
use drivers::vga::{self, Color};

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(multiboot_info_addr: usize) -> ! {
    vga::clear_screen();

    vga::set_color(Color::LightCyan, Color::Black);
    println!("========================================");
    println!("       Welcome to SaeOS (x86_64)!       ");
    println!("========================================");

    // 1. Initialize Architecture (GDT/TSS with IST, IDT, and Interrupt Controller)
    let mode = arch::x86_64::init();
    vga::set_color(Color::LightGreen, Color::Black);
    println!("[OK] GDT & TSS with IST loaded.");
    println!("[OK] IDT loaded (256 vectors).");

    match mode {
        ControllerKind::Apic => {
            vga::set_color(Color::LightGreen, Color::Black);
            println!("[OK] APIC active (LAPIC @ 0xFEE00000, IOAPIC @ 0xFEC00000).");
            println!("[OK] 8259 Legacy PIC masked and disabled.");
            serial_println!("SaeOS initialized with APIC (LAPIC+IOAPIC).");
        }
        ControllerKind::LegacyPic => {
            vga::set_color(Color::Yellow, Color::Black);
            println!("[WARN] APIC unavailable. Falling back to 8259 Legacy PIC.");
            serial_println!("SaeOS initialized with 8259 Legacy PIC fallback.");
        }
    }

    // 2. Initialize Memory Management Subsystem & Kernel Heap Allocator
    mm::init(multiboot_info_addr);
    vga::set_color(Color::LightGreen, Color::Black);
    println!("[OK] Kernel Heap Allocator initialized (10 MiB at 0x400000).");
    serial_println!("Kernel Heap Allocator initialized (10 MiB).");

    // 3. Enable CPU Hardware Interrupts
    cpu::sti();
    println!("[OK] CPU Interrupts enabled (sti).\n");

    // 4. Start interactive Shell
    shell::run();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    vga::set_color(Color::LightRed, Color::Black);
    println!("\n[KERNEL PANIC]");
    println!("{}", info);
    serial_println!("[KERNEL PANIC] {}", info);

    loop {
        cpu::hlt();
    }
}
