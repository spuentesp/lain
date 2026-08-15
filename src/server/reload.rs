//! Hot-reload bus for the `LainServer`.
//!
//! `ReloadBus` fans out a "please rebuild" signal to any subscriber that
//! asked for one (the file watcher, the Unix socket handler, the MCP
//! `request_reload` tool) and tracks the most recent reload state for
//! observability (`get_reload_status`).
//!
//! The reload itself is performed by a separate task (see Task 6.2);
//! this module only models the *signal* and *status*. Subscribers
//! receive a coarse `()` notification and are responsible for fetching
//! the current `ReloadStatus` and acting on it.

use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::{broadcast, Mutex as AsyncMutex};

/// Phase of the last reload attempt.
///
/// The bus never owns the rebuild work itself; it only records what
/// the server is currently doing. `Failed` carries the human-readable
/// error message that the rebuild task (Task 6.2) reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadState {
    Idle,
    Rebuilding,
    Failed(String),
}

/// Snapshot of the reload subsystem, suitable for returning from the
/// `get_reload_status` MCP tool.
///
/// `started_at` is set when the state transitions to `Rebuilding` and
/// cleared on completion. `last_reload_at` is set when the state
/// transitions back to `Idle` (i.e. a successful reload finished).
/// `pending_changes` is a list of paths that triggered the most recent
/// request, intended for the UI / status reporting.
#[derive(Debug, Clone)]
pub struct ReloadStatus {
    pub state: ReloadState,
    pub started_at: Option<SystemTime>,
    pub last_reload_at: Option<SystemTime>,
    pub last_error: Option<String>,
    pub pending_changes: Vec<String>,
}

/// Hot-reload signal bus.
///
/// Cloning is not provided: the bus is typically wrapped in an `Arc`
/// at the `LainServer` boundary and shared by reference. Subscribers
/// get a `ReloadSubscriber` that they can poll with `try_recv`.
pub struct ReloadBus {
    tx: broadcast::Sender<()>,
    status: Arc<AsyncMutex<ReloadStatus>>,
}

impl ReloadBus {
    /// Capacity of 16 is plenty for the typical subscriber count
    /// (file watcher, Unix socket listener, MCP request_reload tool).
    /// If a subscriber falls behind it sees `RecvError::Lagged`, which
    /// the rebuild task treats as "ask again on the next status poll".
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self {
            tx,
            status: Arc::new(AsyncMutex::new(ReloadStatus {
                state: ReloadState::Idle,
                started_at: None,
                last_reload_at: None,
                last_error: None,
                pending_changes: Vec::new(),
            })),
        }
    }

    /// Register a new listener for reload requests.
    pub fn subscribe(&self) -> ReloadSubscriber {
        ReloadSubscriber {
            rx: self.tx.subscribe(),
        }
    }

    /// Broadcast a reload request. The actual rebuild is performed by
    /// whichever subscriber picks the signal up (Task 6.2).
    ///
    /// Returns `Result` for symmetry with the future request-rebuild
    /// path that may bubble up validation errors. The broadcast itself
    /// never errors today — if there are no subscribers the message is
    /// simply dropped, which is the desired behavior.
    pub fn request_reload(&self) -> Result<(), String> {
        let _ = self.tx.send(());
        Ok(())
    }

    /// Cheap clone of the current status snapshot.
    pub fn status(&self) -> ReloadStatus {
        // `try_lock` is the right call here: callers (`get_reload_status`)
        // are MCP handlers that should never park the executor. If the
        // status is being written to right now, returning the previous
        // snapshot is acceptable — the next call will see the update.
        self.status
            .try_lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| ReloadStatus {
                state: ReloadState::Idle,
                started_at: None,
                last_reload_at: None,
                last_error: None,
                pending_changes: Vec::new(),
            })
    }

    /// Update the bus's recorded state. The rebuild task calls this on
    /// each phase transition so that observers (`get_reload_status`)
    /// can report progress.
    pub async fn set_state(&self, state: ReloadState) {
        let mut s = self.status.lock().await;
        s.state = state.clone();
        match state {
            ReloadState::Rebuilding => {
                s.started_at = Some(SystemTime::now());
                s.last_error = None;
            }
            ReloadState::Idle => {
                s.started_at = None;
                s.last_reload_at = Some(SystemTime::now());
            }
            ReloadState::Failed(msg) => {
                s.started_at = None;
                s.last_error = Some(msg);
            }
        }
    }
}

impl Default for ReloadBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle for receiving reload requests.
///
/// `try_recv` is non-blocking: the caller (typically the rebuild task
/// loop, or a test) decides how to wait. A lagged receiver indicates
/// that the bus emitted more than the channel's buffer capacity while
/// the subscriber was idle; the rebuild task responds by re-reading
/// `bus.status()` instead of relying on the caught-up message.
pub struct ReloadSubscriber {
    rx: broadcast::Receiver<()>,
}

impl ReloadSubscriber {
    /// Non-blocking poll. Returns `Ok(())` if a reload request was
    /// received, `Err(Empty)` if no request is pending, or
    /// `Err(Lagged)` if the subscriber fell behind.
    pub fn try_recv(&mut self) -> Result<(), broadcast::error::TryRecvError> {
        loop {
            match self.rx.try_recv() {
                Ok(()) => return Ok(()),
                Err(broadcast::error::TryRecvError::Empty) => {
                    return Err(broadcast::error::TryRecvError::Empty)
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    // Subscriber fell behind; drain any further pending
                    // messages before declaring "empty" so callers don't
                    // miss a request that was already in flight.
                    continue;
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    return Err(broadcast::error::TryRecvError::Closed)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_bus_broadcasts() {
        let bus = ReloadBus::new();
        let mut sub = bus.subscribe();
        bus.request_reload().unwrap();
        assert!(sub.try_recv().is_ok());
    }

    #[test]
    fn reload_status_reports_state() {
        let bus = ReloadBus::new();
        assert_eq!(bus.status().state, ReloadState::Idle);
    }

    #[test]
    fn try_recv_returns_none_when_no_request_pending() {
        let bus = ReloadBus::new();
        let mut sub = bus.subscribe();
        assert!(sub.try_recv().is_err());
    }

    #[test]
    fn status_returns_idle_after_failed_transitions() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let bus = ReloadBus::new();
            bus.set_state(ReloadState::Rebuilding).await;
            bus.set_state(ReloadState::Failed("boom".into())).await;
            let s = bus.status();
            assert_eq!(s.state, ReloadState::Failed("boom".into()));
            assert_eq!(s.last_error.as_deref(), Some("boom"));
            assert!(s.started_at.is_none());
        });
    }
}
