//! Tool registry — inventory-based auto-discovery.
//!
//! Each handler registers itself via `inventory::submit!` at startup.
//! The dispatcher (`ToolRegistry::dispatch`) iterates registered tools by name.
//!
//! Adding a new tool: implement `ToolHandler` in its own module, call
//! `inventory::submit!(ToolHandlerEntry(handler))` at the bottom of the file.
//! No central edit required.

use crate::error::LainError;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use crate::server::presence::{OccupancyMap, PresenceRegistry};
use crate::server::tools::UiSession;
use crate::tuning::TuningConfig;
use async_trait::async_trait;
use inventory::iter;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

/// All dependencies a tool handler needs to do its work.
#[derive(Clone)]
pub struct ToolContext {
    pub graph: GraphDatabase,
    pub overlay: VolatileOverlay,
    pub embedder: NlpEmbedder,
    pub cross_encoder: crate::nlp::CrossEncoder,
    pub git: Arc<Mutex<GitSensor>>,
    pub lsp_pool: Arc<LspPool>,
    pub tuning: Arc<TuningConfig>,
    pub embedding_cache: Arc<Mutex<std::collections::HashMap<String, Vec<f32>>>>,
    pub ui_sessions: Arc<AsyncMutex<std::collections::HashMap<String, UiSession>>>,
    pub jobs: Arc<Mutex<std::collections::HashMap<String, crate::server::tools::JobInfo>>>,
    pub job_webhooks: Arc<AsyncMutex<Vec<String>>>,
    /// Port the HTTP transport is listening on, shared as an atomic so
    /// `run_http` can publish it after construction. 0 = no UI server
    /// (stdio mode); tool handlers then omit the interactive `/ui/...`
    /// link instead of emitting a dead URL.
    pub diagnostics_port: std::sync::Arc<std::sync::atomic::AtomicU16>,
    /// Workspace root path. Used as the default `cwd` for execution tools
    /// (`run_build`, `run_tests`, `run_clippy`) so they don't fail just
    /// because the binary was launched from a different directory.
    pub workspace: std::path::PathBuf,
    /// Presence registry shared with the `LainServer` orchestrator.
    /// Tools that surface multiplayer state (`query_graph`,
    /// `explain_symbol`) read this to attach an `occupancy` summary
    /// to their result. Initialized to an empty registry; the
    /// `LainMcpServer::with_server` wiring hook swaps in the live
    /// `Arc<PresenceRegistry>` from the constructed `LainServer` so
    /// all tool handlers see the same state as the dedicated
    /// `register_agent` / `claim_files` MCP tools.
    pub presence: Arc<PresenceRegistry>,
    /// Occupancy map shared with the `LainServer` orchestrator. Same
    /// wiring story as `presence` — see above.
    pub occupancy: Arc<OccupancyMap>,
    /// Last-refresh outcome shared with the `LainServer` orchestrator.
    /// The startup re-index spawn in `LainMcpServer::run_stdio` /
    /// `run_http` writes the timeout / failure result here; `get_health`
    /// reads it to surface the staleness banner to MCP clients (which
    /// can't see stderr or `tracing::warn`). Step 1 of the staleness
    /// fix: the failure was previously invisible. Initialized to a
    /// default `Skipped`; the `LainMcpServer::with_server` wiring hook
    /// swaps in the live `Arc<Mutex<RefreshOutcome>>` from the
    /// constructed `LainServer`.
    pub last_outcome: Arc<parking_lot::Mutex<crate::server::refresh::RefreshOutcome>>,
    /// The federation, when the server runs in federation mode.
    ///
    /// `graph` / `workspace` above are bound at construction: to the one
    /// repo when the federation holds exactly one, and to an empty
    /// staging placeholder otherwise. With several repos that made every
    /// per-repo tool answer against an empty graph. The dispatcher
    /// already resolves which repo a call targets and injects `repo_id`
    /// into the args; this handle is what lets [`Self::for_repo`] turn
    /// that id back into the right graph and checkout.
    pub federation: Option<Arc<crate::server::federation::federated_index::FederatedIndex>>,
}

