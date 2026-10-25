#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

pub mod arch;
pub mod backtrace;
pub mod boot;
#[cfg(feature = "boot-test")]
mod boot_test;
pub mod cmdline;
pub mod collections;
pub mod drivers;
pub mod fatal;
pub mod fs;
#[macro_use]
pub mod klog;
mod fault_tests;
pub mod klog_ring;
pub mod mm;
pub mod process;
pub mod scheduler;
pub mod shell;
pub mod sync;
pub mod task;
pub mod time;

use arch::x86_64::acpi;
use arch::x86_64::interrupt_controller::ControllerKind;
#[cfg(not(target_os = "uefi"))]
use boot::boot_info::KernelSection;
use boot::boot_info::{BootInfo, BootMode, DisplayMode, KernelSections};
use core::panic::PanicInfo;
use drivers::vga::Color;

/// Must match `KERNEL_OFFSET` in `linker.ld` and `boot.asm`.
const KERNEL_OFFSET: u64 = mm::layout::KERNEL_IMAGE_BASE;

#[cfg(not(target_os = "uefi"))]
unsafe extern "C" {
    static _kernel_start: u8;
    static _kernel_end: u8;
    static _text_start: u8;
    static _text_end: u8;
    static _rodata_start: u8;
    static _rodata_end: u8;
    static _data_start: u8;
    static _data_end: u8;
    static _bss_start: u8;
    static _bss_end: u8;
}

/// The linker places the image at `KERNEL_OFFSET + physical`; these report
/// the physical load addresses the memory manager tracks.
#[cfg(not(target_os = "uefi"))]
fn kernel_physical_bounds() -> (u64, u64) {
    use mm::layout::static_phys;
    (
        static_phys(&raw const _kernel_start),
        static_phys(&raw const _kernel_end),
    )
}

#[cfg(not(target_os = "uefi"))]
fn kernel_sections() -> KernelSections {
    use mm::layout::static_phys;
    let section = |start: *const u8, end: *const u8| KernelSection {
        start: static_phys(start),
        end: static_phys(end),
    };
    KernelSections {
        text: section(&raw const _text_start, &raw const _text_end),
        rodata: section(&raw const _rodata_start, &raw const _rodata_end),
        data: section(&raw const _data_start, &raw const _data_end),
        bss: section(&raw const _bss_start, &raw const _bss_end),
    }
}

#[cfg(target_os = "uefi")]
const fn kernel_physical_bounds() -> (u64, u64) {
    (0, 0)
}

#[cfg(target_os = "uefi")]
const fn kernel_sections() -> KernelSections {
    KernelSections::EMPTY
}

/// BIOS entrypoint called from `boot.asm` with the Multiboot 1 info pointer.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main_bios(multiboot_info_addr: usize) -> ! {
    // boot.asm already runs the image in the top-2-GiB window; record that
    // before anything converts a static's address to a physical one.
    mm::layout::set_image_delta(KERNEL_OFFSET);
    let mut memory_map = boot::boot_info::PhysicalMemoryMap::empty();
    let mut initramfs = [None; boot::boot_info::MAX_INITRAMFS];
    let mut cmdline = boot::boot_info::Cmdline::EMPTY;
    if multiboot_info_addr != 0 {
        let mb_info = unsafe { &*(multiboot_info_addr as *const mm::MultibootInfo) };
        memory_map = mb_info.usable_memory_map();
        // Copy everything out of bootloader memory now: it is ordinary RAM
        // to the allocator once mm is up.
        if let Some(line) = mb_info.cmdline_bytes() {
            cmdline = boot::boot_info::Cmdline::new(strip_kernel_path(line));
        }
        for (slot, module) in initramfs.iter_mut().zip(mb_info.modules()) {
            if module.mod_start < module.mod_end {
                *slot = Some(boot::boot_info::PhysicalRange {
                    start: u64::from(module.mod_start),
                    end: u64::from(module.mod_end),
                });
            }
        }
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
        kernel_sections: kernel_sections(),
        rsdp_addr: acpi::find_rsdp_bios(),
        memory_map_descriptor_count: memory_map.count,
        memory_map_discarded: 0,
        initramfs,
        cmdline,
    };

    kernel_main(&boot_info);
}

/// Multiboot loaders (QEMU, GRUB) put the kernel's path before the
/// parameters; drop a first word that looks like a path, not a parameter.
fn strip_kernel_path(line: &[u8]) -> &[u8] {
    let line = line.trim_ascii_start();
    let word = line
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(line.len());
    if line[..word].contains(&b'/') && !line[..word].contains(&b'=') {
        line[word..].trim_ascii_start()
    } else {
        line
    }
}

