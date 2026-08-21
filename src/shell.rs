use crate::keyboard;
use crate::print;
use crate::println;
use crate::serial_print;
use crate::serial_println;
use crate::vga_buffer::{self, Color};

const BUFFER_CAPACITY: usize = 80;

pub fn run() -> ! {
    println!("SaeOS Echo Shell");
    println!("Type anything and press Enter to echo back.\n");

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
                            vga_buffer::set_color(Color::LightGreen, Color::Black);
                            println!("Echo: {}", line);
                            serial_println!("Echo: {}", line);
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
            // Put CPU to sleep until next hardware interrupt (Timer / Keyboard)
            unsafe {
                core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
            }
        }
    }
}

fn print_prompt() {
    vga_buffer::set_color(Color::LightCyan, Color::Black);
    print!("saeos> ");
    serial_print!("saeos> ");
    vga_buffer::set_color(Color::White, Color::Black);
}