impl ToolContext {
    pub fn new(
        graph: GraphDatabase,
        overlay: VolatileOverlay,
        embedder: NlpEmbedder,
        cross_encoder: crate::nlp::CrossEncoder,
        git: Arc<Mutex<GitSensor>>,
        lsp_pool: Arc<LspPool>,
        tuning: Arc<TuningConfig>,
        embedding_cache: Arc<Mutex<std::collections::HashMap<String, Vec<f32>>>>,
        ui_sessions: Arc<AsyncMutex<std::collections::HashMap<String, UiSession>>>,
        jobs: Arc<Mutex<std::collections::HashMap<String, crate::server::tools::JobInfo>>>,
        job_webhooks: Arc<AsyncMutex<Vec<String>>>,
    ) -> Self {
        Self {
            graph,
            overlay,
            embedder,
            cross_encoder,
            git,
            lsp_pool,
            tuning,
            embedding_cache,
            ui_sessions,
            jobs,
            job_webhooks,
            diagnostics_port: std::sync::Arc::new(std::sync::atomic::AtomicU16::new(0)),
            workspace: std::path::PathBuf::from("."),
            // Default to empty registries so standalone / sidecar
            // executors (which don't carry a `LainServer`) still
            // construct successfully. `LainMcpServer::with_server`
            // replaces these with the live `Arc`s once the
            // orchestrator is built.
            presence: Arc::new(PresenceRegistry::new()),
            occupancy: Arc::new(OccupancyMap::new()),
            // Default to Skipped so a standalone / sidecar executor
            // (which never runs the spawn) still returns a valid
            // outcome. `LainMcpServer::with_server` swaps in the
            // live `Arc<Mutex<RefreshOutcome>>` from the constructed
            // `LainServer` once the orchestrator is built.
            last_outcome: Arc::new(parking_lot::Mutex::new(
                crate::server::refresh::RefreshOutcome::skipped(),
            )),
            // Set by `with_federation` when the server runs in
            // federation mode; single-workspace executors leave it None
            // and `for_repo` is then a no-op.
            federation: None,
        }
    }

    /// Attach the federation so per-repo tools can be rebound per call.
    pub fn with_federation(
        mut self,
        fed: Arc<crate::server::federation::federated_index::FederatedIndex>,
    ) -> Self {
        self.federation = Some(fed);
        self
    }

    /// A copy of this context whose `graph` and `workspace` point at
    /// `repo_id`'s checkout instead of the construction-time binding.
    ///
    /// Returns `None` when there is no federation, the id does not
    /// parse, or no such repo is registered — callers then keep the
    /// context they already have, which is correct for single-workspace
    /// mode and for the single-repo federation that is already bound to
    /// the right graph.
    pub fn for_repo(&self, repo_id: &str) -> Option<Self> {
        let fed = self.federation.as_ref()?;
        let rid = crate::server::federation::repo_id::RepoId::new(repo_id).ok()?;
        let repo = fed.get_repo(&rid)?;
        let mut bound = self.clone();
        bound.graph = repo.db().clone();
        let root = repo.source().local_path().to_path_buf();
        // Git-backed tools (history, diff, branch status) read through
        // `git`, so it has to follow the repo too — otherwise they keep
        // answering from whichever checkout the server was built
        // against. A repo whose checkout is not a git work tree keeps
        // the existing sensor rather than failing the call.
        if let Ok(sensor) = GitSensor::new(&root) {
            bound.git = Arc::new(Mutex::new(sensor));
        }
        bound.workspace = root;
        Some(bound)
    }

    // `with_presence_and_occupancy` was a second, unused way to install the
    // live registries. `LainMcpServer::with_server` is the one that runs, and
    // it assigns `ctx.presence` / `ctx.occupancy` directly.

    pub fn with_workspace(mut self, workspace: std::path::PathBuf) -> Self {
        self.workspace = workspace;
        self
    }

    /// Remove expired UI sessions. Call periodically to prevent unbounded growth.
    pub async fn cleanup_expired_sessions(&self) {
        let mut guard = self.ui_sessions.lock().await;
        let now = std::time::SystemTime::now();
        guard.retain(|_, session| session.expires_at > now);
    }
}

/// Capability classification — determines what kind of system state a tool may touch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolCapability {
    /// Read-only graph queries and analysis — never modifies graph or overlay.
    ReadOnly,
    /// Writes new nodes/edges to the graph or overlay (structural changes).
    StructuralWrite,
    /// Executes commands, spawns processes, or modifies external state.
    Mutating,
}

/// A handler trait — implement this for each tool.
/// `inventory` collects all implementors via `ToolHandlerEntry`.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Stable tool name — must be unique across all registered tools.
    fn name(&self) -> &'static str;

    /// One-sentence description for the agent strategy and schema registry.
    fn description(&self) -> &'static str;

    /// JSON Schema for the tool's input arguments (Draft-7).
    fn input_schema(&self) -> &'static str;

    /// What kind of state this tool touches.
    fn capability(&self) -> ToolCapability;

    /// Execute the tool. Returns a JSON-encoded string on success.
    async fn call(&self, ctx: &ToolContext, args: &Map<String, Value>) -> Result<String, LainError>;
}

// ─── Inventory registry ───────────────────────────────────────────────────────

inventory::collect!(ToolHandlerEntry);

/// Entry wrapper so `inventory` can store `dyn ToolHandler` trait objects.
pub struct ToolHandlerEntry(pub &'static dyn ToolHandler);

impl std::fmt::Debug for ToolHandlerEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ToolHandlerEntry").field(&self.0.name()).finish()
    }
}

/// The global tool registry populated by `inventory`.
pub struct ToolRegistry;

