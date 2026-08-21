use crate::sync::SpinMutex;

const QUEUE_SIZE: usize = 128;

struct KeyQueue {
    buffer: [char; QUEUE_SIZE],
    read_pos: usize,
    write_pos: usize,
}

impl KeyQueue {
    const fn new() -> Self {
        Self {
            buffer: ['\0'; QUEUE_SIZE],
            read_pos: 0,
            write_pos: 0,
        }
    }

    fn push(&mut self, c: char) {
        let next_write = (self.write_pos + 1) % QUEUE_SIZE;
        if next_write != self.read_pos {
            self.buffer[self.write_pos] = c;
            self.write_pos = next_write;
        }
    }

    fn pop(&mut self) -> Option<char> {
        if self.read_pos == self.write_pos {
            None
        } else {
            let c = self.buffer[self.read_pos];
            self.read_pos = (self.read_pos + 1) % QUEUE_SIZE;
            Some(c)
        }
    }
}

static KEY_QUEUE: SpinMutex<KeyQueue> = SpinMutex::new(KeyQueue::new());

struct KeyboardState {
    lshift: bool,
    rshift: bool,
    caps_lock: bool,
}

static STATE: SpinMutex<KeyboardState> = SpinMutex::new(KeyboardState {
    lshift: false,
    rshift: false,
    caps_lock: false,
});

pub fn pop_key() -> Option<char> {
    KEY_QUEUE.lock().pop()
}

pub fn handle_scancode(scancode: u8) {
    let mut state = STATE.lock();

    // Check for Shift press/release
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
        _ => {}
    }

    // Ignore key release events (scancodes with high bit set)
    if scancode >= 0x80 {
        return;
    }

    let shift = state.lshift || state.rshift;
    let uppercase = shift ^ state.caps_lock;

    let character = match scancode {
        0x01 => '\x1b', // Esc
        0x02 => if shift { '!' } else { '1' },
        0x03 => if shift { '@' } else { '2' },
        0x04 => if shift { '#' } else { '3' },
        0x05 => if shift { '$' } else { '4' },
        0x06 => if shift { '%' } else { '5' },
        0x07 => if shift { '^' } else { '6' },
        0x08 => if shift { '&' } else { '7' },
        0x09 => if shift { '*' } else { '8' },
        0x0A => if shift { '(' } else { '9' },
        0x0B => if shift { ')' } else { '0' },
        0x0C => if shift { '_' } else { '-' },
        0x0D => if shift { '+' } else { '=' },
        0x0E => '\x08', // Backspace
        0x0F => '\t',
        0x10 => if uppercase { 'Q' } else { 'q' },
        0x11 => if uppercase { 'W' } else { 'w' },
        0x12 => if uppercase { 'E' } else { 'e' },
        0x13 => if uppercase { 'R' } else { 'r' },
        0x14 => if uppercase { 'T' } else { 't' },
        0x15 => if uppercase { 'Y' } else { 'y' },
        0x16 => if uppercase { 'U' } else { 'u' },
        0x17 => if uppercase { 'I' } else { 'i' },
        0x18 => if uppercase { 'O' } else { 'o' },
        0x19 => if uppercase { 'P' } else { 'p' },
        0x1A => if shift { '{' } else { '[' },
        0x1B => if shift { '}' } else { ']' },
        0x1C => '\n', // Enter
        0x1E => if uppercase { 'A' } else { 'a' },
        0x1F => if uppercase { 'S' } else { 's' },
        0x20 => if uppercase { 'D' } else { 'd' },
        0x21 => if uppercase { 'F' } else { 'f' },
        0x22 => if uppercase { 'G' } else { 'g' },
        0x23 => if uppercase { 'H' } else { 'h' },
        0x24 => if uppercase { 'J' } else { 'j' },
        0x25 => if uppercase { 'K' } else { 'k' },
        0x26 => if uppercase { 'L' } else { 'l' },
        0x27 => if shift { ':' } else { ';' },
        0x28 => if shift { '"' } else { '\'' },
        0x29 => if shift { '~' } else { '`' },
        0x2B => if shift { '|' } else { '\\' },
        0x2C => if uppercase { 'Z' } else { 'z' },
        0x2D => if uppercase { 'X' } else { 'x' },
        0x2E => if uppercase { 'C' } else { 'c' },
        0x2F => if uppercase { 'V' } else { 'v' },
        0x30 => if uppercase { 'B' } else { 'b' },
        0x31 => if uppercase { 'N' } else { 'n' },
        0x32 => if uppercase { 'M' } else { 'm' },
        0x33 => if shift { '<' } else { ',' },
        0x34 => if shift { '>' } else { '.' },
        0x35 => if shift { '?' } else { '/' },
        0x39 => ' ', // Space
        _ => return,
    };

    KEY_QUEUE.lock().push(character);
}
