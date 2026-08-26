use crate::arch::x86_64::cpu;
use crate::sync::SpinMutex;
use core::fmt::{self, Write};

const PORT_COM1: u16 = 0x3F8;
const LINE_STATUS: u16 = 5;

pub struct SerialPort {
    port: u16,
}

impl SerialPort {
    pub const fn new(port: u16) -> Self {
        Self { port }
    }

    /// Initialize the UART as COM1: 115200 baud, 8 data bits, no parity,
    /// one stop bit, with the FIFO enabled.
    pub fn init(&self) {
        unsafe {
            cpu::outb(self.port + 1, 0x00); // Disable interrupts
            cpu::outb(self.port + 3, 0x80); // Enable divisor latch
            cpu::outb(self.port, 0x01); // Divisor low: 115200 baud
            cpu::outb(self.port + 1, 0x00); // Divisor high
            cpu::outb(self.port + 3, 0x03); // 8N1
            cpu::outb(self.port + 2, 0xC7); // Enable and clear FIFO
            cpu::outb(self.port + 4, 0x0B); // IRQs enabled, RTS/DTR set
        }
    }

    pub fn write_byte(&self, byte: u8) {
        unsafe {
            // Do not overwrite a byte still held by the UART transmitter.
            while (cpu::inb(self.port + LINE_STATUS) & 0x20) == 0 {
                core::hint::spin_loop();
            }
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

pub fn init() {
    SERIAL1.lock().init();
}

#[doc(hidden)]
#[cfg(feature = "serial")]
pub fn _serial_print(args: fmt::Arguments) {
    SERIAL1.lock().write_fmt(args).unwrap();
}

#[macro_export]
#[cfg(feature = "serial")]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::drivers::serial::_serial_print(format_args!($($arg)*)));
}

#[macro_export]
#[cfg(feature = "serial")]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}

#[cfg(not(feature = "serial"))]
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {{
        let _ = core::format_args!($($arg)*);
    }};
}

#[cfg(not(feature = "serial"))]
#[macro_export]
macro_rules! serial_println {
    () => {{}};
    ($($arg:tt)*) => {{
        let _ = core::format_args!("{}\n", core::format_args!($($arg)*));
    }};
}
