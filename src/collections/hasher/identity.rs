use core::hash::{BuildHasher, Hasher};

/// A pass-through 1-cycle identity hasher for integer keys (PIDs, IRQs, FDs, Inodes).
#[derive(Debug, Clone, Copy, Default)]
pub struct IdentityHasher(u64);

impl IdentityHasher {
    pub const fn new() -> Self {
        Self(0)
    }
}

impl Hasher for IdentityHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut val = 0u64;
        for (i, &b) in bytes.iter().enumerate().take(8) {
            val |= (b as u64) << (i * 8);
        }
        self.0 = val;
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = i as u64;
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.0 = i as u64;
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildIdentityHasher;

impl BuildIdentityHasher {
    pub const fn new() -> Self {
        Self
    }
}

impl BuildHasher for BuildIdentityHasher {
    type Hasher = IdentityHasher;

    #[inline]
    fn build_hasher(&self) -> Self::Hasher {
        IdentityHasher::new()
    }
}
