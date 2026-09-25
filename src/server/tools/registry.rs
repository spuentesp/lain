//! Tool registry — inventory-based auto-discovery.
//!
//! Each handler registers itself via `inventory::submit!` at startup.
//! The dispatcher (`ToolRegistry::dispatch`) iterates registered tools by name.
//!
//! Adding a new tool: implement `ToolHandler` in its own module, call
//! `inventory::submit!(ToolHandlerEntry(handler))` at the bottom of the file.
//! No central edit required.

use crate::error::LainError;
use crate::git::AnyGitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use crate::server::annotations::AnnotationRegistry;
use crate::server::presence::{OccupancyMap, PresenceRegistry};
use crate::server::tools::UiSession;
use crate::tuning::TuningConfig;
use async_trait::async_trait;
use inventory::iter;
use parking_lot::{Mutex, RwLock};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

/// Construction dependencies for the tool runtime. Grouping these values
/// prevents the constructor from becoming a positional list as services grow.
pub struct ToolContextDeps {
    pub graph: GraphDatabase,
    pub overlay: VolatileOverlay,
    pub embedder: NlpEmbedder,
    pub cross_encoder: crate::nlp::CrossEncoder,
    pub git: Arc<AnyGitSensor>,
    pub lsp_pool: Arc<LspPool>,
    pub tuning: Arc<TuningConfig>,
    pub embedding_cache: Arc<Mutex<HashMap<String, Vec<f32>>>>,
    pub ui_sessions: crate::server::tools::UiSessionStore,
    pub jobs: Arc<Mutex<HashMap<String, crate::server::tools::JobInfo>>>,
    pub job_webhooks: Arc<AsyncMutex<Vec<String>>>,
}

/// All dependencies a tool handler needs to do its work.
#[derive(Clone)]
pub struct ToolContext {
    pub graph: GraphDatabase,
    pub overlay: VolatileOverlay,
    pub embedder: NlpEmbedder,
    pub cross_encoder: crate::nlp::CrossEncoder,
    pub git: Arc<AnyGitSensor>,
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
    /// Wall-clock nanosecond timestamp at which the parking_lot
    /// `GitSensor` mutex became continuously held (Bug #2 from the
    /// 2026-09-18 postmortem), or `0` if the mutex is free. The
    /// watchdog spawned by [`crate::server::ingest::handles::IngestHandle::start_git_sensor_watchdog`]
    /// publishes this on the free→held transition (CAS, so only the
    /// first observer wins) and clears it on the held→free
    /// transition. `get_health` reads it to surface the hang to
    /// operator tooling (alertmanager, dashboards) without requiring
    /// log scraping. Initialized to a fresh zero atomic so standalone
    /// / sidecar executors that never wire a `LainServer` keep
    /// constructing successfully; `LainMcpServer::with_server`
    /// replaces this with the live atomic from the constructed
    /// `LainServer`'s `IngestHandle`.
    pub git_busy_since_unix_nanos: Arc<std::sync::atomic::AtomicU64>,
    /// Single owner for startup indexing state. Health, discovery, and the
    /// readiness gate read snapshots from this handle.
    pub readiness: crate::server::readiness::ReadinessHandle,
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
    /// Per-repo "indexed" notification for the cold-boot race closure.
    ///
    /// `RepoIndex::indexed_signal` is fired after every successful
    /// `index()` / `index_forced()`. The MCP dispatcher awaits this
    /// with a 200 ms budget when the active repo's per-repo graph is
    /// empty, so a tool call that lands in the cold-boot window wakes
    /// up to a populated graph instead of an empty placeholder. This
    /// closes the cold-boot race whose symptom was the
    /// "Node not found for handle" flake in
    /// `feat_negative_paths_end_to_end` (fixed in commit `3436a51`
    /// together with the test-fixture tempdir-lifetime fix). `None`
    /// in single-workspace mode and in tests that don't wire a
    /// federation — the dispatcher's wait is then a no-op.
    pub indexed_signal: Option<Arc<tokio::sync::Notify>>,
    /// Per-repo annotation store shared with the `LainServer` orchestrator.
    /// Tools that surface architectural context (`explain_symbol`,
    /// `get_blast_radius`) read this to append an `### Open annotations`
    /// section so an agent's first call about a symbol surfaces the
    /// human notes left there. Initialized to a temp-dir-backed
    /// best-effort registry so standalone / sidecar executors that don't
    /// carry a `LainServer` still construct successfully; `with_server`
    /// (or `with_annotations`) swaps in the live registry once the
    /// orchestrator is built. Annotations are read-only here — write
    /// paths stay on the dedicated MCP tools so the dispatcher gate
    /// still applies.
    pub annotations: Arc<AnnotationRegistry>,
    /// Workspace handle shared with the running `LainServer`. None
    /// for standalone / sidecar executors that never wire a
    /// `LainMcpServer`; the dispatcher routes workspace tools off
    /// this when present. PR-fix-Item-1 also reads it from
    /// `get_capabilities` so the advertised tool count is exact
    /// under workspace mode (the previous PR shipped the helper
    /// signature with `workspace_active` but the call sites
    /// hard-coded `false`).
    pub workspaces: Option<Arc<RwLock<crate::federation::workspace::WorkspacesFile>>>,
}

