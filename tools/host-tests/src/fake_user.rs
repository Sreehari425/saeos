//! A fake user address space for host tests: one contiguous mapped region,
//! with the same range rules as the kernel. Anything outside it is EFAULT,
//! like an unmapped page.

use crate::fs::Errno;
use crate::uaccess::{UserMemory, check_range};
use std::cell::RefCell;

pub struct FakeUser {
    pub base: u64,
    pub bytes: RefCell<Vec<u8>>,
}

impl FakeUser {
    pub fn new(base: u64, length: usize) -> Self {
        Self {
            base,
            bytes: RefCell::new(vec![0; length]),
        }
    }

    fn offset(&self, address: u64, length: usize) -> Result<usize, Errno> {
        check_range(address, length)?;
        let offset = address.checked_sub(self.base).ok_or(Errno::EFAULT)? as usize;
        if offset + length > self.bytes.borrow().len() {
            return Err(Errno::EFAULT);
        }
        Ok(offset)
    }
}

impl UserMemory for FakeUser {
    fn read(&self, address: u64, buffer: &mut [u8]) -> Result<(), Errno> {
        let offset = self.offset(address, buffer.len())?;
        buffer.copy_from_slice(&self.bytes.borrow()[offset..offset + buffer.len()]);
        Ok(())
    }

    fn write(&self, address: u64, data: &[u8]) -> Result<(), Errno> {
        let offset = self.offset(address, data.len())?;
        self.bytes.borrow_mut()[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}
