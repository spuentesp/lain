use super::LainServer;
use tracing::{debug, info, warn};

impl LainServer {
    /// Run periodic sync every interval_seconds
    pub async fn run_background_sync(&self, interval_secs: u64) {
        // Sidecars never re-ingest; the owner drives that work.
        if self.graph.is_read_only() {
            return;
        }
        let interval = tokio::time::Duration::from_secs(interval_secs);
        loop {
            tokio::time::sleep(interval).await;
            info!("Background sync: checking for updates...");
            let commit = self.git.lock().get_latest_commit_info()
                .map(|(commit, _)| commit)
                .inspect_err(|e| warn!("Background sync: failed to get commit info: {}", e))
                .ok();
            if let Some(commit) = commit {
                if let Ok(Some(last)) = self.graph.get_last_commit() {
                    if last != commit {
                        info!("Background sync: new commits detected, triggering sync");
                        let s = self.clone();
                        if let Err(e) = s.build_core_memory().await {
                            warn!("Background sync failed: {}", e);
                        }
                    } else {
                        debug!("Background sync: already up to date");
                    }
                }
            }
        }
    }

    // `run_sliding_window` lived here: a third background strategy that
    // polled for uncommitted changes and refreshed the overlay
    // dirty-first with its own budgets. It had no caller and no test,
    // and it is now genuinely redundant — `FileWatcher` refreshes dirty
    // files reactively as they are saved, `sync_volatile_overlay` seeds
    // the overlay from uncommitted work at startup, and
    // `run_background_sync` (above) re-indexes when the commit moves.
    // Keeping a fourth, unreachable copy of that job would only rot.

}
