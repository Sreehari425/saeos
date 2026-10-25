//! Reading lines from a file descriptor with a buffer, the way `fgets` or
//! Rust's `BufRead::read_line` do. Pure over a `read` function, so the
//! host model tests it.

use alloc::vec::Vec;

pub const BUFFER: usize = 4096;

#[derive(Default)]
pub struct LineReader {
    buffer: Vec<u8>,
    /// `buffer[start..]` has not been returned yet.
    start: usize,
}

impl LineReader {
    pub const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            start: 0,
        }
    }

    /// Append the next line (with its `\n`, if it had one) to `line`;
    /// returns its length, 0 at end of file. `read` fills a buffer and
    /// returns the bytes read (0 at end of file) or an error code.
    pub fn read_line<E>(
        &mut self,
        line: &mut Vec<u8>,
        mut read: impl FnMut(&mut [u8]) -> Result<usize, E>,
    ) -> Result<usize, E> {
        let before = line.len();
        loop {
            let pending = &self.buffer[self.start..];
            if let Some(newline) = pending.iter().position(|&byte| byte == b'\n') {
                line.extend_from_slice(&pending[..=newline]);
                self.start += newline + 1;
                return Ok(line.len() - before);
            }
            line.extend_from_slice(pending);
            self.buffer.clear();
            self.start = 0;
            self.buffer.resize(BUFFER, 0);
            let count = match read(&mut self.buffer) {
                Ok(count) => count,
                Err(error) => {
                    self.buffer.clear();
                    return Err(error);
                }
            };
            self.buffer.truncate(count);
            if count == 0 {
                return Ok(line.len() - before);
            }
        }
    }
}
