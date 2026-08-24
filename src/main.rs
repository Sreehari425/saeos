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

use arch::x86_64::acpi;
use arch::x86_64::cpu;
use arch::x86_64::interrupt_controller::ControllerKind;
use boot::boot_info::{BootInfo, BootMode, DisplayMode};
use core::panic::PanicInfo;
use drivers::vga::Color;

#[cfg(not(target_os = "uefi"))]
unsafe extern "C" {
    static _kernel_start: u8;
    static _kernel_end: u8;
}

#[cfg(not(target_os = "uefi"))]
fn kernel_physical_bounds() -> (u64, u64) {
    (
        (&raw const _kernel_start) as u64,
        (&raw const _kernel_end) as u64,
    )
}

#[cfg(target_os = "uefi")]
const fn kernel_physical_bounds() -> (u64, u64) {
    (0, 0)
}

/// BIOS entrypoint called from `boot.asm` with the Multiboot 1 info pointer.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main_bios(multiboot_info_addr: usize) -> ! {
    let mut memory_map = boot::boot_info::PhysicalMemoryMap::empty();
    if multiboot_info_addr != 0 {
        let mb_info = unsafe { &*(multiboot_info_addr as *const mm::MultibootInfo) };
        memory_map = mb_info.usable_memory_map();
    }

    let (kernel_physical_start, kernel_physical_end) = kernel_physical_bounds();
    let boot_info = BootInfo {
        display: DisplayMode::VgaText {
            buffer_addr: 0xb8000,
        },
        boot_mode: BootMode::Bios,
        memory_map,
        kernel_physical_start,
        kernel_physical_end,
        rsdp_addr: acpi::find_rsdp_bios(),
        memory_map_descriptor_count: memory_map.count,
        memory_map_discarded: 0,
    };

    kernel_main(&boot_info);
}

/// Unified kernel entrypoint shared by both BIOS and UEFI.
pub fn kernel_main(boot_info: &BootInfo) -> ! {
    // Both BIOS and UEFI mirror console output to COM1. UEFI firmware may
    // have initialized the UART already, but initializing it here keeps the
    // BIOS path deterministic as well.
    drivers::serial::init();

    mm::init(boot_info);

    let acpi_topology = match boot_info.rsdp_addr {
        Some(address) => match unsafe { acpi::parse_rsdp(address) } {
            Ok(topology) => Some(topology),
            Err(error) => {
                serial_println!("[ACPI] MADT discovery failed: {:?}", error);
                None
            }
        },
        None => {
            serial_println!("[ACPI] RSDP not found; using legacy PIC fallback.");
            None
        }
    };

    // 1. Initialize Adaptive Console (VGA Text Buffer or GOP Truecolor Framebuffer)
    drivers::console::init(boot_info.display);
    drivers::console::clear_screen();

    drivers::console::set_color(Color::LightCyan, Color::Black);
    println!("========================================");
    println!("       Welcome to SaeOS (x86_64)!       ");
    println!("========================================");

    drivers::console::set_color(Color::LightGreen, Color::Black);
    println!("[OK] Available RAM: {} MiB.", boot_info.total_memory_mb());

    match boot_info.display {
        DisplayMode::VgaText { buffer_addr } => {
            drivers::console::set_color(Color::LightCyan, Color::Black);
            println!(
                "[OK] Boot Mode: Legacy BIOS (VGA Text 80x25 @ {:#x}).",
                buffer_addr
            );
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
    let mode = arch::x86_64::init(acpi_topology.as_ref());
    drivers::console::set_color(Color::LightGreen, Color::Black);
    println!("[OK] GDT & TSS with IST loaded.");
    println!("[OK] IDT loaded (256 vectors).");

    match mode {
        ControllerKind::Apic => {
            drivers::console::set_color(Color::LightGreen, Color::Black);
            let (lapic_base, ioapic_base) = {
                let controller = arch::x86_64::interrupt_controller::CONTROLLER.lock();
                (
                    controller.lapic.base_address(),
                    controller.ioapic.base_address(),
                )
            };
            println!(
                "[OK] APIC active (LAPIC @ {:#x}, IOAPIC @ {:#x}).",
                lapic_base, ioapic_base
            );
            println!("[OK] 8259 Legacy PIC masked and disabled.");
        }
        ControllerKind::LegacyPic => {
            drivers::console::set_color(Color::Yellow, Color::Black);
            println!("[WARN] APIC unavailable. Falling back to 8259 Legacy PIC.");
        }
    }

    // 3. Memory management and heap were initialized before ACPI/hardware use.
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
