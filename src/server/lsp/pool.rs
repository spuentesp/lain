//! LSP pool for parallel language-server communication.
//!
//! `LspPool` is a `Vec<Arc<AsyncMutex<LspMultiplexer>>>` with a
//! round-robin counter for distributing tool calls across multiplexers.
//! Construction (`new`), the install entry point (`install_servers`),
//! the prewarm sentinel (`pick_prewarm_sentinel`), and the per-call
//! multiplexer selector (`next`) live here. The `LspMultiplexer`
//! itself — and every type/function only used inside one — stays in
//! `mod.rs`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

use super::{LspMultiplexer, PrewarmOutcome};
use crate::error::LainError;

pub struct LspPool {
    multiplexers: Vec<Arc<AsyncMutex<LspMultiplexer>>>,
    next: AtomicUsize,
}

impl Clone for LspPool {
    fn clone(&self) -> Self {
        // `AtomicUsize` isn't `Clone` so we can't `#[derive(Clone)]`, but
        // every clone should share the round-robin counter (a freshly
        // zeroed counter would split the multiplexer pool across clones
        // and starve some multiplexers). The pool is intended to be cloned
        // for read-only sharing, so pointing at the original counter is
        // correct: it's a stateless index, not a per-clone state.
        let next = AtomicUsize::new(self.next.load(Ordering::Relaxed));
        LspPool {
            multiplexers: self.multiplexers.clone(),
            next,
        }
    }
}

impl LspPool {
    pub fn new(
        workspace: &Path,
        size: usize,
        runtime: &crate::tuning::RuntimeConfig,
    ) -> Result<Self, LainError> {
        let mut multiplexers = Vec::with_capacity(size);
        for _ in 0..size {
            multiplexers.push(Arc::new(AsyncMutex::new(LspMultiplexer::new(
                workspace, runtime,
            )?)));
        }
        Ok(Self {
            multiplexers,
            next: AtomicUsize::new(0),
        })
    }

    /// Get next multiplexer in round-robin fashion
    pub fn next(&self) -> Arc<AsyncMutex<LspMultiplexer>> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.multiplexers.len();
        Arc::clone(&self.multiplexers[idx])
    }

    /// Number of multiplexers in the pool. Read-only accessor
    /// used by the cold-boot prewarm loop to log a parallelism
    /// breakdown — `unique_exts.len()` round-robin'd across this
    /// size, with each multiplexer serialising its share on the
    /// inner `AsyncMutex`.
    pub fn size(&self) -> usize {
        self.multiplexers.len()
    }

    /// Snapshot prewarm outcomes from every multiplexer in the pool
    /// and merge them into a single map keyed by LSP binary name.
    ///
    /// Used by `/health` (and by `doctor --json`) to surface the
    /// cold-boot warm-up state without binding an agent to a
    /// specific multiplexer. Because prewarm happens at most once
    /// per binary in any single process lifetime (the prewarm
    /// state is monotonically inserted), the merged map is
    /// well-defined: a binary that prewarmed on two different
    /// multiplexers would conflict, but `prewarm_server` is the
    /// only writer and it routes through `ensure_server` keyed on
    /// binary, so that contention never happens in practice.
    ///
    /// Async because the multiplexers behind the pool are guarded
    /// by `Arc<AsyncMutex<LspMultiplexer>>`. The lock hold is
    /// bounded by the size of each per-mux `prewarm_state` (small),
    /// so a /health handler call doesn't block long.
    pub async fn aggregate_prewarm_outcomes(&self) -> HashMap<String, PrewarmOutcome> {
        let mut merged = HashMap::new();
        for mplex in &self.multiplexers {
            let guard = mplex.lock().await;
            for (binary, outcome) in guard.prewarm_outcomes() {
                merged.insert(binary.clone(), outcome.clone());
            }
        }
        merged
    }

    /// Shutdown all multiplexers in the pool
    pub async fn shutdown_all(&self) {
        for m in &self.multiplexers {
            m.lock().await.shutdown().await;
        }
    }
}
