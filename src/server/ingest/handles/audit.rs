//! Audit — durable SSE event log.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<AuditState>` in PR 3.7b. The `EventsLog` is currently written
//! only via `PresenceLayer::emit_presence_event`; no public methods
//! live on `LainServer` for it today, but the handle reserves the slot
//! so the audit concern is named alongside the others.

use crate::server::events_log::EventsLog;
use std::sync::Arc;

/// Durable SSE event log (P1 #2). Captures every `PresenceEvent`
/// broadcast on the SSE channel with a monotonic `event_id: u64`,
/// supports replay-after-id via `events.jsonl` so SSE subscribers
/// that reconnect with `Last-Event-ID: N` see every event since N.
/// Lives in the same state dir as `audit.jsonl`.
pub struct AuditState {
    pub(crate) events_log: Arc<EventsLog>,
}

impl AuditState {
    pub fn new(events_log: Arc<EventsLog>) -> Self {
        Self { events_log }
    }

    /// Borrowed handle to the underlying [`EventsLog`]. Currently
    /// internal-only — callers go through `PresenceLayer::emit_presence_event`,
    /// which assigns the monotonic `event_id` at the same time as the
    /// broadcast. Exposed here so the audit slot is reachable from the
    /// outside without a future round of plumbing.
    pub fn events_log(&self) -> &Arc<EventsLog> {
        &self.events_log
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_the_log_it_was_built_with() {
        let tmp = tempfile::tempdir().unwrap();
        let log = Arc::new(EventsLog::open(tmp.path()).unwrap());
        let handle = AuditState::new(Arc::clone(&log));
        assert!(Arc::ptr_eq(handle.events_log(), &log));
    }
}
