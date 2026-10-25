//! Fixed-capacity byte ring for the kernel log. Dependency free, so the host
//! model (`tools/mm-model`) can test it directly.

pub struct Ring<const CAPACITY: usize> {
    bytes: [u8; CAPACITY],
    /// Next write position.
    head: usize,
    /// Bytes currently stored (at most `CAPACITY`).
    len: usize,
}

impl<const CAPACITY: usize> Default for Ring<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const CAPACITY: usize> Ring<CAPACITY> {
    pub const fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            head: 0,
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append `data`, overwriting the oldest bytes once full.
    pub fn push(&mut self, data: &[u8]) {
        // Only the newest CAPACITY bytes can survive.
        let data = &data[data.len().saturating_sub(CAPACITY)..];
        let first = data.len().min(CAPACITY - self.head);
        self.bytes[self.head..self.head + first].copy_from_slice(&data[..first]);
        self.bytes[..data.len() - first].copy_from_slice(&data[first..]);
        self.head = (self.head + data.len()) % CAPACITY;
        self.len = (self.len + data.len()).min(CAPACITY);
    }

    /// The stored bytes, oldest first, as up to two slices.
    pub fn as_slices(&self) -> (&[u8], &[u8]) {
        let start = (self.head + CAPACITY - self.len) % CAPACITY;
        if start + self.len <= CAPACITY {
            (&self.bytes[start..start + self.len], &[])
        } else {
            (&self.bytes[start..], &self.bytes[..self.head])
        }
    }

    /// Whether `needle` occurs in the stored bytes (including across the
    /// wrap point).
    pub fn contains(&self, needle: &[u8]) -> bool {
        let (first, second) = self.as_slices();
        let at = |index: usize| {
            if index < first.len() {
                first[index]
            } else {
                second[index - first.len()]
            }
        };
        if needle.len() > self.len {
            return false;
        }
        (0..=self.len - needle.len())
            .any(|start| (0..needle.len()).all(|offset| at(start + offset) == needle[offset]))
    }
}

impl<const CAPACITY: usize> core::fmt::Write for Ring<CAPACITY> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.push(text.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Ring;

    fn contents<const N: usize>(ring: &Ring<N>) -> Vec<u8> {
        let (first, second) = ring.as_slices();
        [first, second].concat()
    }

    #[test]
    fn keeps_everything_until_full() {
        let mut ring = Ring::<8>::new();
        ring.push(b"abc");
        ring.push(b"de");
        assert_eq!(contents(&ring), b"abcde");
        assert_eq!(ring.len(), 5);
    }

    #[test]
    fn overwrites_the_oldest_bytes_when_wrapping() {
        let mut ring = Ring::<8>::new();
        ring.push(b"abcdef");
        ring.push(b"ghij");
        assert_eq!(contents(&ring), b"cdefghij");
        assert!(ring.contains(b"fgh"), "match across the wrap point");
        assert!(!ring.contains(b"ab"), "overwritten bytes are gone");
    }

    #[test]
    fn oversized_writes_keep_the_newest_tail() {
        let mut ring = Ring::<4>::new();
        ring.push(b"x");
        ring.push(b"0123456789");
        assert_eq!(contents(&ring), b"6789");
        assert_eq!(ring.len(), 4);
    }

    #[test]
    fn many_small_writes_match_a_model() {
        let mut ring = Ring::<37>::new();
        let mut model = Vec::new();
        for step in 0..500u32 {
            let chunk: Vec<u8> = (0..(step % 11)).map(|i| (step + i) as u8).collect();
            ring.push(&chunk);
            model.extend_from_slice(&chunk);
            let tail = &model[model.len().saturating_sub(37)..];
            assert_eq!(contents(&ring), tail, "step {step}");
        }
    }
}
