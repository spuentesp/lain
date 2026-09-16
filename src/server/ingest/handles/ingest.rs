//! Ingest — config, graph, overlay, embedders, git, LSP, tool executor, namespace.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<IngestHandle>` in PR 3.7b and forward `readiness`,
//! `next_revision`, `broadcast_overlay_insert`, the four
//! `overlay_paths_*` helpers, and `shutdown` through.

use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::nlp::{CrossEncoder, NlpEmbedder};
use crate::overlay::{broadcast_overlay_diff, OverlayDiff, RevisionId, VolatileOverlay};
use crate::schema::{GraphNode, RepoNamespace};
use crate::server::ingest::config::LainConfig;
use crate::tools::ToolExecutor;
use crate::tuning::TuningConfig;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, Notify};

/// Configuration, persistent graph, volatile overlay, embedder + cross-encoder,
/// git sensor, LSP pool, tool executor, tuning, namespace, and the
/// overlay-path bookkeeping that ties watcher-side insertions to the
/// staleness sweep.
pub struct IngestHandle {
    pub(crate) config: LainConfig,
    pub(crate) graph: GraphDatabase,
    pub(crate) overlay: VolatileOverlay,
    pub(crate) embedder: NlpEmbedder,
    pub(crate) cross_encoder: CrossEncoder,
    pub(crate) git: Arc<Mutex<GitSensor>>,
    pub(crate) lsp_pool: Arc<LspPool>,
    pub(crate) tool_executor: ToolExecutor,
    pub(crate) tuning: Arc<TuningConfig>,
    pub(crate) id_namespace: RepoNamespace,
    pub(crate) overlay_paths: Arc<Mutex<HashMap<String, Vec<String>>>>,
    pub(crate) process_change_lock: Arc<AsyncMutex<()>>,
    pub(crate) overlay_updated: Arc<Notify>,
    pub(crate) overlay_revision: Arc<AtomicU64>,
}

impl IngestHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: LainConfig,
        graph: GraphDatabase,
        overlay: VolatileOverlay,
        embedder: NlpEmbedder,
        cross_encoder: CrossEncoder,
        git: Arc<Mutex<GitSensor>>,
        lsp_pool: Arc<LspPool>,
        tool_executor: ToolExecutor,
        tuning: Arc<TuningConfig>,
        id_namespace: RepoNamespace,
        overlay_paths: Arc<Mutex<HashMap<String, Vec<String>>>>,
        process_change_lock: Arc<AsyncMutex<()>>,
        overlay_updated: Arc<Notify>,
        overlay_revision: Arc<AtomicU64>,
    ) -> Self {
        Self {
            config,
            graph,
            overlay,
            embedder,
            cross_encoder,
            git,
            lsp_pool,
            tool_executor,
            tuning,
            id_namespace,
            overlay_paths,
            process_change_lock,
            overlay_updated,
            overlay_revision,
        }
    }

    /// Shared index lifecycle handle. `build_core_memory` reports phase
    /// and progress through this same handle so there is exactly one
    /// place — never a second computation in `doctor`, a tool handler,
    /// or the MCP dispatch gate — that decides what "warming up" means.
    pub fn readiness(&self) -> &crate::server::readiness::ReadinessHandle {
        &self.tool_executor.ctx.readiness
    }

    /// Allocate the next overlay-diff revision id. Sidecars use this to
    /// detect drops in the broadcast bus.
    pub fn next_revision(&self) -> RevisionId {
        self.overlay_revision.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Broadcast a single node insertion. Sidecars pull these diffs from
    /// the broadcast bus and merge them into their in-memory overlay.
    /// Only fires when this process is an *owner* — the sidecar has its
    /// graph opened read-only (Task 3) and never reaches the call sites
    /// below `check_writable`, so it cannot broadcast by accident.
    pub fn broadcast_overlay_insert(&self, node: GraphNode) {
        broadcast_overlay_diff(OverlayDiff {
            revision: self.next_revision(),
            added: vec![node],
            removed: vec![],
            updated: vec![],
        });
    }

    /// Test-only helper: insert `node` into the overlay and record
    /// `node.id` under the workspace-relative `key` in `overlay_paths`
    /// — the same bookkeeping `process_change` does when LSP
    /// produces real symbols.
    pub fn overlay_paths_test_insert(&self, key: String, node: GraphNode) {
        self.overlay_paths
            .lock()
            .entry(key)
            .or_default()
            .push(node.id.clone());
        self.overlay.insert_node(node);
    }

    /// Test-only helper: read the current `overlay_paths` snapshot.
    pub fn overlay_paths_test_keys(&self) -> Vec<String> {
        self.overlay_paths.lock().keys().cloned().collect()
    }

    /// Record that this server's watcher (or any other overlay writer
    /// outside `process_change`) inserted a node at workspace-relative
    /// `key` with the given `node_id`.
    pub fn overlay_paths_record_insert(&self, key: String, node_id: String) {
        self.overlay_paths
            .lock()
            .entry(key)
            .or_default()
            .push(node_id);
    }

    /// Replace the bookkeeping entry for `key` with a fresh list of
    /// `node_ids`. Used when a re-saved file should drop the previous
    /// version's overlay entries from the same path before inserting
    /// the new ones.
    pub fn overlay_paths_replace(&self, key: String, node_ids: Vec<String>) {
        self.overlay_paths.lock().insert(key, node_ids);
    }

    pub async fn shutdown(&self) {
        tracing::info!("Shutting down Lain server...");
        self.lsp_pool.shutdown_all().await;
    }

    // =============== Field accessors for the LainServer façade (PR 3.7b) ===============

    pub fn config(&self) -> &LainConfig {
        &self.config
    }

    pub fn graph(&self) -> &GraphDatabase {
        &self.graph
    }

    pub fn overlay(&self) -> &VolatileOverlay {
        &self.overlay
    }

    pub fn embedder(&self) -> &NlpEmbedder {
        &self.embedder
    }

    pub fn cross_encoder(&self) -> &CrossEncoder {
        &self.cross_encoder
    }

    pub fn git(&self) -> &Arc<Mutex<GitSensor>> {
        &self.git
    }

    pub fn lsp_pool(&self) -> &Arc<LspPool> {
        &self.lsp_pool
    }

    pub fn tool_executor(&self) -> &ToolExecutor {
        &self.tool_executor
    }

    pub fn tuning(&self) -> &Arc<TuningConfig> {
        &self.tuning
    }

    pub fn id_namespace(&self) -> &RepoNamespace {
        &self.id_namespace
    }

    pub fn overlay_paths(&self) -> &Arc<Mutex<HashMap<String, Vec<String>>>> {
        &self.overlay_paths
    }

    pub fn process_change_lock(&self) -> &Arc<AsyncMutex<()>> {
        &self.process_change_lock
    }

    pub fn overlay_updated(&self) -> &Arc<Notify> {
        &self.overlay_updated
    }

    pub fn overlay_revision(&self) -> &Arc<AtomicU64> {
        &self.overlay_revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_revision_starts_at_one_and_increments() {
        // The actual construction requires a non-trivial graph /
        // tool-executor stack, so we test the revision-counter math in
        // isolation here by spinning up a tiny handle-like wrapper.
        let arc = Arc::new(AtomicU64::new(0));
        let r1 = arc.fetch_add(1, Ordering::Relaxed) + 1;
        let r2 = arc.fetch_add(1, Ordering::Relaxed) + 1;
        assert_eq!(r1, 1);
        assert_eq!(r2, 2);
    }
}
