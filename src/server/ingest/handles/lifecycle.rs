//! Lifecycle — server start time, cooperative shutdown signal, and
//! startup-task join handle.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` holds an
//! `Arc<LifecycleInfo>` in PR 3.7b and forwards every accessor through.
//!
//! AGENT_UX_ROADMAP.md Milestone 4 follow-up (FOLLOWUPS.md
//! §"Cooperative cancellation token"): the single
//! `startup_cancel: CancellationToken` created here is the one
//! cooperative shutdown signal shared by every long-running indexing
//! phase (`build_core_memory`, `index_one_repo`, the detached NLP
//! prewarm, both file watchers) and the background startup task. A
//! `Drop` impl on the underlying `LainServer` cancels the token so
//! any in-flight `is_cancelled()` check observes shutdown before
//! the process tears the runtime down. `startup_task` retains the
//! `JoinHandle` of the backgrounded startup re-index so `Drop` (or
//! an explicit `shutdown()`) can `JoinHandle::await` it within the
//! bounded budget, replacing the pre-fix `AbortHandle::abort()`.

use std::time::SystemTime;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Process start time, captured at construction. Used by
/// `get_server_status` to report uptime.
pub struct LifecycleInfo {
    pub(crate) started_at: SystemTime,
    /// Server-owned cooperative shutdown signal. One owner; every
    /// long-running phase observes it via `is_cancelled()` and a
    /// `select!` race. Created here, cancelled by `Drop` or by an
    /// explicit `shutdown()` call.
    pub(crate) startup_cancel: CancellationToken,
    /// The `JoinHandle` of the background startup re-index task
    /// spawned by `LainMcpServer::run_stdio`/`run_http`. Stored so
    /// `Drop` / `shutdown` can bound-await it within the existing
    /// 5-second shutdown budget. The HTTP transport previously
    /// dropped the handle — it now stores here too so a SIGINT
    /// during a long federation cold-boot has a real cooperative
    /// join path rather than just detaching the task and relying on
    /// process exit to reclaim it.
    pub(crate) startup_task: parking_lot::Mutex<Option<JoinHandle<()>>>,
    /// Optional shutdown signal receiver. The HTTP transport sets
    /// this so an external supervisor (e.g. a signal handler or
    /// server-stop call) can wake `LainServer::shutdown` without
    /// polling. Stays `None` for stdio (the transport's natural
    /// end-of-stream IS the shutdown signal).
    pub(crate) shutdown_rx: parking_lot::Mutex<Option<oneshot::Receiver<()>>>,
}

impl LifecycleInfo {
    pub fn new(started_at: SystemTime) -> Self {
        Self {
            started_at,
            startup_cancel: CancellationToken::new(),
            startup_task: parking_lot::Mutex::new(None),
            shutdown_rx: parking_lot::Mutex::new(None),
        }
    }

    /// Process start time, captured at construction. Used by
    /// `get_server_status` to report uptime.
    pub fn started_at(&self) -> SystemTime {
        self.started_at
    }

    /// Clone of the server-owned cancellation token. Pass to every
    /// long-running phase; cancelling the original (via `cancel()`
    /// on the handle held in `LifecycleInfo`, or implicitly via
    /// `Drop`) is observed by every clone.
    pub fn cancel_token(&self) -> CancellationToken {
        self.startup_cancel.clone()
    }

    /// Cancel the server-owned token. Idempotent.
    pub fn cancel(&self) {
        self.startup_cancel.cancel();
    }

    /// True iff the server-owned token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.startup_cancel.is_cancelled()
    }

    /// Install the backgrounded startup task's `JoinHandle` so
    /// shutdown can await it. Replaces any previously installed
    /// handle (which is treated as orphaned and dropped — a
    /// misbehaving caller should not stack join handles here).
    pub fn install_startup_task(&self, handle: JoinHandle<()>) {
        let mut slot = self.startup_task.lock();
        // If a previous handle is sitting here it was orphaned by a
        // buggy caller (the transport already moved on). The runtime
        // will continue running it; dropping our handle does NOT
        // cancel that task. The pre-fix behavior was to drop the
        // handle and rely on process exit, which is what we keep
        // doing for orphaned entries.
        *slot = Some(handle);
    }

    /// Take the stored `JoinHandle`, leaving `None` in its place.
    /// Used by `LainServer::shutdown` and `Drop` to bound-await the
    /// startup task within the shutdown budget.
    pub fn take_startup_task(&self) -> Option<JoinHandle<()>> {
        self.startup_task.lock().take()
    }

    /// Install a oneshot receiver that, when fired, requests
    /// shutdown. Used by the HTTP transport so an external signal
    /// handler can wake the shutdown path. Only one receiver is
    /// stored; a second install overwrites the first.
    pub fn install_shutdown_signal(&self, rx: oneshot::Receiver<()>) {
        *self.shutdown_rx.lock() = Some(rx);
    }

    /// Take the stored shutdown signal receiver. Used by
    /// `LainServer::shutdown` to await an external shutdown
    /// request alongside its own 5-second budget.
    pub fn take_shutdown_signal(&self) -> Option<oneshot::Receiver<()>> {
        self.shutdown_rx.lock().take()
    }
}

impl Drop for LifecycleInfo {
    fn drop(&mut self) {
        // Cancelling the token is what every long-running phase
        // observes; the JoinHandle is intentionally dropped here,
        // because we are at end-of-life for the server. The token
        // cancel still races in-flight `is_cancelled()` checks
        // before the runtime tears down.
        self.startup_cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_started_at_passed_in() {
        let now = SystemTime::now();
        let handle = LifecycleInfo::new(now);
        assert_eq!(handle.started_at(), now);
    }

    #[test]
    fn cancel_token_observes_cancellation() {
        let handle = LifecycleInfo::new(SystemTime::now());
        let token = handle.cancel_token();
        assert!(!token.is_cancelled());
        handle.cancel();
        assert!(token.is_cancelled());
        assert!(handle.is_cancelled());
    }

    #[tokio::test]
    async fn take_startup_task_clears_the_slot() {
        let handle = LifecycleInfo::new(SystemTime::now());
        let join: JoinHandle<()> = tokio::spawn(async {});
        handle.install_startup_task(join);
        assert!(handle.take_startup_task().is_some());
        assert!(handle.take_startup_task().is_none());
    }

    #[test]
    fn drop_cancels_the_token() {
        let token;
        {
            let handle = LifecycleInfo::new(SystemTime::now());
            token = handle.cancel_token();
            assert!(!token.is_cancelled());
        }
        assert!(token.is_cancelled());
    }
}
