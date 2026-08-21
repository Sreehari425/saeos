use crate::interrupt_controller::{self, ControllerKind};
use crate::keyboard;
use crate::print;
use crate::println;
use crate::serial_print;
use crate::serial_println;
use crate::vga_buffer::{self, Color};

const BUFFER_CAPACITY: usize = 80;

pub fn run() -> ! {
    println!("SaeOS Interactive Shell (APIC Enabled)");
    println!("Type 'help' for commands, or type anything to echo.\n");

    print_prompt();

    let mut line_buffer = [0u8; BUFFER_CAPACITY];
    let mut cursor = 0;

    loop {
        if let Some(c) = keyboard::pop_key() {
            match c {
                '\x08' => {
                    // Backspace
                    if cursor > 0 {
                        cursor -= 1;
                        vga_buffer::backspace();
                        serial_print!("\x08 \x08");
                    }
                }
                '\n' => {
                    // Enter / Execute
                    println!();
                    serial_println!();

                    if cursor > 0 {
                        if let Ok(line) = core::str::from_utf8(&line_buffer[..cursor]) {
                            execute_command(line.trim());
                        }
                        cursor = 0;
                    }

                    print_prompt();
                }
                c if c.is_ascii() && !c.is_ascii_control() && cursor < BUFFER_CAPACITY - 1 => {
                    line_buffer[cursor] = c as u8;
                    cursor += 1;
                    vga_buffer::set_color(Color::White, Color::Black);
                    print!("{}", c);
                    serial_print!("{}", c);
                }
                _ => {}
            }
        } else {
            // Sleep CPU until next hardware interrupt
            unsafe {
                core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
            }
        }
    }
}

fn execute_command(cmd: &str) {
    match cmd {
        "help" => {
            vga_buffer::set_color(Color::Yellow, Color::Black);
            println!("Available Commands:");
            println!("  help    - Show this help message");
            println!("  clear   - Clear the VGA text screen");
            println!("  apic    - Display APIC & interrupt controller status");
            println!("  <text>  - Echoes your input back to the screen");
        }
        "clear" => {
            vga_buffer::clear_screen();
        }
        "apic" | "status" => {
            let ctrl = interrupt_controller::CONTROLLER.lock();
            vga_buffer::set_color(Color::LightCyan, Color::Black);
            println!("--- Interrupt Controller Diagnostics ---");
            match ctrl.mode {
                ControllerKind::Apic => {
                    vga_buffer::set_color(Color::LightGreen, Color::Black);
                    println!("Mode:          APIC (Local APIC + I/O APIC) [ACTIVE]");
                    vga_buffer::set_color(Color::White, Color::Black);
                    println!("Local APIC:    Base={:#x}, ID={}, Version={:#x}", ctrl.lapic.base_address(), ctrl.lapic.id(), ctrl.lapic.version());
                    println!("I/O APIC:      Base={:#x}, ID={}, Max IRQs={}", ctrl.ioapic.base_address(), ctrl.ioapic.id(), ctrl.ioapic.max_redirection_entries());
                    println!("Legacy 8259:   Masked (Disabled)");
                }
                ControllerKind::LegacyPic => {
                    vga_buffer::set_color(Color::LightRed, Color::Black);
                    println!("Mode:          8259 Legacy PIC (Fallback)");
                    vga_buffer::set_color(Color::White, Color::Black);
                    println!("Master/Slave:  Base Ports 0x20/0xA0 (IRQs 0x20..=0x2F)");
                }
            }
            println!("----------------------------------------");
        }
        _ => {
            vga_buffer::set_color(Color::LightGreen, Color::Black);
            println!("Echo: {}", cmd);
            serial_println!("Echo: {}", cmd);
        }
    }
}

fn print_prompt() {
    vga_buffer::set_color(Color::LightCyan, Color::Black);
    print!("saeos> ");
    serial_print!("saeos> ");
    vga_buffer::set_color(Color::White, Color::Black);
}