impl ToolContext {
    pub fn from_deps(deps: ToolContextDeps) -> Self {
        let ToolContextDeps {
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
        } = deps;
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
            // Default to a fresh zero atomic. `LainMcpServer::with_server`
            // swaps in the live atomic from the constructed
            // `LainServer`'s `IngestHandle` once the orchestrator is
            // built; until then `get_health` reads 0 ("mutex free").
            git_busy_since_unix_nanos: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            readiness: crate::server::readiness::ReadinessHandle::default(),
            // Set by `with_federation` when the server runs in
            // federation mode; single-workspace executors leave it None
            // and `for_repo` is then a no-op.
            federation: None,
            // Set by `with_indexed_signal` for federation-mode servers;
            // single-workspace executors leave it None and the
            // dispatcher's cold-boot wait is then a no-op.
            indexed_signal: None,
            // Default to a temp-dir-backed best-effort registry so a
            // standalone executor (one that never wires a `LainServer`)
            // can still construct without panicking. `with_annotations`
            // and `LainMcpServer::with_server` swap this for the live
            // registry once the orchestrator is built. No annotation
            // data lives in the temp dir for the default case —
            // `summaries_for_targets` walks an empty store and returns
            // an empty vector, so the appended `### Open annotations`
            // section is suppressed by the same `is_empty()` guard the
            // followup tests pin.
            annotations: AnnotationRegistry::open_best_effort(&std::env::temp_dir()),
            // Default to `None` so standalone / sidecar executors
            // (which never wire a `LainMcpServer`) construct
            // successfully. `LainMcpServer` sets this from its
            // constructor before the executor reaches any tool.
            workspaces: None,
        }
    }

    /// Install the workspace handle shared with `LainMcpServer`.
    /// Used by the MCP dispatcher to expose workspace tools and
    /// (since PR-fix-Item-1) by `get_capabilities` to report an
    /// exact `tool_profile.advertised_count` under workspace mode.
    pub fn with_workspaces(
        mut self,
        workspaces: Arc<RwLock<crate::federation::workspace::WorkspacesFile>>,
    ) -> Self {
        self.workspaces = Some(workspaces);
        self
    }

    /// Install a live annotation registry. `LainMcpServer::with_server`
    /// uses this when it has the orchestrator in hand; tests construct
    /// one off a tempdir directly.
    pub fn with_annotations(mut self, registry: Arc<AnnotationRegistry>) -> Self {
        self.annotations = registry;
        self
    }

    /// Attach the federation so per-repo tools can be rebound per call.
    pub fn with_federation(
        mut self,
        fed: Arc<crate::server::federation::federated_index::FederatedIndex>,
    ) -> Self {
        self.federation = Some(fed);
        self
    }

    /// Attach the active repo's `indexed_signal`. Federation-mode
    /// servers set this once on the single-repo binding; multi-repo
    /// callers go through `for_repo`, which rebinds the signal per
    /// call. The dispatcher's cold-boot race closure depends on it.
    pub fn with_indexed_signal(mut self, signal: Arc<tokio::sync::Notify>) -> Self {
        self.indexed_signal = Some(signal);
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
        // Cold-boot race closure: the active repo's indexed signal is
        // what the dispatcher awaits when this repo's per-repo graph
        // is empty. Without this rebind, the single-repo binding's
        // signal (or `None` in multi-repo mode) would be used for
        // every per-repo call, which is wrong in multi-repo
        // federation.
        bound.indexed_signal = Some(repo.indexed_signal());
        let root = repo.source().local_path().to_path_buf();
        // Git-backed tools (history, diff, branch status) read through
        // `git`, so it has to follow the repo too. Rebind to the repo's
        // own AnyGitSensor.
        bound.git = Arc::clone(repo.git());
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
    async fn call(&self, ctx: &ToolContext, args: &Map<String, Value>)
        -> Result<String, LainError>;
}

// ─── Inventory registry ───────────────────────────────────────────────────────

inventory::collect!(ToolHandlerEntry);

/// Entry wrapper so `inventory` can store `dyn ToolHandler` trait objects.
pub struct ToolHandlerEntry(pub &'static dyn ToolHandler);

impl std::fmt::Debug for ToolHandlerEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ToolHandlerEntry")
            .field(&self.0.name())
            .finish()
    }
}

