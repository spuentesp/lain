//! Attribution — background attribution watcher backend.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<AttributionState>` in PR 3.7b and forward `attribution()` through.

use crate::server::attribution::AttributionBackend;
use std::sync::Arc;

/// Strategy used by the background attribution watcher to map a
/// workspace path to the PID that wrote it.
pub struct AttributionState {
    pub(crate) attribution: Arc<dyn AttributionBackend>,
}

impl AttributionState {
    pub fn new(attribution: Arc<dyn AttributionBackend>) -> Self {
        Self { attribution }
    }

    /// Borrowed handle to the [`AttributionBackend`] this server was
    /// constructed with. Surfaced for diagnostics and tests; the
    /// background [`AttributionWatcher`] already shares the same `Arc`.
    pub fn attribution(&self) -> &Arc<dyn AttributionBackend> {
        &self.attribution
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::attribution::NoopBackend;

    #[test]
    fn exposes_the_backend_it_was_built_with() {
        let backend: Arc<dyn AttributionBackend> = Arc::new(NoopBackend);
        let handle = AttributionState::new(Arc::clone(&backend));
        assert!(Arc::ptr_eq(handle.attribution(), &backend));
    }
}
