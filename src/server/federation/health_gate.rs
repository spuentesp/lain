//! Per-repo health + startup-hold, decided atomically.
//!
//! `RepoIndex` used to keep `health` (an `RwLock`) and `hold_ready` (an
//! `AtomicBool`) separately. `mark_ready` (end of an indexing pass) read the
//! hold and then wrote health; `hold_ready(false)` stored the flag, read
//! health, then wrote health. Interleaved, the releaser promoted the repo to
//! `Ready` and the indexer's stale read of `hold = true` then overwrote it
//! with `Indexing`: a repo stuck `Indexing` after its hold was released, until
//! some later re-index (`docs/formal/HoldGate.tla`, `NotStuck`, 5 steps).
//!
//! Both flag and health now live under ONE lock, so each decision is a single
//! critical section. Primitives come from `crate::sync` so loom explores it.
use crate::federation::health::RepoHealth;
use crate::sync::Mutex;

#[derive(Debug)]
struct Gate {
    health: RepoHealth,
    hold: bool,
}

#[derive(Debug)]
pub(crate) struct HealthGate(Mutex<Gate>);

impl HealthGate {
    pub(crate) fn new(initial: RepoHealth) -> Self {
        Self(Mutex::new(Gate {
            health: initial,
            hold: false,
        }))
    }

    pub(crate) fn health(&self) -> RepoHealth {
        self.0.lock().health
    }

    pub(crate) fn set_health(&self, health: RepoHealth) {
        self.0.lock().health = health;
    }

    /// End of a successful indexing pass: `Ready`, unless the startup hold is on.
    pub(crate) fn mark_ready(&self) {
        let mut g = self.0.lock();
        g.health = if g.hold {
            RepoHealth::Indexing
        } else {
            RepoHealth::Ready
        };
    }

    /// Hold (`true`) or release (`false`) readiness. Releasing promotes a repo
    /// whose pass already finished while it was held. `has_indexed` is
    /// evaluated inside the critical section so the promotion cannot act on a
    /// stale answer.
    pub(crate) fn set_hold(&self, hold: bool, has_indexed: impl FnOnce() -> bool) {
        let mut g = self.0.lock();
        g.hold = hold;
        if !hold && g.health == RepoHealth::Indexing && has_indexed() {
            g.health = RepoHealth::Ready;
        }
    }
}

#[cfg(test)]
#[path = "health_gate_verification.rs"]
mod verification;

#[cfg(kani)]
#[path = "health_gate_kani.rs"]
mod kani_proofs;
