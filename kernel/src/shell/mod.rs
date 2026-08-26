pub mod commands;

use crate::arch::x86_64::cpu;
use crate::drivers::console;
use crate::drivers::keyboard;
use crate::drivers::vga::Color;
use crate::print;
use crate::println;
use core::fmt::Write;

const BUFFER_CAPACITY: usize = 80;

pub fn run() -> ! {
    println!("SaeOS Interactive Shell");
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
                        console::backspace();
                        let _ = crate::drivers::serial::SERIAL1
                            .lock()
                            .write_str("\x08 \x08");
                    }
                }
                '\n' => {
                    // Enter / Execute
                    println!();

                    if cursor > 0 {
                        if let Ok(line) = core::str::from_utf8(&line_buffer[..cursor]) {
                            commands::execute(line.trim());
                        }
                        cursor = 0;
                    }

                    print_prompt();
                }
                c if c.is_ascii() && !c.is_ascii_control() && cursor < BUFFER_CAPACITY - 1 => {
                    line_buffer[cursor] = c as u8;
                    cursor += 1;
                    console::set_color(Color::White, Color::Black);
                    print!("{}", c);
                }
                _ => {}
            }
        } else {
            // Sleep CPU until next hardware interrupt
            cpu::hlt();
        }
    }
}

fn print_prompt() {
    console::set_color(Color::LightCyan, Color::Black);
    print!("saeos> ");
    console::set_color(Color::White, Color::Black);
}
