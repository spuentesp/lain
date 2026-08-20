//! Sync attempt bookkeeping shared by the federation ingest/sync paths.
//!
//! `LainServer` holds a `SyncStatus` behind `Arc` and updates it on every
//! sync attempt; `get_server_status` reads it. Pulled out so the two
//! pieces of state (`last_sync_at`, `last_error`) live next to the
//! transitions they describe and the LainServer impl block doesn't have
//! to grow for each new tracking field.

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone)]
pub struct SyncStatus {
    inner: Arc<SyncStatusInner>,
}

struct SyncStatusInner {
    last_sync_at: Arc<Mutex<SystemTime>>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl SyncStatus {
    /// Create with `last_sync_at = now` and `last_error = None`. The
    /// caller supplies the boot time so every `SyncStatus` for a given
    /// server agrees on the initial timestamp.
    pub fn new(boot_at: SystemTime) -> Self {
        Self {
            inner: Arc::new(SyncStatusInner {
                last_sync_at: Arc::new(Mutex::new(boot_at)),
                last_error: Arc::new(Mutex::new(None)),
            }),
        }
    }

    pub fn last_sync_at(&self) -> SystemTime {
        *self.inner.last_sync_at.lock()
    }

    pub fn last_error(&self) -> Option<String> {
        self.inner.last_error.lock().clone()
    }

    /// Sync attempt succeeded: bump `last_sync_at`, clear `last_error`.
    pub fn record_ok(&self) {
        *self.inner.last_sync_at.lock() = SystemTime::now();
        *self.inner.last_error.lock() = None;
    }

    /// Sync attempt failed: bump `last_sync_at` so operators can see
    /// the attempt happened, and store `msg` as the new error.
    pub fn record_error(&self, msg: impl Into<String>) {
        *self.inner.last_sync_at.lock() = SystemTime::now();
        *self.inner.last_error.lock() = Some(msg.into());
    }

    /// Handle to the inner `last_sync_at` mutex. `LainServer::with_status`
    /// Arc-shares this with `LainMcpServer` so the status tool sees live
    /// updates without an extra hop. Crate-private — not part of the
    /// public API.
    pub(crate) fn last_sync_at_handle(&self) -> Arc<Mutex<SystemTime>> {
        Arc::clone(&self.inner.last_sync_at)
    }

    /// Crate-private handle to the inner `last_error` mutex; see
    /// [`Self::last_sync_at_handle`] for the rationale.
    pub(crate) fn last_error_handle(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.inner.last_error)
    }
}
