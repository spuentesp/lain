//! `LainServer` — the façade that holds the 9 partition handles and
//! forwards the 42 public methods.
//!
//! PR 3.7 clobbered the 31-field god struct. Each partition now owns
//! its fields in `src/server/ingest/handles/`, and LainServer holds
//! `Arc<XxxHandle>` for each one. The 42 public methods here are
//! forwarding shims — their signatures and semantics are byte-identical
//! to the pre-split LainServer so the 99 external call sites keep
//! working with the smallest possible diff.
//!
//! Constructors (`new`, `with_federation*`) live in
//! [`super::constructors`]; background tasks live in
//! [`super::background`]; config types live in
//! [`super::config`]; the handle structs live in
//! [`super::handles`]. This module holds the façade + every accessor,
//! lifecycle, and persistence method.

use super::handles::{
    AttributionState, AuditState, AuthHandle, FederationHandle, HotReloadBus, IngestHandle,
    LifecycleInfo, PresenceLayer, RefreshState,
};
use crate::config::state_path_for_workspace;
use crate::server::activity::ActivityTracker;
use crate::server::annotations::AnnotationRegistry;
use crate::server::federation::config::RepoConfig;
use crate::server::federation::federated_index::FederatedIndex;
use crate::server::federation::repo_id::RepoId;
use crate::server::federation::workspace::WorkspacesFile;
use crate::server::intent::IntentRegistry;
use crate::server::overlay::{broadcast_overlay_diff, OverlayDiff, RevisionId};
use crate::server::presence::{
    save_pair as save_presence_pair, AgentId, OccupancyMap, PresenceEvent, PresenceRegistry,
};
use crate::server::reload::ReloadBus;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::broadcast;
use tracing::info;

#[derive(Clone)]
pub struct LainServer {
    ingest: Arc<IngestHandle>,
    refresh: Arc<RefreshState>,
    federation: Arc<FederationHandle>,
    presence: Arc<PresenceLayer>,
    audit: Arc<AuditState>,
    hot_reload: Arc<HotReloadBus>,
    auth: Arc<AuthHandle>,
    attribution: Arc<AttributionState>,
    lifecycle: Arc<LifecycleInfo>,
    /// Per-repo annotation registry. Lives on the LainServer (not in
    /// any handle) because it post-dates the audit's 9-partition list;
    /// it gets its own partition during the next handle pass.
    pub annotations: Arc<AnnotationRegistry>,
}

