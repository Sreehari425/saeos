//! The kernel console: the screen (VGA text or the GOP framebuffer) and
//! the serial port.
//!
//! While a program owns the screen (`/dev/fb0` is open,
//! [`take_screen`]), the console stops drawing on it, as Linux does in
//! `KD_GRAPHICS` mode; the kernel log and serial output go on. When the
//! program lets go ([`release_screen`]) the screen is cleared and the
//! console draws again. A panic takes the screen back at once.

use crate::boot::boot_info::DisplayMode;
#[cfg(feature = "framebuffer")]
use crate::drivers::framebuffer::GOP_WRITER;
#[cfg(feature = "serial")]
use crate::drivers::serial::SERIAL1;
use crate::drivers::vga::Color;
#[cfg(feature = "vga-text")]
use crate::drivers::vga::{self, WRITER as VGA_WRITER};
use crate::sync::SpinMutex;
use core::fmt;
#[cfg(any(feature = "vga-text", feature = "framebuffer", feature = "serial"))]
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

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

/// A program owns the screen: draw nothing on it. An atomic, so the
/// panic path can take the screen back without a lock.
static GRAPHICS: AtomicBool = AtomicBool::new(false);

fn drawing() -> bool {
    !GRAPHICS.load(Ordering::Acquire)
}

/// A program takes the screen: the console stops drawing on it.
pub fn take_screen() {
    GRAPHICS.store(true, Ordering::Release);
}

/// The program let go: clear the screen and draw again.
pub fn release_screen() {
    GRAPHICS.store(false, Ordering::Release);
    clear_screen();
}

/// For the fatal path: draw again, whatever the screen shows.
pub fn reclaim_screen_for_panic() {
    GRAPHICS.store(false, Ordering::Release);
}

/// Hold the console lock (the panic-lock test panics while holding it).
#[cfg(feature = "panic-lock-test")]
pub fn lock_for_panic_test() -> impl Sized {
    CONSOLE_STATE.lock()
}

pub fn init(display_mode: DisplayMode) {
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
    if !drawing() {
        return;
    }
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
    if !drawing() {
        return;
    }
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
        ConsoleKind::Vga => {
            let _ = (fg, bg);
        }
        #[cfg(feature = "framebuffer")]
        ConsoleKind::Gop => GOP_WRITER.lock().set_color(fg, bg),
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    crate::klog::record(args);
    let state = CONSOLE_STATE.lock();
    match state.kind {
        _ if !drawing() => {}
        #[cfg(feature = "vga-text")]
        ConsoleKind::Vga => {
            VGA_WRITER.lock().write_fmt(args).unwrap();
        }
        #[cfg(not(feature = "vga-text"))]
        ConsoleKind::Vga => {
            let _ = args;
        }
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
