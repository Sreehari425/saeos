//! DMA buffers: memory a device reads and writes by itself.
//!
//! ```text
//! DmaBuffer::new(len, constraints)
//!   plan:   the smallest buddy block that holds `len`, meets the alignment
//!           and does not cross the boundary          (pure, host-tested)
//!   frames: frame::alloc_contiguous_in_range(order, below the limit), zeroed
//!   CPU:    as_slice / as_mut_slice    (the direct map)
//!   device: device_address()           (a DmaAddress, never a VirtAddr)
//! ```
//!
//! A device does not use the CPU's page tables, so it must be given the
//! *physical* address. Handing it a virtual address is the classic DMA bug:
//! the device writes into whatever RAM happens to sit there. The device
//! side is therefore its own type, [`DmaAddress`], with no conversion from
//! a virtual address. Without an IOMMU it is the physical address.
//!
//! Devices limit what they can reach (a 32-bit device: below 4 GiB) and
//! some cannot cross certain boundaries in one transfer (64 KiB for ISA
//! DMA). Buddy blocks are naturally aligned to their size, so a block no
//! larger than a power-of-two boundary can never cross it, and alignment
//! up to the block size comes for free.
//!
//! x86 DMA is cache-coherent: devices snoop the CPU caches, so buffers stay
//! in the ordinary write-back direct map (uncached would be slower and
//! would hide ordering bugs rather than fix them). [`DmaBuffer::sync_for_device`]
//! and [`DmaBuffer::sync_for_cpu`] are fences that mark who owns the buffer.
//! The rule: the CPU leaves a buffer alone while the device owns it, and a
//! buffer is only dropped once the device has stopped using it.

const PAGE_SIZE: u64 = 4096;
/// The buddy allocator's largest block (`2^MAX_ORDER` pages).
const MAX_ORDER: usize = 18;

/// What a device can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaConstraints {
    /// The buffer must end at or below this physical address.
    pub limit: u64,
    /// Required alignment of the start (a power of two).
    pub alignment: u64,
    /// The buffer must not cross a multiple of this (a power of two).
    pub boundary: Option<u64>,
}

impl DmaConstraints {
    /// A 32-bit device.
    pub const BELOW_4G: Self = Self::below(1 << 32);

    /// A device that reaches addresses below `limit`.
    pub const fn below(limit: u64) -> Self {
        Self {
            limit,
            alignment: PAGE_SIZE,
            boundary: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// Zero length, or more than the largest block.
    TooLarge,
    /// The buffer is larger than the boundary it must not cross.
    CrossesBoundary,
    /// A limit of 0, or an alignment or boundary that is not a power of two.
    BadConstraints,
    OutOfMemory,
}

/// The buddy order (block of `2^order` pages) for a buffer.
pub fn plan(len: usize, constraints: DmaConstraints) -> Result<usize, DmaError> {
    let DmaConstraints {
        limit,
        alignment,
        boundary,
    } = constraints;
    if limit == 0
        || !alignment.is_power_of_two()
        || boundary.is_some_and(|boundary| !boundary.is_power_of_two())
    {
        return Err(DmaError::BadConstraints);
    }
    if len == 0 {
        return Err(DmaError::TooLarge);
    }
    // A block's start is aligned to its size.
    let bytes = (len as u64).max(alignment).max(PAGE_SIZE);
    let pages = bytes.div_ceil(PAGE_SIZE).next_power_of_two();
    let order = pages.trailing_zeros() as usize;
    if order > MAX_ORDER {
        return Err(DmaError::TooLarge);
    }
    // The block starts at a multiple of its size, so at a multiple of any
    // smaller power-of-two boundary too: the buffer stays in one boundary
    // window exactly when it is no longer than the boundary.
    if boundary.is_some_and(|boundary| len as u64 > boundary) {
        return Err(DmaError::CrossesBoundary);
    }
    Ok(order)
}

/// Whether `start..start + len` stays within one `boundary` window.
pub fn within_boundary(start: u64, len: u64, boundary: u64) -> bool {
    len == 0 || start / boundary == (start + len - 1) / boundary
}

/// An address as a device sees it. With no IOMMU, the physical address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DmaAddress(pub u64);

#[cfg(not(host_model))]
mod kernel {
    use super::{DmaAddress, DmaConstraints, DmaError, plan};
    use crate::mm::frame::{self, AllocationConstraints, AllocationOrder, PhysAddr, PhysFrame};
    use core::sync::atomic::{Ordering, fence};

    /// Physically contiguous memory a device can reach. Freed on drop: stop
    /// the device first.
    pub struct DmaBuffer {
        frame: PhysFrame,
        order: AllocationOrder,
        len: usize,
    }

    impl DmaBuffer {
        /// A zeroed buffer of `len` bytes, so no old kernel data reaches a
        /// device.
        pub fn new(len: usize, constraints: DmaConstraints) -> Result<Self, DmaError> {
            let order = AllocationOrder(plan(len, constraints)?);
            let frame = frame::alloc_contiguous_in_range(
                order,
                AllocationConstraints {
                    minimum: None,
                    maximum: Some(PhysAddr(constraints.limit)),
                },
            )
            .map_err(|_| DmaError::OutOfMemory)?;
            let buffer = Self { frame, order, len };
            unsafe { core::ptr::write_bytes(buffer.pointer(), 0, order.bytes() as usize) };
            Ok(buffer)
        }

        fn pointer(&self) -> *mut u8 {
            crate::mm::paging::phys_to_virt(PhysAddr(self.frame.0)).0 as *mut u8
        }

        /// Where the device reads or writes it.
        pub fn device_address(&self) -> DmaAddress {
            DmaAddress(self.frame.0)
        }

        pub fn len(&self) -> usize {
            self.len
        }

        pub fn is_empty(&self) -> bool {
            self.len == 0
        }

        pub fn as_slice(&self) -> &[u8] {
            unsafe { core::slice::from_raw_parts(self.pointer(), self.len) }
        }

        pub fn as_mut_slice(&mut self) -> &mut [u8] {
            unsafe { core::slice::from_raw_parts_mut(self.pointer(), self.len) }
        }

        /// Before handing the buffer to the device: the CPU's writes are
        /// done before the device is told to start.
        pub fn sync_for_device(&self) {
            fence(Ordering::SeqCst);
        }

        /// After the device signalled completion: read what it wrote, not
        /// a value from before.
        pub fn sync_for_cpu(&self) {
            fence(Ordering::SeqCst);
        }
    }

    impl Drop for DmaBuffer {
        fn drop(&mut self) {
            frame::release_contiguous(self.frame, self.order);
        }
    }
}

#[cfg(not(host_model))]
pub use kernel::DmaBuffer;
