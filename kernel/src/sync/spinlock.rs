use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "mm-lock-debug")]
use core::sync::atomic::AtomicUsize;

#[cfg(feature = "mm-lock-debug")]
static MM_LOCK_DEPTH: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "mm-lock-debug")]
pub fn mm_lock_depth() -> usize {
    MM_LOCK_DEPTH.load(Ordering::Acquire)
}

/// A lightweight spinlock mutex suitable for bare-metal `#![no_std]` environments.
pub struct SpinMutex<T> {
    lock: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinMutex<T> {}
unsafe impl<T: Send> Send for SpinMutex<T> {}

pub struct MutexGuard<'a, T: 'a> {
    lock: &'a AtomicBool,
    data: &'a mut T,
}

/// Spinlock for MM state that may be touched from interrupt context. The
/// previous interrupt-enable state is restored when the guard is dropped.
pub struct IrqSpinMutex<T> {
    lock: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for IrqSpinMutex<T> {}
unsafe impl<T: Send> Send for IrqSpinMutex<T> {}

pub struct IrqMutexGuard<'a, T: 'a> {
    lock: &'a AtomicBool,
    data: &'a mut T,
    interrupts_enabled: bool,
}

impl<T> IrqSpinMutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            lock: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn try_lock(&self) -> Option<IrqMutexGuard<'_, T>> {
        let interrupts_enabled = crate::arch::x86_64::cpu::interrupts_enabled();
        crate::arch::x86_64::cpu::cli();
        match self
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => Some(IrqMutexGuard {
                lock: &self.lock,
                data: unsafe { &mut *self.data.get() },
                interrupts_enabled,
            })
            .map(|guard| {
                #[cfg(feature = "mm-lock-debug")]
                MM_LOCK_DEPTH.fetch_add(1, Ordering::AcqRel);
                guard
            }),
            Err(_) => {
                if interrupts_enabled {
                    crate::arch::x86_64::cpu::sti();
                }
                None
            }
        }
    }

    pub fn lock(&self) -> IrqMutexGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            core::hint::spin_loop();
        }
    }
}

impl<'a, T> Deref for IrqMutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.data
    }
}
impl<'a, T> DerefMut for IrqMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}
impl<'a, T> Drop for IrqMutexGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.store(false, Ordering::Release);
        #[cfg(feature = "mm-lock-debug")]
        MM_LOCK_DEPTH.fetch_sub(1, Ordering::AcqRel);
        if self.interrupts_enabled {
            crate::arch::x86_64::cpu::sti();
        }
    }
}

impl<T> SpinMutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            lock: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard {
                lock: &self.lock,
                data: unsafe { &mut *self.data.get() },
            })
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_lock() {
                return guard;
            }
            core::hint::spin_loop();
        }
    }

    pub fn reset(&self) {
        self.lock.store(false, Ordering::Relaxed);
    }

    pub fn is_locked(&self) -> bool {
        self.lock.load(Ordering::Acquire)
    }
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.store(false, Ordering::Release);
    }
}
