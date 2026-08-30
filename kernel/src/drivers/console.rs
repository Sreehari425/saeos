use crate::boot::boot_info::DisplayMode;
#[cfg(feature = "framebuffer")]
use crate::drivers::framebuffer::GOP_WRITER;
#[cfg(feature = "serial")]
use crate::drivers::serial::SERIAL1;
use crate::drivers::vga::Color;
#[cfg(feature = "vga-text")]
use crate::drivers::vga::{self, WRITER as VGA_WRITER};
use crate::sync::SpinMutex;
use core::fmt::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleKind {
    Vga,
    #[cfg(feature = "framebuffer")]
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
            #[cfg(feature = "vga-text")]
            {
                VGA_WRITER.lock().set_buffer_address(buffer_addr as u64);
                VGA_WRITER.lock().clear_screen();
            }
            #[cfg(not(feature = "vga-text"))]
            let _ = buffer_addr;
        }
        DisplayMode::GopFramebuffer(info) => {
            #[cfg(feature = "framebuffer")]
            {
                state.kind = ConsoleKind::Gop;
                GOP_WRITER.lock().init(info);
            }
            #[cfg(not(feature = "framebuffer"))]
            {
                let _ = info;
                state.kind = ConsoleKind::Vga;
                #[cfg(feature = "vga-text")]
                {
                    VGA_WRITER.lock().clear_screen();
                }
            }
        }
    }
}

pub fn clear_screen() {
    let state = CONSOLE_STATE.lock();
    match state.kind {
        #[cfg(feature = "vga-text")]
        ConsoleKind::Vga => VGA_WRITER.lock().clear_screen(),
        #[cfg(not(feature = "vga-text"))]
        ConsoleKind::Vga => {}
        #[cfg(feature = "framebuffer")]
        ConsoleKind::Gop => GOP_WRITER.lock().clear_screen(),
    }
}

pub fn backspace() {
    let state = CONSOLE_STATE.lock();
    match state.kind {
        #[cfg(feature = "vga-text")]
        ConsoleKind::Vga => VGA_WRITER.lock().backspace(),
        #[cfg(not(feature = "vga-text"))]
        ConsoleKind::Vga => {}
        #[cfg(feature = "framebuffer")]
        ConsoleKind::Gop => GOP_WRITER.lock().backspace(),
    }
}

pub fn set_color(fg: Color, bg: Color) {
    let state = CONSOLE_STATE.lock();
    match state.kind {
        #[cfg(feature = "vga-text")]
        ConsoleKind::Vga => vga::set_color(fg, bg),
        #[cfg(not(feature = "vga-text"))]
        ConsoleKind::Vga => {}
        #[cfg(feature = "framebuffer")]
        ConsoleKind::Gop => GOP_WRITER.lock().set_color(fg, bg),
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let state = CONSOLE_STATE.lock();
    match state.kind {
        #[cfg(feature = "vga-text")]
        ConsoleKind::Vga => {
            VGA_WRITER.lock().write_fmt(args).unwrap();
        }
        #[cfg(not(feature = "vga-text"))]
        ConsoleKind::Vga => {}
        #[cfg(feature = "framebuffer")]
        ConsoleKind::Gop => {
            GOP_WRITER.lock().write_fmt(args).unwrap();
        }
    }
    // Mirror to serial COM1 console (stdio)
    #[cfg(feature = "serial")]
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
