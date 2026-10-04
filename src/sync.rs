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

/// A "needs rebuild" flag with the *clear-before-read* protocol.
///
/// Writers call [`DirtyFlag::mark`] after changing the inputs; the single
/// rebuilder calls [`DirtyFlag::run_if_dirty`], which clears the flag
/// **before** the rebuild reads its inputs. A writer landing mid-rebuild then
/// re-arms the flag and the next call redoes the work. Clearing *after* the
/// rebuild (what the code used to do) overwrites that writer's mark and
/// leaves the derived state permanently stale
/// (`docs/formal/RejoinProtocol.tla`, `NoLostUpdate`).
///
/// The caller is expected to serialise rebuilders (a mutex); writers are
/// lock-free by design.
#[derive(Debug)]
pub struct DirtyFlag(AtomicBool);

impl DirtyFlag {
    pub fn new(dirty: bool) -> Self {
        Self(AtomicBool::new(dirty))
    }

    pub fn mark(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_dirty(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Run `rebuild` if the flag is set. Clears the flag first; restores it
    /// if `rebuild` fails so the change is not silently swallowed.
    pub fn run_if_dirty<E>(&self, rebuild: impl FnOnce() -> Result<(), E>) -> Result<(), E> {
        if !self.is_dirty() {
            return Ok(());
        }
        self.0.store(false, Ordering::Release);
        match rebuild() {
            Ok(()) => Ok(()),
            Err(e) => {
                self.mark();
                Err(e)
            }
        }
    }
}

#[cfg(test)]
#[path = "sync_verification.rs"]
mod verification;