impl ToolRegistry {
    /// Iterate all registered tools and dispatch by name.
    pub async fn dispatch(
        ctx: &ToolContext,
        name: &str,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        // Bind to the repo the caller resolved, when there is one. The
        // MCP dispatcher injects `repo_id` after resolving the symbol or
        // an explicit argument; without this the id was injected and
        // then ignored, so in a multi-repo federation every per-repo
        // tool read the empty staging placeholder and answered "not
        // found" for symbols that plainly exist.
        let rebound;
        let ctx = match args.get("repo_id").and_then(|v| v.as_str()) {
            Some(rid) => match ctx.for_repo(rid) {
                Some(bound) => {
                    rebound = bound;
                    &rebound
                }
                None => ctx,
            },
            None => ctx,
        };
        for entry in iter::<ToolHandlerEntry>() {
            if entry.0.name() == name {
                return entry.0.call(ctx, args).await;
            }
        }
        Err(LainError::NotFound(format!("Unknown tool: {}", name)))
    }

    /// Collect all tool definitions for MCP schema registration.
    pub fn definitions() -> Vec<crate::server::tools::definitions::ToolDefinition> {
        iter::<ToolHandlerEntry>()
            .map(|entry| {
                let schema: Value = serde_json::from_str(entry.0.input_schema())
                    .unwrap_or_else(|_| serde_json::json!({}));
                crate::server::tools::definitions::ToolDefinition {
                    name: entry.0.name(),
                    description: entry.0.description(),
                    input_schema: schema,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod federation_binding_tests {
    use super::*;
    use crate::federation::federated_index::FederatedIndex;
    use crate::federation::graph_backend::PetgraphBackend;
    use crate::federation::repo_id::RepoId;
    use crate::federation::repo_source::WorkspaceDirSource;
    use crate::schema::{GraphNode, NodeType};

    /// `graph` / `workspace` are bound once at construction, and with
    /// several repos that binding is an empty staging placeholder. The
    /// dispatcher resolves which repo a call targets and injects
    /// `repo_id`; `for_repo` is what turns that id back into the right
    /// graph and checkout. Without it every per-repo tool in a
    /// multi-repo federation answered against an empty graph — a
    /// confident "not found" for symbols that plainly exist.
    #[tokio::test]
    async fn for_repo_rebinds_graph_and_workspace_per_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = Arc::new(FederatedIndex::new(Arc::new(
            PetgraphBackend::new(tmp.path()).unwrap(),
        )));

        // Two repos, each with its own checkout on disk.
        let mut roots = Vec::new();
        for name in ["alpha", "beta"] {
            let dir = tempfile::tempdir().unwrap();
            git2::Repository::init(dir.path()).unwrap();
            let root = dir.path().to_path_buf();
            let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
                WorkspaceDirSource::new(RepoId::new(name).unwrap(), root.clone()).unwrap(),
            );
            fed.add_repo(src, tmp.path()).await.unwrap();
            // Keep the tempdir alive for the length of the test.
            roots.push((name, root, dir));
        }

        // Put a distinct symbol in each repo's own graph.
        for (name, _, _) in &roots {
            let rid = RepoId::new(name).unwrap();
            let repo = fed.get_repo(&rid).expect("repo registered");
            repo.db()
                .upsert_node(GraphNode::new(
                    NodeType::Function,
                    format!("{name}_only"),
                    "src/lib.rs".to_string(),
                ))
                .unwrap();
        }

        // A context bound to an empty placeholder graph, as the
        // multi-repo constructor leaves it.
        let placeholder = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            crate::graph::GraphDatabase::new(&placeholder.path().join("graph.bin")).unwrap(),
            crate::overlay::VolatileOverlay::new(),
            crate::nlp::NlpEmbedder::new_with_threads(0).unwrap(),
            crate::nlp::CrossEncoder::from_dir(std::path::Path::new("/nonexistent")),
            Arc::new(Mutex::new(
                GitSensor::new(&roots[0].1).expect("git sensor"),
            )),
            Arc::new(LspPool::new(&roots[0].1, 1, &crate::tuning::RuntimeConfig::default()).unwrap()),
            Arc::new(TuningConfig::default()),
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AsyncMutex::new(std::collections::HashMap::new())),
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            Arc::new(AsyncMutex::new(Vec::new())),
        )
        .with_federation(Arc::clone(&fed));

        assert_eq!(
            ctx.graph.node_count(),
            0,
            "the multi-repo binding starts on an empty placeholder"
        );

        for (name, root, _) in &roots {
            let bound = ctx.for_repo(name).expect("repo should rebind");
            assert!(
                bound.graph.find_node_by_name(&format!("{name}_only")).is_some(),
                "{name}'s own symbol must resolve after rebinding"
            );
            let other = if *name == "alpha" { "beta" } else { "alpha" };
            assert!(
                bound
                    .graph
                    .find_node_by_name(&format!("{other}_only"))
                    .is_none(),
                "rebinding to {name} must not expose {other}'s symbols"
            );
            assert_eq!(
                bound.workspace, *root,
                "workspace must follow the repo, so relative paths read the right checkout"
            );
        }

        assert!(
            ctx.for_repo("no-such-repo").is_none(),
            "an unknown repo leaves the caller's context alone"
        );
    }
}
