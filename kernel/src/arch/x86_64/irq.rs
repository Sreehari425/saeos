//! Interrupt-context tracking.
//!
//! Hardware interrupt handlers hold an [`IrqGuard`] for their duration. Code
//! that must never run in interrupt context (heap allocation, anything that
//! may block) checks [`in_interrupt`].

use core::sync::atomic::{AtomicUsize, Ordering};

static DEPTH: AtomicUsize = AtomicUsize::new(0);

/// Marks the current CPU as handling a hardware interrupt until dropped.
pub struct IrqGuard(());

pub fn enter() -> IrqGuard {
    DEPTH.fetch_add(1, Ordering::Relaxed);
    IrqGuard(())
}

impl Drop for IrqGuard {
    fn drop(&mut self) {
        DEPTH.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn in_interrupt() -> bool {
    DEPTH.load(Ordering::Relaxed) != 0
}
