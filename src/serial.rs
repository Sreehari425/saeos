use crate::sync::SpinMutex;
use core::fmt::{self, Write};

pub const SERIAL_COM1: u16 = 0x3F8;

pub struct SerialPort {
    port: u16,
}

impl SerialPort {
    pub const fn new(port: u16) -> Self {
        Self { port }
    }

    pub fn send_byte(&mut self, byte: u8) {
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") self.port,
                in("al") byte,
                options(nomem, nostack, preserves_flags)
            );
        }
    }

    pub fn send_string(&mut self, s: &str) {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.send_byte(b'\r');
            }
            self.send_byte(byte);
        }
    }
}

impl Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.send_string(s);
        Ok(())
    }
}

impl Default for SerialPort {
    fn default() -> Self {
        Self::new(SERIAL_COM1)
    }
}

pub static SERIAL1: SpinMutex<SerialPort> = SpinMutex::new(SerialPort::new(SERIAL_COM1));

#[doc(hidden)]
pub fn _serial_print(args: fmt::Arguments) {
    SERIAL1.lock().write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_serial_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