/// The global tool registry populated by `inventory`.
pub struct ToolRegistry;

impl ToolRegistry {
    /// Budget for the cold-boot race closure: a tool call that lands
    /// while the active repo's per-repo graph is empty (between
    /// `repo.index()` completing and the next `index_forced()`, or in
    /// the first boot window before any index has populated the
    /// graph) waits up to this long for the indexer's
    /// `indexed_signal` to fire. Below the existing
    /// `wait_for_repo_index` polling interval (200 ms) for
    /// `list_repos`, so the test's overall wait budget doesn't grow.
    const COLD_BOOT_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

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
        // Cold-boot race closure: when the per-repo graph is empty,
        // await the active repo's `indexed_signal` with a bounded
        // 200 ms budget so a tool call that lands in the cold-boot
        // window wakes up to a populated graph instead of the empty
        // placeholder. After the first successful `index()` pass
        // the graph has data, `node_count() > 0`, and the wait is
        // skipped — no latency cost in steady state.
        // `indexed_signal` is `Some` only in federation mode after
        // `for_repo` (multi-repo) or the LainServer's single-repo
        // binding (single-repo); in single-workspace mode and tests
        // that don't wire a federation it's `None` and the wait is
        // a no-op.
        //
        // The test fixture's `wait_for_repo_index` previously used
        // `tools_call_text`, which panics on `isError=true`, so the
        // first cold-boot miss turned into a panic instead of a
        // retry. That helper now uses `tools_call_envelope` (see
        // the doc comment there) so the bounded wait below surfaces
        // the actual race window and the test can poll through it.
        if ctx.graph.node_count() == 0 {
            if let Some(signal) = ctx.indexed_signal.as_ref() {
                let _ = tokio::time::timeout(Self::COLD_BOOT_WAIT, signal.notified()).await;
            }
        }
        for entry in iter::<ToolHandlerEntry>() {
            if entry.0.name() == name {
                return entry.0.call(ctx, args).await;
            }
        }
        Err(LainError::InvalidArgument(format!("Unknown tool: {}", name)))
    }

    /// Collect all tool definitions for MCP schema registration.
    ///
    /// Sorted alphabetically by tool name so the inventory iteration
    /// order — which is non-deterministic across linker layouts and
    /// rebuilds — doesn't leak into the wire surface. The
    /// `lain schema dump` artifact (docs/tool-schema.json) and the
    /// live `tools/list` response both flow through this method, so
    /// sorting here pins both sides to the same byte-order contract
    /// (`tests/schema_dump_smoke::live_tools_list_byte_matches_on_disk_schema_dump`).
    pub fn definitions() -> Vec<crate::server::tools::definitions::ToolDefinition> {
        let mut defs: Vec<crate::server::tools::definitions::ToolDefinition> = iter::<
            ToolHandlerEntry,
        >()
        .map(|entry| {
            let schema: Value = serde_json::from_str(entry.0.input_schema())
                .unwrap_or_else(|_| serde_json::json!({}));
            crate::server::tools::definitions::ToolDefinition {
                name: entry.0.name(),
                description: entry.0.description(),
                input_schema: schema,
                readiness: crate::server::tools::definitions::readiness_requirement(entry.0.name())
                    .expect("every registered tool must declare a readiness requirement"),
            }
        })
        .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
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
        let ctx = ToolContext::from_deps(ToolContextDeps {
            graph: crate::graph::GraphDatabase::new(&placeholder.path().join("graph.bin")).unwrap(),
            overlay: crate::overlay::VolatileOverlay::new(),
            embedder: crate::nlp::NlpEmbedder::new_with_threads(0).unwrap(),
            cross_encoder: crate::nlp::CrossEncoder::from_dir(std::path::Path::new("/nonexistent")),
            git: Arc::new(AnyGitSensor::from_env(&roots[0].1).expect("git sensor")),
            lsp_pool: Arc::new(
                LspPool::new(&roots[0].1, 1, &crate::tuning::RuntimeConfig::default()).unwrap(),
            ),
            tuning: Arc::new(TuningConfig::default()),
            embedding_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
            ui_sessions: Arc::new(AsyncMutex::new(std::collections::HashMap::new())),
            jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            job_webhooks: Arc::new(AsyncMutex::new(Vec::new())),
        })
        .with_federation(Arc::clone(&fed));

        assert_eq!(
            ctx.graph.node_count(),
            0,
            "the multi-repo binding starts on an empty placeholder"
        );

        for (name, root, _) in &roots {
            let bound = ctx.for_repo(name).expect("repo should rebind");
            assert!(
                bound
                    .graph
                    .find_node_by_name(&format!("{name}_only"))
                    .is_some(),
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
