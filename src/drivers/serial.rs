use crate::arch::x86_64::cpu;
use crate::sync::SpinMutex;
use core::fmt::{self, Write};

const PORT_COM1: u16 = 0x3F8;

pub struct SerialPort {
    port: u16,
}

impl SerialPort {
    pub const fn new(port: u16) -> Self {
        Self { port }
    }

    pub fn write_byte(&self, byte: u8) {
        unsafe {
            cpu::outb(self.port, byte);
        }
    }

    pub fn write_string(&self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }
}

impl Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

pub static SERIAL1: SpinMutex<SerialPort> = SpinMutex::new(SerialPort::new(PORT_COM1));

#[doc(hidden)]
pub fn _serial_print(args: fmt::Arguments) {
    SERIAL1.lock().write_fmt(args).unwrap();
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::drivers::serial::_serial_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
