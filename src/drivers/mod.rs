pub mod console;
pub mod font;
pub mod framebuffer;
pub mod keyboard;
pub mod serial;
pub mod vga;

pub use console::{backspace, clear_screen, set_color};
pub use keyboard::pop_key;
pub use vga::{Color, ColorCode};