impl LainServer {
    /// Construct from the 9 handles plus the post-audit annotations
    /// registry. The constructors module wires everything up before
    /// calling this.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_handles(
        ingest: Arc<IngestHandle>,
        refresh: Arc<RefreshState>,
        federation: Arc<FederationHandle>,
        presence: Arc<PresenceLayer>,
        audit: Arc<AuditState>,
        hot_reload: Arc<HotReloadBus>,
        auth: Arc<AuthHandle>,
        attribution: Arc<AttributionState>,
        lifecycle: Arc<LifecycleInfo>,
        annotations: Arc<AnnotationRegistry>,
    ) -> Self {
        Self {
            ingest,
            refresh,
            federation,
            presence,
            audit,
            hot_reload,
            auth,
            attribution,
            lifecycle,
            annotations,
        }
    }

    // =============== Handle accessors ===============
    // `pub` because integration tests under `tests/` reach into the
    // partitions; sibling modules in `src/` use the same path.

    pub fn ingest(&self) -> &IngestHandle {
        &self.ingest
    }

    pub fn refresh_handle(&self) -> &RefreshState {
        &self.refresh
    }

    pub fn federation_handle(&self) -> &FederationHandle {
        &self.federation
    }

    pub fn presence_handle(&self) -> &PresenceLayer {
        &self.presence
    }

    pub fn audit_handle(&self) -> &AuditState {
        &self.audit
    }

    pub fn hot_reload_handle(&self) -> &HotReloadBus {
        &self.hot_reload
    }

    pub fn auth_handle_inner(&self) -> &AuthHandle {
        &self.auth
    }

    pub fn attribution_handle(&self) -> &AttributionState {
        &self.attribution
    }

    pub fn lifecycle_handle(&self) -> &LifecycleInfo {
        &self.lifecycle
    }

    /// Owned clone of the server-owned lifecycle handle. Used by
    /// shutdown paths and tests that need to move the handle into
    /// a `'static` future (e.g. cancelling the token from a
    /// separate `tokio::spawn`).
    pub fn lifecycle_arc(&self) -> std::sync::Arc<LifecycleInfo> {
        std::sync::Arc::clone(&self.lifecycle)
    }

    // =============== Public façade (forwarding shims) ===============
    // Signatures and semantics are byte-identical to the pre-split
    // LainServer.

    pub fn federation(&self) -> Option<&Arc<FederatedIndex>> {
        self.federation.federation()
    }

    pub fn reload_bus(&self) -> Arc<ReloadBus> {
        self.hot_reload.reload_bus()
    }

    pub fn overlay_updated(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(self.ingest.overlay_updated())
    }

    pub fn readiness(&self) -> &crate::server::readiness::ReadinessHandle {
        self.ingest.readiness()
    }

    pub fn presence(&self) -> &Arc<PresenceRegistry> {
        self.presence.presence()
    }

    pub fn occupancy(&self) -> &Arc<OccupancyMap> {
        self.presence.occupancy()
    }

    /// Intent registry (PR 1 of `docs/INTENT_AND_OBSERVABILITY_PLAN.md`).
    /// Forwards to the underlying `PresenceLayer`'s intent handle.
    /// The MCP `lain_intent` and `list_active_intents` tools consult
    /// this; the per-agent activity feed in `who_am_i` /
    /// `list_active_agents` reads it.
    pub fn intent(&self) -> &Arc<IntentRegistry> {
        self.presence.intent()
    }

    /// Activity tracker (PR 1). Forwards to the underlying
    /// `PresenceLayer`'s activity handle. The hook ingestion
    /// endpoint (PR 2) will record observations here; the per-agent
    /// activity feed surfaces `recent_tools`, `focus`, and
    /// `observed_reads` from this registry.
    pub fn activity(&self) -> &Arc<ActivityTracker> {
        self.presence.activity()
    }

    pub fn presence_event_tx(&self) -> &broadcast::Sender<(u64, PresenceEvent)> {
        self.presence.presence_event_tx()
    }

    pub fn overlay(&self) -> &crate::server::overlay::VolatileOverlay {
        self.ingest.overlay()
    }

    pub fn emit_presence_event(&self, event: PresenceEvent) {
        self.presence
            .emit_presence_event(self.audit.events_log().as_ref(), event);
    }

    pub fn unregister_agent(&self, agent_id: &AgentId) -> Vec<PathBuf> {
        self.presence
            .unregister_agent(self.audit.events_log().as_ref(), agent_id)
    }

    pub fn unregister_session(&self, agent_id: &AgentId) -> Vec<PathBuf> {
        self.unregister_agent(agent_id)
    }

    pub fn attribution(&self) -> &Arc<dyn crate::server::attribution::AttributionBackend> {
        self.attribution.attribution()
    }

    pub fn started_at(&self) -> SystemTime {
        self.lifecycle.started_at()
    }

    pub fn last_sync_at(&self) -> SystemTime {
        self.refresh.last_sync_at()
    }

    pub fn last_error(&self) -> Option<String> {
        self.refresh.last_error()
    }

    pub fn record_sync(&self) {
        self.refresh.record_sync();
    }

    pub fn record_last_error(&self, msg: impl Into<String>) {
        self.refresh.record_last_error(msg);
    }

    pub fn transport(&self) -> Option<super::config::Transport> {
        self.federation.transport()
    }

    pub fn port(&self) -> Option<u16> {
        self.federation.port()
    }

    pub fn repos_yaml(&self) -> Option<&Path> {
        self.federation.repos_yaml()
    }

    pub fn repo_count(&self) -> usize {
        self.federation.repo_count()
    }

    pub fn workspace_count(&self) -> usize {
        self.federation.workspace_count()
    }

    pub fn annotations(&self) -> &Arc<AnnotationRegistry> {
        &self.annotations
    }

    pub fn federation_repos(&self) -> Vec<RepoId> {
        self.federation.federation_repos()
    }

    /// Consume the server and run the federation-mode MCP loop.
    pub async fn serve(self) -> Result<(), crate::server::error::LainError> {
        let server_arc = Arc::new(self.clone());
        let federation = self.federation.federation().cloned().ok_or_else(|| {
            crate::server::error::LainError::Other(
                "LainServer::serve() called on a non-federation server (use LainServer::new for single-workspace)".into(),
            )
        })?;
        let transport = self.transport().ok_or_else(|| {
            crate::server::error::LainError::Other(
                "LainServer::serve(): missing transport (internal)".into(),
            )
        })?;
        let port = self.port().unwrap_or(9999);

        let workspaces = self.federation.workspaces_handle();
        let mcp = match workspaces {
            Some(ws) => crate::server::mcp::handler::LainMcpServer::with_federation_and_workspaces(
                self.ingest.tool_executor().clone(),
                federation,
                ws,
            ),
            None => crate::server::mcp::handler::LainMcpServer::with_federation(
                self.ingest.tool_executor().clone(),
                federation,
            ),
        }
        .with_status(
            Some(transport),
            Some(port),
            self.lifecycle.started_at(),
            self.refresh.sync_status().last_sync_at_handle(),
            self.refresh.sync_status().last_error_handle(),
        )
        .with_reload_bus(self.hot_reload.reload_bus())
        .with_server(server_arc);
        match transport {
            super::config::Transport::Http => mcp
                .run_http(port)
                .await
                .map_err(|e| crate::server::error::LainError::Mcp(format!("HTTP transport: {e}"))),
            super::config::Transport::Stdio => mcp
                .run_stdio()
                .await
                .map_err(|e| crate::server::error::LainError::Mcp(format!("stdio transport: {e}"))),
        }
    }

    pub fn clone_for_background(&self) -> Self {
        self.clone()
    }

    pub fn next_revision(&self) -> RevisionId {
        self.ingest.next_revision()
    }

    pub fn broadcast_overlay_insert(&self, node: crate::server::schema::GraphNode) {
        broadcast_overlay_diff(OverlayDiff {
            revision: self.next_revision(),
            added: vec![node],
            removed: vec![],
            updated: vec![],
        });
    }

    pub fn overlay_paths_test_insert(&self, key: String, node: crate::server::schema::GraphNode) {
        self.ingest.overlay_paths_test_insert(key, node);
    }

    pub fn overlay_paths_test_keys(&self) -> Vec<String> {
        self.ingest.overlay_paths_test_keys()
    }

    pub fn overlay_paths_record_insert(&self, key: String, node_id: String) {
        self.ingest.overlay_paths_record_insert(key, node_id);
    }

    pub fn overlay_paths_replace(&self, key: String, node_ids: Vec<String>) {
        self.ingest.overlay_paths_replace(key, node_ids);
    }

    pub async fn add_repo(
        &self,
        repo: &RepoConfig,
        data_dir: &Path,
    ) -> Result<(), crate::server::error::LainError> {
        self.federation.add_repo(repo, data_dir).await?;
        self.refresh.record_sync();
        Ok(())
    }

    pub fn remove_repo(&self, repo_id: &str) -> Result<(), crate::server::error::LainError> {
        self.federation.remove_repo(repo_id)?;
        self.refresh.record_sync();
        Ok(())
    }

    pub fn set_workspace(&self, workspaces: Arc<WorkspacesFile>) {
        self.federation.set_workspace(workspaces);
    }

    pub fn workspaces_snapshot(&self) -> Option<Arc<WorkspacesFile>> {
        self.federation.workspaces_snapshot()
    }

    pub fn workspaces_handle(&self) -> Option<Arc<RwLock<WorkspacesFile>>> {
        self.federation.workspaces_handle()
    }

    pub async fn shutdown(&self) {
        info!("Shutting down Lain server...");
        self.ingest.shutdown().await;
    }

    pub fn state_path(&self) -> PathBuf {
        if let Some(repos) = self.federation.repos_yaml() {
            state_path_for_workspace(repos)
        } else {
            state_path_for_workspace(self.ingest.config().workspace.as_path())
        }
    }

    pub fn state_dir_for_audit(&self) -> PathBuf {
        self.state_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(crate::config::state_dir)
    }

    pub fn events_log_path_from_config(mem_path: &Path) -> PathBuf {
        mem_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(crate::config::state_dir)
    }

    pub fn static_graph_generation_unix(&self) -> Option<i64> {
        self.refresh.static_graph_generation_unix()
    }

    pub fn save_state(&self) -> Result<(), crate::server::error::LainError> {
        self.presence.save_state()
    }

    pub fn load_state(&self) -> Result<(), crate::server::error::LainError> {
        self.presence.load_state()
    }

    pub fn with_shared_presence<T>(&self, f: impl FnOnce() -> T) -> T {
        self.presence.with_shared_presence(f)
    }

    pub fn refresh_shared_presence(&self) {
        self.presence.refresh_shared_presence();
    }

    /// Install a persist callback on `presence`, `occupancy`,
    /// `intent`, and `activity` that drives `save_state` on every
    /// mutation. Called once from each constructor, immediately
    /// after the registries are built and after `load_state` has
    /// hydrated them. PR 1 of
    /// `docs/INTENT_AND_OBSERVABILITY_PLAN.md` extends the persist
    /// surface to the intent and activity registries — every
    /// mutation (declare intent, update intent, record tool
    /// observation) flows through the same `save_presence_pair`
    /// saver that presence and occupancy already use.
    pub(crate) fn install_persist_callback(&self) {
        let path = self.presence.state_path();
        let presence = Arc::clone(self.presence.presence());
        let occupancy = Arc::clone(self.presence.occupancy());
        let intent = Arc::clone(self.presence.intent());
        let activity = Arc::clone(self.presence.activity());
        let cb = move || {
            if let Err(e) = save_presence_pair(&path, &presence, &occupancy, &intent, &activity) {
                tracing::warn!("persist failed: {e}");
            }
        };
        self.presence.presence().set_persist_callback(cb.clone());
        let presence2 = Arc::clone(self.presence.presence());
        let occupancy2 = Arc::clone(self.presence.occupancy());
        let intent2 = Arc::clone(self.presence.intent());
        let activity2 = Arc::clone(self.presence.activity());
        let path2 = self.presence.state_path();
        let cb2 = move || {
            if let Err(e) =
                save_presence_pair(&path2, &presence2, &occupancy2, &intent2, &activity2)
            {
                tracing::warn!("persist failed: {e}");
            }
        };
        self.presence.occupancy().set_persist_callback(cb2);
        // Intent + activity get their own callbacks so a mutation in
        // either triggers the same saver. The callbacks capture
        // identical state by `Arc::clone`, so a single persist fires
        // for every mutation regardless of which registry originated
        // it.
        let cb3 = cb.clone();
        self.presence.intent().set_persist_callback(cb3);
        let cb4 = cb.clone();
        self.presence.activity().set_persist_callback(cb4);
        let occupancy_for_remove = Arc::clone(self.presence.occupancy());
        self.presence.presence().set_on_remove_callback(move |id| {
            occupancy_for_remove.release_all_for(id);
        });
        let workspace = self.ingest.config().workspace.clone();
        self.presence.occupancy().set_workspace_root(&workspace);
        if let Some(fed) = self.federation.federation() {
            self.presence.occupancy().add_claim_roots(&fed.repo_paths());
        }
        let _ = cb;
    }
}
