#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

pub mod arch;
pub mod boot;
pub mod collections;
pub mod drivers;
pub mod mm;
pub mod shell;
pub mod sync;

use arch::x86_64::cpu;
use arch::x86_64::interrupt_controller::ControllerKind;
use boot::boot_info::{BootInfo, DisplayMode};
use core::panic::PanicInfo;
use drivers::vga::Color;

/// BIOS entrypoint called from `boot.asm` with the Multiboot 1 info pointer.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main_bios(multiboot_info_addr: usize) -> ! {
    let mut total_memory_mb = 128;
    if multiboot_info_addr != 0 {
        let mb_info = unsafe { &*(multiboot_info_addr as *const mm::MultibootInfo) };
        if mb_info.has_mem_info() {
            total_memory_mb = mb_info.total_memory_mb();
        }
    }

    let boot_info = BootInfo {
        display: DisplayMode::VgaText {
            buffer_addr: 0xb8000,
        },
        total_memory_mb,
        rsdp_addr: None,
    };

    kernel_main(&boot_info);
}

/// Unified kernel entrypoint shared by both BIOS and UEFI.
pub fn kernel_main(boot_info: &BootInfo) -> ! {
    // Both BIOS and UEFI mirror console output to COM1. UEFI firmware may
    // have initialized the UART already, but initializing it here keeps the
    // BIOS path deterministic as well.
    drivers::serial::init();

    // 1. Initialize Adaptive Console (VGA Text Buffer or GOP Truecolor Framebuffer)
    drivers::console::init(boot_info.display);
    drivers::console::clear_screen();

    drivers::console::set_color(Color::LightCyan, Color::Black);
    println!("========================================");
    println!("       Welcome to SaeOS (x86_64)!       ");
    println!("========================================");

    match boot_info.display {
        DisplayMode::VgaText { buffer_addr } => {
            drivers::console::set_color(Color::LightCyan, Color::Black);
            println!("[OK] Boot Mode: Legacy BIOS (VGA Text 80x25 @ {:#x}).", buffer_addr);
        }
        DisplayMode::GopFramebuffer(info) => {
            drivers::console::set_color(Color::LightCyan, Color::Black);
            println!(
                "[OK] Boot Mode: Modern 64-bit UEFI (GOP Truecolor {}x{} @ {:#x}).",
                info.width, info.height, info.base_addr
            );
        }
    }

    // 2. Initialize Architecture (GDT/TSS with IST, IDT, and Interrupt Controller)
    let mode = arch::x86_64::init();
    drivers::console::set_color(Color::LightGreen, Color::Black);
    println!("[OK] GDT & TSS with IST loaded.");
    println!("[OK] IDT loaded (256 vectors).");

    match mode {
        ControllerKind::Apic => {
            drivers::console::set_color(Color::LightGreen, Color::Black);
            println!("[OK] APIC active (LAPIC @ 0xFEE00000, IOAPIC @ 0xFEC00000).");
            println!("[OK] 8259 Legacy PIC masked and disabled.");
        }
        ControllerKind::LegacyPic => {
            drivers::console::set_color(Color::Yellow, Color::Black);
            println!("[WARN] APIC unavailable. Falling back to 8259 Legacy PIC.");
        }
    }

    // 3. Initialize Memory Management & Heap Allocator (10 MiB)
    mm::init(0);
    drivers::console::set_color(Color::LightGreen, Color::Black);
    println!(
        "[OK] Kernel Heap Allocator initialized (10 MiB at {:#x}).",
        mm::heap_start()
    );

    // 4. Initialize Keyboard Controller & Enable CPU Hardware Interrupts
    drivers::keyboard::init();
    cpu::sti();
    println!("[OK] CPU Interrupts enabled (sti).\n");

    // 5. Start interactive Shell
    shell::run();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    drivers::console::set_color(Color::LightRed, Color::Black);
    println!("\n[KERNEL PANIC]");
    println!("{}", info);

    loop {
        cpu::hlt();
    }
}
