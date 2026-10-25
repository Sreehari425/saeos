//! The filesystem's mutex: the kernel's `SpinMutex` (never an
//! interrupt-disabling lock, so file I/O runs with interrupts enabled), or a
//! `std` mutex with the same interface in the host model.

#[cfg(not(host_model))]
pub use crate::sync::{MutexGuard, SpinMutex as Mutex};

#[cfg(host_model)]
pub struct Mutex<T>(std::sync::Mutex<T>);

#[cfg(host_model)]
pub type MutexGuard<'a, T> = std::sync::MutexGuard<'a, T>;

#[cfg(host_model)]
impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
