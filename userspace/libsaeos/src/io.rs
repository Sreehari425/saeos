//! Standard streams and files.
//!
//! `print!`/`println!` go to a line-buffered stdout (flushed on newline,
//! when full, and at exit); `eprint!`/`eprintln!` write stderr directly.
//! [`stdin_read_line`] reads whole lines whatever fd 0 is (a terminal hands
//! out one line per read; a file needs buffering).

use crate::errno::{Errno, Result, check};
use crate::lines::LineReader;
use crate::raw;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::fmt::{self, Write};

/// Write all of `bytes` to `fd` (looping over short writes).
pub fn write_all(fd: u32, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let written = check(raw::write(fd, bytes))? as usize;
        if written == 0 {
            return Err(Errno::EIO);
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

/// Process-wide state for the single-threaded runtime.
struct Global<T>(UnsafeCell<T>);
unsafe impl<T> Sync for Global<T> {}

struct Stdout {
    buffer: [u8; 1024],
    length: usize,
}

impl Stdout {
    fn flush(&mut self) {
        let _ = write_all(1, &self.buffer[..self.length]);
        self.length = 0;
    }
}

impl Write for Stdout {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for &byte in text.as_bytes() {
            if self.length == self.buffer.len() {
                self.flush();
            }
            self.buffer[self.length] = byte;
            self.length += 1;
            if byte == b'\n' {
                self.flush();
            }
        }
        Ok(())
    }
}

static STDOUT: Global<Stdout> = Global(UnsafeCell::new(Stdout {
    buffer: [0; 1024],
    length: 0,
}));

#[doc(hidden)]
pub fn _print(arguments: fmt::Arguments) {
    let _ = unsafe { &mut *STDOUT.0.get() }.write_fmt(arguments);
}

/// Send buffered stdout now (`exit` does this).
pub fn flush() {
    unsafe { &mut *STDOUT.0.get() }.flush();
}

struct Stderr;

impl Write for Stderr {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        write_all(2, text.as_bytes()).map_err(|_| fmt::Error)
    }
}

#[doc(hidden)]
pub fn _eprint(arguments: fmt::Arguments) {
    // Anything already printed comes first.
    flush();
    let _ = Stderr.write_fmt(arguments);
}

#[macro_export]
macro_rules! print {
    ($($argument:tt)*) => ($crate::io::_print(format_args!($($argument)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($argument:tt)*) => ($crate::io::_print(format_args!("{}\n", format_args!($($argument)*))));
}

#[macro_export]
macro_rules! eprint {
    ($($argument:tt)*) => ($crate::io::_eprint(format_args!($($argument)*)));
}

#[macro_export]
macro_rules! eprintln {
    () => ($crate::eprint!("\n"));
    ($($argument:tt)*) => ($crate::io::_eprint(format_args!("{}\n", format_args!($($argument)*))));
}

static STDIN: Global<LineReader> = Global(UnsafeCell::new(LineReader::new()));

/// Append the next line of stdin (with its newline) to `line`; returns its
/// length, 0 at end of file. Invalid UTF-8 is replaced.
pub fn stdin_read_line(line: &mut String) -> Result<usize> {
    flush();
    let mut bytes = Vec::new();
    let count = unsafe { &mut *STDIN.0.get() }.read_line(&mut bytes, |buffer| {
        check(raw::read(0, buffer)).map(|count| count as usize)
    })?;
    line.push_str(&String::from_utf8_lossy(&bytes));
    Ok(count)
}

/// `path` with a NUL terminator, for the kernel.
pub(crate) fn c_path(path: &str) -> Result<Vec<u8>> {
    if path.as_bytes().contains(&0) {
        return Err(Errno::EINVAL);
    }
    let mut bytes = Vec::with_capacity(path.len() + 1);
    bytes.extend_from_slice(path.as_bytes());
    bytes.push(0);
    Ok(bytes)
}

/// An open file, closed on drop.
pub struct File {
    fd: u32,
}

pub enum SeekFrom {
    Start(u64),
    Current(i64),
    End(i64),
}

impl File {
    fn open_with(path: &str, flags: u64) -> Result<Self> {
        let fd = check(raw::open(&c_path(path)?, flags))?;
        Ok(Self { fd: fd as u32 })
    }

    /// Open for reading.
    pub fn open(path: &str) -> Result<Self> {
        Self::open_with(path, raw::O_RDONLY)
    }

    /// Create (or truncate) for writing.
    pub fn create(path: &str) -> Result<Self> {
        Self::open_with(path, raw::O_WRONLY | raw::O_CREAT | raw::O_TRUNC)
    }

    /// Open for appending, creating it if needed.
    pub fn append(path: &str) -> Result<Self> {
        const O_APPEND: u64 = 0o2000;
        Self::open_with(path, raw::O_WRONLY | raw::O_CREAT | O_APPEND)
    }

    pub fn fd(&self) -> u32 {
        self.fd
    }

    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        check(raw::read(self.fd, buffer)).map(|count| count as usize)
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<usize> {
        check(raw::write(self.fd, bytes)).map(|count| count as usize)
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        write_all(self.fd, bytes)
    }

    pub fn read_to_end(&mut self, out: &mut Vec<u8>) -> Result<usize> {
        let start = out.len();
        let mut chunk = [0u8; 4096];
        loop {
            match self.read(&mut chunk)? {
                0 => return Ok(out.len() - start),
                count => out.extend_from_slice(&chunk[..count]),
            }
        }
    }

    pub fn read_to_string(&mut self, out: &mut String) -> Result<usize> {
        let mut bytes = Vec::new();
        let count = self.read_to_end(&mut bytes)?;
        out.push_str(&String::from_utf8_lossy(&bytes));
        Ok(count)
    }

    pub fn seek(&mut self, position: SeekFrom) -> Result<u64> {
        let (offset, whence) = match position {
            SeekFrom::Start(offset) => (offset as i64, 0),
            SeekFrom::Current(offset) => (offset, 1),
            SeekFrom::End(offset) => (offset, 2),
        };
        check(raw::lseek(self.fd, offset, whence))
    }
}

impl Write for File {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_all(text.as_bytes()).map_err(|_| fmt::Error)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        raw::close(self.fd);
    }
}
