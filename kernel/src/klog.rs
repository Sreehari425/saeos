//! Kernel log: everything printed to the serial port or console is kept in
//! a 64 KiB ring (`dmesg` in the shell), and the leveled macros
//! (`kerror!`, `kwarn!`, `kinfo!`, `kdebug!`) add a runtime threshold.
//!
//! The ring never allocates and sits behind an `IrqSpinMutex`, so interrupt
//! handlers and the fatal path (which steals the lock) may log.

use crate::klog_ring::Ring;
use crate::sync::IrqSpinMutex;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

pub const CAPACITY: usize = 64 * 1024;

static LOG: IrqSpinMutex<Ring<CAPACITY>> = IrqSpinMutex::new(Ring::new());
/// Set while `dmesg` replays the ring, so the replay is not logged again.
static PAUSED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    pub const fn tag(self) -> &'static str {
        match self {
            Level::Error => "[ERR]",
            Level::Warn => "[WRN]",
            Level::Info => "[INF]",
            Level::Debug => "[DBG]",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "error" => Some(Level::Error),
            "warn" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            _ => None,
        }
    }
}

const DEFAULT_LEVEL: Level = if cfg!(feature = "scheduler-debug") {
    Level::Debug
} else {
    Level::Info
};
static LEVEL: AtomicU8 = AtomicU8::new(DEFAULT_LEVEL as u8);

pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Level::Error,
        1 => Level::Warn,
        2 => Level::Info,
        _ => Level::Debug,
    }
}

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 <= LEVEL.load(Ordering::Relaxed)
}

/// Append to the ring (called by the serial and console print paths).
pub fn record(args: fmt::Arguments) {
    if !PAUSED.load(Ordering::Relaxed) {
        let _ = LOG.lock().write_fmt(args);
    }
}

/// Emit a leveled message: to the ring, and to the serial port when the
/// `serial` feature is on. Not to the screen: logs are for diagnostics.
pub fn log(level: Level, args: fmt::Arguments) {
    if !enabled(level) {
        return;
    }
    #[cfg(feature = "serial")]
    crate::drivers::serial::_serial_print(format_args!("{} {}\n", level.tag(), args));
    #[cfg(not(feature = "serial"))]
    record(format_args!("{} {}\n", level.tag(), args));
}

/// Whether `text` is in the log (diagnostics and tests).
pub fn contains(text: &str) -> bool {
    LOG.lock().contains(text.as_bytes())
}

/// Copy the log out and hand it to `print` without recording the replay.
/// Task context only: the copy is heap allocated.
pub fn replay(print: impl Fn(&str)) {
    let snapshot = {
        let ring = LOG.lock();
        let (first, second) = ring.as_slices();
        [first, second].concat()
    };
    PAUSED.store(true, Ordering::Relaxed);
    for line in snapshot.split(|byte| *byte == b'\n') {
        print(core::str::from_utf8(line).unwrap_or("<non-UTF-8 log line>"));
    }
    PAUSED.store(false, Ordering::Relaxed);
}

#[macro_export]
macro_rules! kerror {
    ($($arg:tt)*) => ($crate::klog::log($crate::klog::Level::Error, format_args!($($arg)*)));
}

#[macro_export]
macro_rules! kwarn {
    ($($arg:tt)*) => ($crate::klog::log($crate::klog::Level::Warn, format_args!($($arg)*)));
}

#[macro_export]
macro_rules! kinfo {
    ($($arg:tt)*) => ($crate::klog::log($crate::klog::Level::Info, format_args!($($arg)*)));
}

#[macro_export]
macro_rules! kdebug {
    ($($arg:tt)*) => ($crate::klog::log($crate::klog::Level::Debug, format_args!($($arg)*)));
}