/// Unified kernel entrypoint shared by both BIOS and UEFI.
pub fn kernel_main(boot_info: &BootInfo) -> ! {
    // Both BIOS and UEFI mirror console output to COM1. UEFI firmware may
    // have initialized the UART already, but initializing it here keeps the
    // BIOS path deterministic as well.
    #[cfg(feature = "serial")]
    drivers::serial::init();

    // BIOS still has the boot assembly's writable identity map here, so parse
    // firmware tables before replacing CR3 with the sparse final map. UEFI
    // performs this after mm initialization because firmware paging is gone.
    let bios_acpi = if boot_info.boot_mode == BootMode::Bios {
        boot_info
            .rsdp_addr
            .and_then(|address| unsafe { acpi::parse_rsdp(address).ok() })
    } else {
        None
    };

    mm::init_early(boot_info);

    let handoff = Handoff {
        boot_info: *boot_info,
        bios_acpi,
    };
    // UEFI still executes from the firmware's identity mapping of the image;
    // rebase it into the top-2-GiB window before continuing. BIOS linked the
    // image there, so it simply carries on.
    #[cfg(target_os = "uefi")]
    unsafe {
        arch::x86_64::uefi::relocate::enter_higher_half(handoff)
    }
    #[cfg(not(target_os = "uefi"))]
    kernel_main_high(handoff)
}

/// State carried across the switch from the boot-time image alias to the
/// final high-half image.
#[derive(Clone, Copy)]
pub struct Handoff {
    pub boot_info: BootInfo,
    pub bios_acpi: Option<acpi::InterruptTopology>,
}

/// Second half of kernel bring-up, always running from the high image window.
pub fn kernel_main_high(handoff: Handoff) -> ! {
    let boot_info = &handoff.boot_info;
    let bios_acpi = handoff.bios_acpi;
    // First, so loglevel= applies to the rest of bring-up.
    cmdline::init(&boot_info.cmdline);
    mm::init_late(boot_info);
    fs::init(boot_info);

    let acpi_topology = bios_acpi.or_else(|| match boot_info.rsdp_addr {
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
    });

    // 1. Initialize Adaptive Console (VGA Text Buffer or GOP Truecolor Framebuffer)
    drivers::console::init(boot_info.display);
    drivers::console::clear_screen();

    drivers::console::set_color(Color::LightCyan, Color::Black);
    println!("========================================");
    println!("       Welcome to SaeOS (x86_64)!       ");
    println!("========================================");

    drivers::console::set_color(Color::LightGreen, Color::Black);
    println!("[OK] Available RAM: {} MiB.", boot_info.total_memory_mb());
    println!(
        "[OK] Highest physical address: {:#x}.",
        boot_info.memory_map.regions[..boot_info.memory_map.count]
            .iter()
            .map(|region| region.end())
            .max()
            .unwrap_or(0)
    );

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
    time::init();
    // GDT, IDT, TSS, and every MMIO pointer now use upper-half addresses, so
    // the boot identity map can go; the lower half belongs to processes.
    mm::finish_boot();
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
            #[cfg(feature = "apic-timer")]
            {
                if time::apic_timer::is_active() {
                    println!(
                        "[OK] Local APIC Timer active ({} ticks/ms).",
                        time::apic_timer::ticks_per_ms()
                    );
                }
            }
        }
        ControllerKind::LegacyPic => {
            drivers::console::set_color(Color::Yellow, Color::Black);
            println!("[WARN] APIC unavailable. Falling back to 8259 Legacy PIC.");
        }
    }

    #[cfg(feature = "tsc")]
    {
        drivers::console::set_color(Color::LightGreen, Color::Black);
        let hz = time::tsc::frequency_hz();
        let mhz = hz / 1_000_000;
        let remainder_khz = (hz % 1_000_000) / 1_000;
        let inv_str = if time::tsc::is_invariant() {
            "Invariant TSC"
        } else {
            "Non-Invariant TSC"
        };
        println!(
            "[OK] {} calibrated @ {}.{:03} MHz.",
            inv_str, mhz, remainder_khz
        );
    }

    // 3. Memory management and heap were initialized before ACPI/hardware use.
    drivers::console::set_color(Color::LightGreen, Color::Black);
    println!(
        "[OK] Kernel Heap Allocator initialized (10 MiB at {:#x}).",
        mm::heap_start()
    );

    // 4. Initialize Keyboard Controller & Enable CPU Hardware Interrupts
    #[cfg(feature = "keyboard")]
    drivers::keyboard::init();
    scheduler::init();
    println!("[OK] Scheduler initialized; starting the first task.\n");

    // 5. Start the scheduler. The shell is now a kernel task and the idle
    // task handles periods where no normal task is runnable.
    scheduler::start_first_task();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // The panic-lock test panics on purpose; anything else is a failure.
    fatal::report(
        format_args!("[KERNEL PANIC]\n{}", info),
        cfg!(feature = "panic-lock-test"),
    )
}
