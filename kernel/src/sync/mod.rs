pub mod spinlock;

#[cfg(feature = "mm-lock-debug")]
pub use spinlock::mm_lock_depth;
pub use spinlock::{IrqMutexGuard, IrqSpinMutex, MutexGuard, SpinMutex};
