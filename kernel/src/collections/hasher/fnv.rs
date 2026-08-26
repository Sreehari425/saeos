use core::hash::{BuildHasher, Hasher};

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

/// 64-bit FNV-1a non-cryptographic hash algorithm.
#[derive(Debug, Clone, Copy)]
pub struct FnvHasher(u64);

impl FnvHasher {
    pub const fn new() -> Self {
        Self(FNV_OFFSET_BASIS)
    }
}

impl Default for FnvHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher for FnvHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= byte as u64;
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildFnvHasher;

impl BuildFnvHasher {
    pub const fn new() -> Self {
        Self
    }
}

impl BuildHasher for BuildFnvHasher {
    type Hasher = FnvHasher;

    #[inline]
    fn build_hasher(&self) -> Self::Hasher {
        FnvHasher::new()
    }
}
