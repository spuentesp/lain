//! Re-index timeout + outcome tracking.
//!
//! Step 1 of the staleness fix: make the startup re-index timeout
//! configurable (env var + CLI flag, default 300s) and write the
//! outcome to shared state so `get_health` can surface failures
//! that the model never sees via stderr. Steps 2–4 land separately.

use std::time::{Duration, SystemTime};

/// Result of a single re-index attempt. Stored in `LainServer`'s
/// `last_outcome` so `get_health` can read it without depending on
/// `tracing` (which `lain mcp` never initializes).
#[derive(Debug, Clone)]
pub enum RefreshResult {
    /// The re-index ran to completion. The graph is current.
    Ok,
    /// The re-index was skipped (mode == Off, or graph already
    /// current and the implementation short-circuited). Not a failure.
    Skipped,
    /// The re-index exceeded the configured timeout. The graph
    /// state at the timeout is whatever the worker had written so
    /// far (likely partial).
    Timeout,
    /// The re-index returned an error (e.g. git lock contention,
    /// LSP server failed to start). The graph is unchanged.
    Failed(String),
}

impl RefreshResult {
    /// Short human-readable label for the failure modes, used by
    /// `RefreshOutcome::banner_line`.
    pub fn label(&self) -> &'static str {
        match self {
            RefreshResult::Ok => "ok",
            RefreshResult::Skipped => "skipped",
            RefreshResult::Timeout => "timed out",
            RefreshResult::Failed(_) => "failed",
        }
    }
}

/// Snapshot of the most recent re-index attempt. Wrapped in
/// `Arc<parking_lot::Mutex<...>>` and stored on `LainServer` as
/// `last_outcome`. Read by `get_health` and (in step 3) by the
/// tool dispatcher for the scoped banner.
#[derive(Debug, Clone)]
pub struct RefreshOutcome {
    pub started_at: SystemTime,
    pub completed_at: Option<SystemTime>,
    pub result: RefreshResult,
}

impl RefreshOutcome {
    /// `Skipped` at construction time. The LainServer constructors
    /// initialize `last_outcome` to this so `get_health` always has
    /// a value to format.
    pub fn skipped() -> Self {
        Self {
            started_at: SystemTime::now(),
            completed_at: Some(SystemTime::now()),
            result: RefreshResult::Skipped,
        }
    }

    pub fn ok(started_at: SystemTime) -> Self {
        Self {
            started_at,
            completed_at: Some(SystemTime::now()),
            result: RefreshResult::Ok,
        }
    }

    pub fn timeout(started_at: SystemTime) -> Self {
        Self {
            started_at,
            completed_at: Some(SystemTime::now()),
            result: RefreshResult::Timeout,
        }
    }

    pub fn failed(started_at: SystemTime, e: impl Into<String>) -> Self {
        Self {
            started_at,
            completed_at: Some(SystemTime::now()),
            result: RefreshResult::Failed(e.into()),
        }
    }

    /// One-line banner for `get_health` and (later) the dispatcher.
    /// `None` for the Ok / Skipped cases — those don't need a banner.
    /// `true` when the last refresh did not complete, so the graph
    /// being served may not match HEAD. Drives the `Status:` line in
    /// `get_health` — which used to print `Operational ✅` beside its
    /// own `⚠ startup re-index failed` warning, for two days, while
    /// serving a graph 94 files behind the working tree.
    pub fn is_degraded(&self) -> bool {
        matches!(
            self.result,
            RefreshResult::Timeout | RefreshResult::Failed(_)
        )
    }

    pub fn banner_line(&self) -> Option<String> {
        match &self.result {
            RefreshResult::Ok | RefreshResult::Skipped => None,
            RefreshResult::Timeout => {
                let elapsed = self
                    .completed_at?
                    .duration_since(self.started_at)
                    .unwrap_or_default();
                Some(format!(
                    "⚠ startup re-index timed out after {}s; serving existing graph",
                    elapsed.as_secs()
                ))
            }
            RefreshResult::Failed(e) => Some(format!(
                "⚠ startup re-index failed: {e}; serving existing graph"
            )),
        }
    }
}

impl Default for RefreshOutcome {
    fn default() -> Self {
        Self::skipped()
    }
}

/// Parse the `LAIN_REINDEX_TIMEOUT` env var into a `Duration`.
/// Falls back to the placeholder default of 300s (5min). The user
/// is expected to measure a real full index and set this to the
/// measured p95 + headroom before the PR merges — the plan
/// explicitly forbids picking a default by guessing.
pub fn parse_reindex_timeout() -> Duration {
    match std::env::var("LAIN_REINDEX_TIMEOUT") {
        Ok(s) => match s.parse::<u64>() {
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => {
                eprintln!(
                    "LAIN_REINDEX_TIMEOUT={s:?} is not a valid integer; using default 300s"
                );
                Duration::from_secs(300)
            }
        },
        Err(_) => Duration::from_secs(300),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_outcome_has_no_banner() {
        let o = RefreshOutcome::skipped();
        assert!(o.banner_line().is_none());
    }

    #[test]
    fn ok_outcome_has_no_banner() {
        let o = RefreshOutcome::ok(SystemTime::now());
        assert!(o.banner_line().is_none());
    }

    #[test]
    fn timeout_outcome_banner_states_elapsed_and_graph_status() {
        // Started 92 seconds ago. 92s → "timed out after 92s".
        let started = SystemTime::now() - Duration::from_secs(92);
        let o = RefreshOutcome::timeout(started);
        let line = o.banner_line().expect("timeout must produce a banner");
        assert!(line.contains("timed out"));
        assert!(line.contains("92s"));
        assert!(line.contains("existing graph"));
    }

    #[test]
    fn failed_outcome_banner_carries_error_message() {
        let o = RefreshOutcome::failed(SystemTime::now(), "git lock contention");
        let line = o.banner_line().expect("failure must produce a banner");
        assert!(line.contains("failed"));
        assert!(line.contains("git lock contention"));
        assert!(line.contains("existing graph"));
    }

    #[test]
    fn parse_reindex_timeout_uses_default_when_unset() {
        // SAFETY: this test is single-threaded and serial; clearing
        // LAIN_REINDEX_TIMEOUT for the duration of the assertion is
        // safe. The surrounding tests don't depend on the env var.
        let prev = std::env::var("LAIN_REINDEX_TIMEOUT").ok();
        std::env::remove_var("LAIN_REINDEX_TIMEOUT");
        assert_eq!(parse_reindex_timeout(), Duration::from_secs(300));
        if let Some(v) = prev {
            std::env::set_var("LAIN_REINDEX_TIMEOUT", v);
        }
    }

    #[test]
    fn parse_reindex_timeout_honors_env_var() {
        let prev = std::env::var("LAIN_REINDEX_TIMEOUT").ok();
        std::env::set_var("LAIN_REINDEX_TIMEOUT", "600");
        assert_eq!(parse_reindex_timeout(), Duration::from_secs(600));
        if let Some(v) = prev {
            std::env::set_var("LAIN_REINDEX_TIMEOUT", v);
        } else {
            std::env::remove_var("LAIN_REINDEX_TIMEOUT");
        }
    }
}
