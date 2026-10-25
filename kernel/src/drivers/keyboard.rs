use crate::sync::IrqSpinMutex;
use crate::task::WaitQueue;

const KEY_BUFFER_CAPACITY: usize = 256;

/// Raw console input bytes, from the keyboard and the serial port, before
/// line editing (`drivers::tty` reads them in the reader's context).
pub struct KeyQueue {
    buffer: [u8; KEY_BUFFER_CAPACITY],
    head: usize,
    tail: usize,
    count: usize,
}

impl KeyQueue {
    pub const fn new() -> Self {
        Self {
            buffer: [0; KEY_BUFFER_CAPACITY],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    pub fn push(&mut self, c: u8) {
        if self.count < KEY_BUFFER_CAPACITY {
            self.buffer[self.tail] = c;
            self.tail = (self.tail + 1) % KEY_BUFFER_CAPACITY;
            self.count += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn pop(&mut self) -> Option<u8> {
        if self.count == 0 {
            None
        } else {
            let c = self.buffer[self.head];
            self.head = (self.head + 1) % KEY_BUFFER_CAPACITY;
            self.count -= 1;
            Some(c)
        }
    }
}

impl Default for KeyQueue {
    fn default() -> Self {
        Self::new()
    }
}

// Both are taken by the keyboard IRQ handler, so holders must mask interrupts.
pub static KEY_QUEUE: IrqSpinMutex<KeyQueue> = IrqSpinMutex::new(KeyQueue::new());

struct KeyboardState {
    lshift: bool,
    rshift: bool,
    caps_lock: bool,
    /// Either Ctrl key (the right one arrives as E0 1D, and the E0 prefix
    /// is ignored).
    control: bool,
}

impl KeyboardState {
    pub const fn new() -> Self {
        Self {
            lshift: false,
            rshift: false,
            caps_lock: false,
            control: false,
        }
    }

    pub fn is_shifted(&self) -> bool {
        (self.lshift || self.rshift) ^ self.caps_lock
    }
}

static KEYBOARD_STATE: IrqSpinMutex<KeyboardState> = IrqSpinMutex::new(KeyboardState::new());

/// Readers sleep here until the keyboard interrupt queues a key.
pub static KEY_WAIT: IrqSpinMutex<WaitQueue> =
    IrqSpinMutex::new(WaitQueue::new(crate::scheduler::KEYBOARD_WAIT_ID));

pub fn has_key() -> bool {
    !KEY_QUEUE.lock().is_empty()
}

/// The next input byte as a character (the kernel shell is ASCII-only).
pub fn pop_key() -> Option<char> {
    KEY_QUEUE.lock().pop().map(char::from)
}

pub fn pop_byte() -> Option<u8> {
    KEY_QUEUE.lock().pop()
}

/// Queue one input byte and wake a reader. Safe from interrupt handlers.
pub fn push_input(byte: u8) {
    KEY_QUEUE.lock().push(byte);
    crate::scheduler::wake_one(&KEY_WAIT);
}

/// Initialize the 8042 PS/2 keyboard controller.
/// Enables the keyboard port, enables IRQ1 interrupts in the configuration byte, and starts keyboard scanning.
pub fn init() {
    unsafe {
        // Drain any pending output in 8042 buffer
        while (inb(0x64) & 0x01) != 0 {
            let _ = inb(0x60);
        }

        // Enable first PS/2 port (keyboard)
        outb(0x64, 0xAE);

        // Read 8042 Controller Configuration Byte
        outb(0x64, 0x20);
        wait_input_full();
        let mut config = inb(0x60);

        // Enable IRQ1 (bit 0) and enable port clock (clear bit 4)
        config |= 0x01; // First PS/2 port interrupt enabled
        config &= !0x10; // First PS/2 port clock enabled

        // Write updated Configuration Byte back
        outb(0x64, 0x60);
        wait_input_empty();
        outb(0x60, config);

        // Send enable scanning command to keyboard
        wait_input_empty();
        outb(0x60, 0xF4);

        // Drain response
        let mut timeout = 10000;
        while (inb(0x64) & 0x01) != 0 && timeout > 0 {
            let _ = inb(0x60);
            timeout -= 1;
        }
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    }
    value
}

#[inline]
unsafe fn outb(port: u16, value: u8) {
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

#[inline]
unsafe fn wait_input_empty() {
    for _ in 0..100_000 {
        if (unsafe { inb(0x64) } & 0x02) == 0 {
            break;
        }
    }
}

#[inline]
unsafe fn wait_input_full() {
    for _ in 0..100_000 {
        if (unsafe { inb(0x64) } & 0x01) != 0 {
            break;
        }
    }
}

pub fn handle_scancode(scancode: u8) {
    let mut state = KEYBOARD_STATE.lock();

    // Check release events (high bit set = 0x80)
    match scancode {
        0x2A => {
            state.lshift = true;
            return;
        }
        0xAA => {
            state.lshift = false;
            return;
        }
        0x36 => {
            state.rshift = true;
            return;
        }
        0xB6 => {
            state.rshift = false;
            return;
        }
        0x3A => {
            state.caps_lock = !state.caps_lock;
            return;
        }
        0x1D => {
            state.control = true;
            return;
        }
        0x9D => {
            state.control = false;
            return;
        }
        _ => {}
    }

    if scancode & 0x80 != 0 {
        // Other key release event, ignore
        return;
    }

    let is_shifted = state.is_shifted();
    let is_symbol_shifted = state.lshift || state.rshift;

    let char_opt = match scancode {
        0x01 => Some('\x1b'), // Esc
        0x02 => Some(if is_symbol_shifted { '!' } else { '1' }),
        0x03 => Some(if is_symbol_shifted { '@' } else { '2' }),
        0x04 => Some(if is_symbol_shifted { '#' } else { '3' }),
        0x05 => Some(if is_symbol_shifted { '$' } else { '4' }),
        0x06 => Some(if is_symbol_shifted { '%' } else { '5' }),
        0x07 => Some(if is_symbol_shifted { '^' } else { '6' }),
        0x08 => Some(if is_symbol_shifted { '&' } else { '7' }),
        0x09 => Some(if is_symbol_shifted { '*' } else { '8' }),
        0x0A => Some(if is_symbol_shifted { '(' } else { '9' }),
        0x0B => Some(if is_symbol_shifted { ')' } else { '0' }),
        0x0C => Some(if is_symbol_shifted { '_' } else { '-' }),
        0x0D => Some(if is_symbol_shifted { '+' } else { '=' }),
        0x0E => Some('\x08'), // Backspace
        0x0F => Some('\t'),   // Tab
        0x10 => Some(if is_shifted { 'Q' } else { 'q' }),
        0x11 => Some(if is_shifted { 'W' } else { 'w' }),
        0x12 => Some(if is_shifted { 'E' } else { 'e' }),
        0x13 => Some(if is_shifted { 'R' } else { 'r' }),
        0x14 => Some(if is_shifted { 'T' } else { 't' }),
        0x15 => Some(if is_shifted { 'Y' } else { 'y' }),
        0x16 => Some(if is_shifted { 'U' } else { 'u' }),
        0x17 => Some(if is_shifted { 'I' } else { 'i' }),
        0x18 => Some(if is_shifted { 'O' } else { 'o' }),
        0x19 => Some(if is_shifted { 'P' } else { 'p' }),
        0x1A => Some(if is_symbol_shifted { '{' } else { '[' }),
        0x1B => Some(if is_symbol_shifted { '}' } else { ']' }),
        0x1C => Some('\n'), // Enter
        0x1E => Some(if is_shifted { 'A' } else { 'a' }),
        0x1F => Some(if is_shifted { 'S' } else { 's' }),
        0x20 => Some(if is_shifted { 'D' } else { 'd' }),
        0x21 => Some(if is_shifted { 'F' } else { 'f' }),
        0x22 => Some(if is_shifted { 'G' } else { 'g' }),
        0x23 => Some(if is_shifted { 'H' } else { 'h' }),
        0x24 => Some(if is_shifted { 'J' } else { 'j' }),
        0x25 => Some(if is_shifted { 'K' } else { 'k' }),
        0x26 => Some(if is_shifted { 'L' } else { 'l' }),
        0x27 => Some(if is_symbol_shifted { ':' } else { ';' }),
        0x28 => Some(if is_symbol_shifted { '"' } else { '\'' }),
        0x29 => Some(if is_symbol_shifted { '~' } else { '`' }),
        0x2B => Some(if is_symbol_shifted { '|' } else { '\\' }),
        0x2C => Some(if is_shifted { 'Z' } else { 'z' }),
        0x2D => Some(if is_shifted { 'X' } else { 'x' }),
        0x2E => Some(if is_shifted { 'C' } else { 'c' }),
        0x2F => Some(if is_shifted { 'V' } else { 'v' }),
        0x30 => Some(if is_shifted { 'B' } else { 'b' }),
        0x31 => Some(if is_shifted { 'N' } else { 'n' }),
        0x32 => Some(if is_shifted { 'M' } else { 'm' }),
        0x33 => Some(if is_symbol_shifted { '<' } else { ',' }),
        0x34 => Some(if is_symbol_shifted { '>' } else { '.' }),
        0x35 => Some(if is_symbol_shifted { '?' } else { '/' }),
        0x39 => Some(' '), // Space
        _ => None,
    };

    // Ctrl+letter gives the control character (Ctrl-C = 0x03, Ctrl-D =
    // 0x04, ...), as a terminal does.
    let control = state.control;
    drop(state);
    if let Some(c) = char_opt {
        let byte = c as u8;
        let byte = if control && byte.is_ascii_alphabetic() {
            byte.to_ascii_lowercase() & 0x1f
        } else {
            byte
        };
        push_input(byte);
    }
}
