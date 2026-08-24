use crate::boot::boot_info::DisplayMode;
use crate::drivers::framebuffer::GOP_WRITER;
use crate::drivers::serial::SERIAL1;
use crate::drivers::vga::{self, Color, WRITER as VGA_WRITER};
use crate::sync::SpinMutex;
use core::fmt::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleKind {
    Vga,
    Gop,
}

struct ConsoleState {
    kind: ConsoleKind,
}

static CONSOLE_STATE: SpinMutex<ConsoleState> = SpinMutex::new(ConsoleState {
    kind: ConsoleKind::Vga,
});

pub fn init(display_mode: DisplayMode) {
    CONSOLE_STATE.reset();
    let mut state = CONSOLE_STATE.lock();
    match display_mode {
        DisplayMode::VgaText { buffer_addr } => {
            state.kind = ConsoleKind::Vga;
            VGA_WRITER.lock().set_buffer_address(buffer_addr as u64);
            VGA_WRITER.lock().clear_screen();
        }
        DisplayMode::GopFramebuffer(info) => {
            state.kind = ConsoleKind::Gop;
            GOP_WRITER.lock().init(info);
        }
    }
}

pub fn clear_screen() {
    let state = CONSOLE_STATE.lock();
    match state.kind {
        ConsoleKind::Vga => VGA_WRITER.lock().clear_screen(),
        ConsoleKind::Gop => GOP_WRITER.lock().clear_screen(),
    }
}

pub fn backspace() {
    let state = CONSOLE_STATE.lock();
    match state.kind {
        ConsoleKind::Vga => VGA_WRITER.lock().backspace(),
        ConsoleKind::Gop => GOP_WRITER.lock().backspace(),
    }
}

pub fn set_color(fg: Color, bg: Color) {
    let state = CONSOLE_STATE.lock();
    match state.kind {
        ConsoleKind::Vga => vga::set_color(fg, bg),
        ConsoleKind::Gop => GOP_WRITER.lock().set_color(fg, bg),
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let state = CONSOLE_STATE.lock();
    match state.kind {
        ConsoleKind::Vga => {
            VGA_WRITER.lock().write_fmt(args).unwrap();
        }
        ConsoleKind::Gop => {
            GOP_WRITER.lock().write_fmt(args).unwrap();
        }
    }
    // Mirror to serial COM1 console (stdio)
    SERIAL1.lock().write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::drivers::console::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
