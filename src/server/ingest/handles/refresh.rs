//! Refresh — sync-status + last-outcome bookkeeping.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<RefreshState>` in PR 3.7b and forward `record_sync`,
//! `last_sync_at`, `last_error`, `record_last_error`, and
//! `static_graph_generation_unix` through.

use crate::server::refresh::{RefreshOutcome, RefreshResult};
use crate::server::sync_status::SyncStatus;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Sync-attempt bookkeeping (last successful sync, last error) plus
/// the snapshot of the most recent re-index outcome. Read by
/// `get_server_status`, `get_health`, and the scoped banner.
pub struct RefreshState {
    pub(crate) last_outcome: Arc<Mutex<RefreshOutcome>>,
    pub(crate) sync_status: SyncStatus,
}

impl RefreshState {
    pub fn new(last_outcome: Arc<Mutex<RefreshOutcome>>, sync_status: SyncStatus) -> Self {
        Self {
            last_outcome,
            sync_status,
        }
    }

    /// Last successful sync time. Updated via [`Self::record_sync`];
    /// consumed by `get_server_status`.
    pub fn last_sync_at(&self) -> SystemTime {
        self.sync_status.last_sync_at()
    }

    /// Most recent ingest/sync error message, if any.
    pub fn last_error(&self) -> Option<String> {
        self.sync_status.last_error()
    }

    /// Mark a sync attempt as successful: clear `last_error` and bump
    /// `last_sync_at` to now. Called by ingest/sync paths that finish
    /// without an error; errors should call [`Self::record_last_error`]
    /// instead.
    pub fn record_sync(&self) {
        self.sync_status.record_ok();
    }

    /// Record an error message from the ingest/sync paths and refresh
    /// `last_sync_at` to the current time so the operator can see when
    /// the last attempt was.
    pub fn record_last_error(&self, msg: impl Into<String>) {
        self.sync_status.record_error(msg);
    }

    /// Static-graph generation as a Unix-epoch seconds value, or `None`
    /// if no successful re-index has happened in this process. Used by
    /// the JSON-RPC `_meta.static_graph_generation` envelope field (P1
    /// #1) so the LLM knows how fresh the static graph is without
    /// needing a separate `list_repos` round-trip.
    pub fn static_graph_generation_unix(&self) -> Option<i64> {
        let outcome = self.last_outcome.lock();
        if !matches!(outcome.result, RefreshResult::Ok) {
            return None;
        }
        outcome
            .started_at
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::refresh::RefreshOutcome;

    #[test]
    fn record_sync_clears_error_and_updates_last_sync_at() {
        let outcome = Arc::new(Mutex::new(RefreshOutcome::skipped()));
        let sync = SyncStatus::new(SystemTime::now() - std::time::Duration::from_secs(1));
        let state = RefreshState::new(outcome, sync);

        state.record_last_error("boom");
        assert_eq!(state.last_error().as_deref(), Some("boom"));

        state.record_sync();
        assert!(state.last_error().is_none());
    }

    #[test]
    fn static_graph_generation_unix_returns_none_when_outcome_not_ok() {
        let outcome = Arc::new(Mutex::new(RefreshOutcome::skipped()));
        let sync = SyncStatus::new(SystemTime::now());
        let state = RefreshState::new(outcome, sync);
        assert_eq!(state.static_graph_generation_unix(), None);
    }

    #[test]
    fn static_graph_generation_unix_returns_unix_secs_when_outcome_ok() {
        let outcome = Arc::new(Mutex::new(RefreshOutcome::ok(
            SystemTime::now() - std::time::Duration::from_secs(5),
        )));
        let sync = SyncStatus::new(SystemTime::now());
        let state = RefreshState::new(outcome, sync);
        let secs = state.static_graph_generation_unix().expect("ok outcome");
        let expected = (SystemTime::now() - std::time::Duration::from_secs(5))
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        // Allow ±2s drift between the `RefreshOutcome::ok` timestamp
        // and the `now()` captured inside the test.
        assert!(
            (secs - expected).abs() <= 2,
            "expected ~{expected}, got {secs}"
        );
    }
}
