use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

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
