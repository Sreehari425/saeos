//! The block-device seam for on-disk filesystems.
//!
//! A filesystem written against `BlockDevice` runs unchanged on `RamDisk`
//! (in the host tests and Miri) and later on virtio-blk.

use super::errno::{Errno, FsResult};
use super::lock::Mutex;
use alloc::vec;
use alloc::vec::Vec;

pub trait BlockDevice: Send + Sync {
    fn block_size(&self) -> usize;
    fn block_count(&self) -> u64;
    /// Read block `index` into `buf`, which must be exactly one block.
    fn read_block(&self, index: u64, buf: &mut [u8]) -> FsResult<()>;
    /// Write `buf` (exactly one block) to block `index`.
    fn write_block(&self, index: u64, buf: &[u8]) -> FsResult<()>;
}

/// A block device backed by memory.
pub struct RamDisk {
    block_size: usize,
    blocks: u64,
    data: Mutex<Vec<u8>>,
}

impl RamDisk {
    /// A zero-filled disk. `block_size` must be a power of two of at least
    /// 512 bytes.
    pub fn new(block_size: usize, blocks: u64) -> FsResult<Self> {
        if block_size < 512 || !block_size.is_power_of_two() {
            return Err(Errno::EINVAL);
        }
        let bytes = usize::try_from(blocks)
            .ok()
            .and_then(|blocks| blocks.checked_mul(block_size))
            .ok_or(Errno::EINVAL)?;
        Ok(Self {
            block_size,
            blocks,
            data: Mutex::new(vec![0; bytes]),
        })
    }

    fn range(&self, index: u64, length: usize) -> FsResult<core::ops::Range<usize>> {
        if length != self.block_size {
            return Err(Errno::EINVAL);
        }
        if index >= self.blocks {
            return Err(Errno::EIO);
        }
        let start = index as usize * self.block_size;
        Ok(start..start + self.block_size)
    }
}

impl BlockDevice for RamDisk {
    fn block_size(&self) -> usize {
        self.block_size
    }

    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn read_block(&self, index: u64, buf: &mut [u8]) -> FsResult<()> {
        let range = self.range(index, buf.len())?;
        buf.copy_from_slice(&self.data.lock()[range]);
        Ok(())
    }

    fn write_block(&self, index: u64, buf: &[u8]) -> FsResult<()> {
        let range = self.range(index, buf.len())?;
        self.data.lock()[range].copy_from_slice(buf);
        Ok(())
    }
}
