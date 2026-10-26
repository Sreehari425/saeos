pub mod ac97;
pub mod bochs_vbe;
pub mod console;
pub mod fb;
pub mod font;
#[cfg(feature = "framebuffer")]
pub mod framebuffer;
pub mod hda;
pub mod keyboard;
pub mod pci;
pub mod serial;
pub mod tty;
pub mod vga;

pub use console::{backspace, clear_screen, set_color};
pub use keyboard::pop_key;
pub use vga::{Color, ColorCode};
