//! Synchronisation primitives, switchable to `loom` for model checking.
//!
//! Modules whose concurrency protocol is verified with loom
//! (`RUSTFLAGS="--cfg lain_loom" cargo test --lib loom_`) import their `Mutex`
//! and atomics from here instead of `parking_lot` / `std::sync::atomic`.
//! Production builds re-export the real primitives: zero cost, same API.

#[cfg(not(lain_loom))]
pub use parking_lot::Mutex;
#[cfg(not(lain_loom))]
pub use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// `parking_lot`-shaped wrapper over `loom::sync::Mutex` (no poisoning).
#[cfg(lain_loom)]
#[derive(Debug)]
pub struct Mutex<T>(loom::sync::Mutex<T>);

#[cfg(lain_loom)]
impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self(loom::sync::Mutex::new(value))
    }

    pub fn lock(&self) -> loom::sync::MutexGuard<'_, T> {
        self.0.lock().expect("loom mutex is never poisoned")
    }
}

#[cfg(lain_loom)]
pub use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
