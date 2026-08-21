use crate::arch::x86_64::interrupt_controller::{self, ControllerKind};
use crate::drivers::vga::{self, Color};
use crate::println;
use crate::serial_println;

pub fn execute(cmd: &str) {
    match cmd {
        "help" => {
            vga::set_color(Color::Yellow, Color::Black);
            println!("Available Commands:");
            println!("  help    - Show this help message");
            println!("  clear   - Clear the VGA text screen");
            println!("  apic    - Display APIC & interrupt controller status");
            println!("  <text>  - Echoes your input back to the screen");
        }
        "clear" => {
            vga::clear_screen();
        }
        "apic" | "status" => {
            let ctrl = interrupt_controller::CONTROLLER.lock();
            vga::set_color(Color::LightCyan, Color::Black);
            println!("--- Interrupt Controller Diagnostics ---");
            match ctrl.mode {
                ControllerKind::Apic => {
                    vga::set_color(Color::LightGreen, Color::Black);
                    println!("Mode:          APIC (Local APIC + I/O APIC) [ACTIVE]");
                    vga::set_color(Color::White, Color::Black);
                    println!(
                        "Local APIC:    Base={:#x}, ID={}, Version={:#x}",
                        ctrl.lapic.base_address(),
                        ctrl.lapic.id(),
                        ctrl.lapic.version()
                    );
                    println!(
                        "I/O APIC:      Base={:#x}, ID={}, Max IRQs={}",
                        ctrl.ioapic.base_address(),
                        ctrl.ioapic.id(),
                        ctrl.ioapic.max_redirection_entries()
                    );
                    println!("Legacy 8259:   Masked (Disabled)");
                }
                ControllerKind::LegacyPic => {
                    vga::set_color(Color::LightRed, Color::Black);
                    println!("Mode:          8259 Legacy PIC (Fallback)");
                    vga::set_color(Color::White, Color::Black);
                    println!("Master/Slave:  Base Ports 0x20/0xA0 (IRQs 0x20..=0x2F)");
                }
            }
            println!("----------------------------------------");
        }
        _ => {
            vga::set_color(Color::LightGreen, Color::Black);
            println!("Echo: {}", cmd);
            serial_println!("Echo: {}", cmd);
        }
    }
}
