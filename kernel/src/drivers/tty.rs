//! The console's line discipline: canonical ("cooked") input, as a Linux
//! terminal does it by default.
//!
//! ```text
//! keyboard IRQ ─┐                   reader's context (read on /dev/console)
//! serial IRQ  ──┴▶ raw bytes ──▶ LineDiscipline::input ──▶ echo
//!                  (KEY_QUEUE)        │ editing: Backspace/DEL, ^U, ^W, ^C
//!                                      ▼
//!                              completed lines ──▶ read() gets one at a time
//! ```
//!
//! | input            | effect                                              |
//! |------------------|-----------------------------------------------------|
//! | printable        | added to the line and echoed                        |
//! | Tab              | added to the line, echoed as spaces to the next tab stop |
//! | Enter (CR or LF) | completes the line, with `\n`                       |
//! | Backspace, DEL   | erases one character (`\b \b`)                      |
//! | Ctrl-U           | erases the whole line                               |
//! | Ctrl-W           | erases the last word                                |
//! | Ctrl-D           | on an empty line: end of file (read returns 0); otherwise completes the line without `\n` |
//! | Ctrl-C           | discards the line (`^C`); signals come later        |
//!
//! Lines hold at most [`LINE_MAX`] bytes (Linux's `N_TTY_BUF_SIZE`);
//! characters past that are dropped. Editing runs in the reader's context
//! so echo never prints from an interrupt handler. The discipline is pure
//! and tested in the host model.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub const LINE_MAX: usize = 4096;

const BACKSPACE: u8 = 0x08;
const DELETE: u8 = 0x7f;
const CONTROL_C: u8 = 0x03;
const CONTROL_D: u8 = 0x04;
const CONTROL_U: u8 = 0x15;
const CONTROL_W: u8 = 0x17;
const ERASE_ECHO: &[u8] = b"\x08 \x08";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Record {
    /// A completed line and how much of it has been read.
    Line(Vec<u8>, usize),
    EndOfFile,
}

#[derive(Debug, Default)]
pub struct LineDiscipline {
    /// The line being edited.
    line: Vec<u8>,
    /// How many columns each character of `line` was echoed as (a Tab is
    /// echoed as spaces to the next tab stop), so erasing takes back
    /// exactly what was drawn.
    widths: Vec<u8>,
    ready: VecDeque<Record>,
}

impl LineDiscipline {
    pub const fn new() -> Self {
        Self {
            line: Vec::new(),
            widths: Vec::new(),
            ready: VecDeque::new(),
        }
    }

    /// Whether `read` has something to return.
    pub fn has_input(&self) -> bool {
        !self.ready.is_empty()
    }

    /// Erase the last character (a whole UTF-8 sequence). False if empty.
    fn erase_character(&mut self, echo: &mut Vec<u8>) -> bool {
        let Some(mut last) = self.line.pop() else {
            return false;
        };
        // Continuation bytes are 0b10xx_xxxx: drop back to the start byte.
        while last & 0xc0 == 0x80 {
            match self.line.pop() {
                Some(byte) => last = byte,
                None => break,
            }
        }
        for _ in 0..self.widths.pop().unwrap_or(1) {
            echo.extend_from_slice(ERASE_ECHO);
        }
        true
    }

    /// The column the next character is echoed at, counted from the
    /// start of the line.
    fn column(&self) -> usize {
        self.widths.iter().map(|&width| usize::from(width)).sum()
    }

    fn complete(&mut self) {
        let line = core::mem::take(&mut self.line);
        self.widths.clear();
        self.ready.push_back(Record::Line(line, 0));
    }

    /// Process one input byte; what to echo is appended to `echo`.
    pub fn input(&mut self, byte: u8, echo: &mut Vec<u8>) {
        match byte {
            b'\r' | b'\n' => {
                self.line.push(b'\n');
                echo.push(b'\n');
                self.complete();
            }
            BACKSPACE | DELETE => {
                self.erase_character(echo);
            }
            CONTROL_U => while self.erase_character(echo) {},
            CONTROL_W => {
                while self.line.last() == Some(&b' ') {
                    self.erase_character(echo);
                }
                while self.line.last().is_some_and(|&byte| byte != b' ') {
                    self.erase_character(echo);
                }
            }
            CONTROL_D if self.line.is_empty() => self.ready.push_back(Record::EndOfFile),
            CONTROL_D => self.complete(),
            CONTROL_C => {
                self.line.clear();
                self.widths.clear();
                echo.extend_from_slice(b"^C\n");
            }
            // Text, keeping room for the newline that completes the line;
            // past that, characters are dropped. A Tab is echoed as spaces
            // to the next tab stop (every 8 columns from the line start).
            b'\t' if self.line.len() < LINE_MAX - 1 => {
                let width = 8 - self.column() % 8;
                self.line.push(byte);
                self.widths.push(width as u8);
                echo.resize(echo.len() + width, b' ');
            }
            0x20.. if self.line.len() < LINE_MAX - 1 => {
                self.line.push(byte);
                // UTF-8 continuation bytes add no column.
                if byte & 0xc0 != 0x80 {
                    self.widths.push(1);
                }
                echo.push(byte);
            }
            // Other control characters are ignored.
            _ => {}
        }
    }

    /// Copy from the oldest completed line into `buffer`: never more than
    /// one line per call, as a canonical-mode read. `Some(0)` is end of
    /// file; `None` means no line is complete yet.
    pub fn read(&mut self, buffer: &mut [u8]) -> Option<usize> {
        match self.ready.front_mut()? {
            Record::EndOfFile => {
                self.ready.pop_front();
                Some(0)
            }
            Record::Line(line, consumed) => {
                let count = (line.len() - *consumed).min(buffer.len());
                buffer[..count].copy_from_slice(&line[*consumed..*consumed + count]);
                *consumed += count;
                if *consumed == line.len() {
                    self.ready.pop_front();
                }
                Some(count)
            }
        }
    }
}

#[cfg(not(host_model))]
mod kernel {
    use super::LineDiscipline;
    use crate::drivers::keyboard;
    use crate::sync::IrqSpinMutex;
    use alloc::vec::Vec;

    static CONSOLE: IrqSpinMutex<LineDiscipline> = IrqSpinMutex::new(LineDiscipline::new());

    /// `read` on `/dev/console`: one line (or the part of it that fits),
    /// blocking until a line is complete. A zero-length read returns at
    /// once.
    pub fn read_console(buffer: &mut [u8]) -> usize {
        if buffer.is_empty() {
            return 0;
        }
        loop {
            let mut echo = Vec::new();
            let result = {
                let mut console = CONSOLE.lock();
                while let Some(byte) = keyboard::pop_byte() {
                    console.input(byte, &mut echo);
                }
                console.read(buffer)
            };
            if !echo.is_empty() {
                crate::print!("{}", alloc::string::String::from_utf8_lossy(&echo));
            }
            if let Some(count) = result {
                return count;
            }
            // Lost-wakeup-free: input arriving after the check wakes us.
            crate::scheduler::wait_event(&keyboard::KEY_WAIT, keyboard::has_key);
        }
    }
}

#[cfg(not(host_model))]
pub use kernel::read_console;
