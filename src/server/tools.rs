//! Modular tool execution system
//!
//! Follows SOLID and DRY principles by delegating logic to specialized handlers.

pub mod definitions;
pub mod handlers;
#[cfg(test)]
pub mod proptest_helpers;
pub mod registry;
pub mod utils;
#[cfg(test)]
pub mod utils_tests;

use crate::error::LainError;
use crate::federation::repo_id::RepoId;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use crate::server::tools::registry::{ToolContext, ToolContextDeps, ToolRegistry};
use crate::tuning::TuningConfig;
use parking_lot::Mutex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task;
use tracing::{error, info};
use uuid::Uuid;

/// Shared registry types used by interactive tool responses. Keeping these
/// aliases in the tools module gives handlers a stable vocabulary and avoids
/// repeating the implementation details of the session store in every API.
pub type UiSessionStore = Arc<AsyncMutex<HashMap<String, UiSession>>>;
pub type UiLink<'a> = Option<(&'a UiSessionStore, u16, std::time::Duration)>;

pub use definitions::ToolDefinition;
// `use utils::*;` was only needed by `augment_knowledge`'s
// `resolve_node_at_location` call, which was removed with it.

#[derive(Clone, Serialize, Deserialize)]
pub enum JobState {
    Running,
    Completed {
        success: bool,
        output: Option<String>,
        error: Option<String>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: String,
    pub created_at: std::time::SystemTime,
    pub state: JobState,
}

/// UI session for interactive visualizations (blast radius, coupling heatmap, call chain)
#[derive(Clone, Serialize, Deserialize)]
pub struct UiSession {
    pub id: String,
    pub session_type: String, // "blast-radius", "coupling", "call-chain"
    pub created_at: std::time::SystemTime,
    pub data: UiSessionData,
    pub expires_at: std::time::SystemTime,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum UiSessionData {
    BlastRadius {
        symbol: String,
        nodes: Vec<BlastRadiusNode>,
    },
    Coupling {
        symbol: String,
        matrix: Vec<Vec<f32>>,
        files: Vec<String>,
    },
    CallChain {
        from: String,
        to: String,
        path: Vec<String>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct BlastRadiusNode {
    pub id: String,
    pub name: String,
    pub node_type: String,
    pub path: String,
    pub depth: u32,
    pub is_direct: bool,
}

/// Tool executor - orchestrates MCP tool execution by delegating to specialized handlers.
#[derive(Clone)]
pub struct ToolExecutor {
    pub ctx: ToolContext,
    jobs: Arc<Mutex<HashMap<String, JobInfo>>>,
    job_webhooks: Arc<AsyncMutex<Vec<String>>>,
    pub tuning: Arc<TuningConfig>,
}

/// Dependencies needed to construct a full tool executor.
///
/// Keeping these inputs together makes the construction contract explicit and
/// leaves room for adding a subsystem without another positional argument.
pub struct ToolExecutorConfig {
    pub graph: GraphDatabase,
    pub overlay: VolatileOverlay,
    pub embedder: NlpEmbedder,
    pub cross_encoder: crate::nlp::CrossEncoder,
    pub git: Arc<Mutex<GitSensor>>,
    pub lsp_pool: Arc<LspPool>,
    pub tuning: Arc<TuningConfig>,
    pub workspace: std::path::PathBuf,
}

impl ToolExecutor {
    pub fn graph(&self) -> &GraphDatabase {
        &self.ctx.graph
    }
    pub fn overlay(&self) -> &VolatileOverlay {
        &self.ctx.overlay
    }
    pub fn embedder(&self) -> &NlpEmbedder {
        &self.ctx.embedder
    }
    pub fn ui_sessions(&self) -> &AsyncMutex<HashMap<String, UiSession>> {
        &self.ctx.ui_sessions
    }
    /// Record the port the HTTP transport is actually listening on, so
    /// tool output can link to `/ui/...` sessions. 0 (the default) means
    /// "no UI server" (stdio mode) — handlers then skip the link instead
    /// of emitting a dead URL.
    pub fn set_diagnostics_port(&self, port: u16) {
        self.ctx
            .diagnostics_port
            .store(port, std::sync::atomic::Ordering::Relaxed);
    }
}

impl ToolExecutor {
    pub fn new(config: ToolExecutorConfig) -> Self {
        let ToolExecutorConfig {
            graph,
            overlay,
            embedder,
            cross_encoder,
            git,
            lsp_pool,
            tuning,
            workspace,
        } = config;
        let jobs_registry = Arc::new(Mutex::new(HashMap::<String, JobInfo>::new()));
        let webhooks = Arc::new(AsyncMutex::new(Vec::new()));

        let ctx = ToolContext::from_deps(ToolContextDeps {
            graph,
            overlay,
            embedder,
            cross_encoder,
            git,
            lsp_pool,
            tuning: Arc::clone(&tuning),
            embedding_cache: Arc::new(Mutex::new(HashMap::new())),
            ui_sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            jobs: Arc::clone(&jobs_registry),
            job_webhooks: Arc::clone(&webhooks),
        })
        .with_workspace(workspace);

        // Snapshot persistence (optional, for resumeability)
        let jobs_path =
            std::env::var("LAIN_JOB_STORE").unwrap_or_else(|_| ".lain/jobs.json".into());
        if let Ok(contents) = std::fs::read_to_string(&jobs_path) {
            let jobs_registry = Arc::clone(&jobs_registry);
            let jobs_path_for_log = jobs_path.clone();
            task::spawn(async move {
                match serde_json::from_str::<Vec<JobInfo>>(&contents) {
                    Ok(vec) => {
                        let mut guard = jobs_registry.lock();
                        for j in vec {
                            guard.insert(j.id.clone(), j);
                        }
                    }
                    // A store that exists but will not parse means jobs
                    // were lost, most likely to an interrupted write.
                    // Skipping in silence made that indistinguishable
                    // from having had no jobs at all.
                    Err(e) => tracing::warn!(
                        "job store at {jobs_path_for_log} could not be read ({e}); \
                         previously running jobs will not be resumed"
                    ),
                }
            });
        }

        Self {
            ctx,
            jobs: jobs_registry,
            job_webhooks: webhooks,
            tuning,
        }
    }

    /// Construct a minimal read-only executor for the sidecar runtime.
    ///
    /// Sidecar processes never start LSP multiplexers, never load a bi-encoder
    /// model, and never open the git repository — they only need a graph view
    /// and an overlay that the owner feeds via `/overlay/subscribe`. Any tool
    /// handler that touches a heavier subsystem will fail at call time and
    /// surface a clear error to the MCP client.
    ///
    /// `workspace` is preferred for the git/LSP fallbacks when it points at a
    /// real repository; otherwise the constructor falls back to a tmpdir that
    /// has been initialized as a git repo so the read-only `ToolContext` can
    /// still satisfy the `GitSensor` contract.
    pub fn new_read_only(
        graph: GraphDatabase,
        overlay: VolatileOverlay,
        workspace: std::path::PathBuf,
    ) -> Self {
        let jobs_registry = Arc::new(Mutex::new(HashMap::<String, JobInfo>::new()));
        let webhooks = Arc::new(AsyncMutex::new(Vec::new()));
        let tuning = Arc::new(TuningConfig::default());

        // The sidecar's read-only `ToolContext` carries a git sensor and LSP
        // pool because the rest of the executor plumbing is shared with the
        // owner. A sidecar should never call into the git/LSP handlers, so
        // any failure to construct those subsystems must not block the
        // sidecar from booting. Use the real workspace when it is a git repo
        // (the normal case in production); otherwise initialize a stub repo
        // under the system temp dir so the constructor still succeeds in
        // tests and minimal workspaces.
        let git_root = match crate::git::GitSensor::new(&workspace) {
            Ok(_) => workspace.clone(),
            Err(_) => match Self::ensure_stub_git_repo() {
                Ok(path) => path,
                Err(e) => panic!(
                    "sidecar stub git sensor setup failed: workspace={:?} error={}",
                    workspace, e
                ),
            },
        };
        let git = Arc::new(Mutex::new(
            crate::git::GitSensor::new(&git_root)
                .expect("sidecar git sensor must succeed after stub init"),
        ));
        let runtime = crate::tuning::load_tuning_config(&workspace).runtime;
        let lsp_root = match crate::lsp::LspPool::new(&workspace, 1, &runtime) {
            Ok(pool) => pool,
            Err(_) => {
                // `LspPool::new` only fails on unexpected errors; an empty
                // multiplex registry is fine for the sidecar, so retry on
                // the stub root if the workspace cannot be used.
                let _ = Self::ensure_stub_git_repo();
                crate::lsp::LspPool::new(&git_root, 1, &runtime).unwrap_or_else(|e| {
                    panic!("sidecar lsp pool fallback failed: {e}");
                })
            }
        };
        let lsp_pool = Arc::new(lsp_root);

        let ctx = ToolContext::from_deps(ToolContextDeps {
            graph,
            overlay,
            embedder: NlpEmbedder::new_stub(),
            cross_encoder: crate::nlp::CrossEncoder::from_dir(std::path::Path::new("/nonexistent")),
            git,
            lsp_pool,
            tuning: Arc::clone(&tuning),
            embedding_cache: Arc::new(Mutex::new(HashMap::new())),
            ui_sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            jobs: Arc::clone(&jobs_registry),
            job_webhooks: Arc::clone(&webhooks),
        })
        .with_workspace(workspace);

        // A read-only executor never indexes: the sidecar answers from the
        // owner's already-persisted graph (streamed updates land in the
        // overlay, not here), and the doctor probe deliberately opens an
        // empty graph to exercise the MCP handshake only. Leaving this
        // handle at its `warming_up` default would gate every graph tool
        // forever, since nothing else ever transitions it.
        ctx.readiness
            .ready(ctx.graph.get_last_commit().ok().flatten());

        Self {
            ctx,
            jobs: jobs_registry,
            job_webhooks: webhooks,
            tuning,
        }
    }

    /// Persist the job registry for resumeability.
    ///
    /// This used to `fs::write` directly and discard the result with
    /// `let _ =`, then return `Ok(())` either way — so it reported
    /// success having written nothing, and a crash mid-write left a
    /// truncated `jobs.json` that the loader silently skipped. Same
    /// shape as the torn audit line: a partial write plus a tolerant
    /// reader is indistinguishable from "there were no jobs".
    async fn persist_jobs_snapshot(
        jobs: Arc<Mutex<HashMap<String, JobInfo>>>,
    ) -> Result<(), LainError> {
        let path = std::env::var("LAIN_JOB_STORE").unwrap_or_else(|_| ".lain/jobs.json".into());
        let vec: Vec<JobInfo> = {
            let guard = jobs.lock();
            guard.values().cloned().collect()
        };
        let json = serde_json::to_string(&vec)
            .map_err(|e| LainError::Serialization(format!("jobs snapshot: {e}")))?;
        crate::cli::io::write_file_atomic(std::path::Path::new(&path), json)
            .map_err(|e| LainError::Io(format!("write {path}: {e}")))?;
        Ok(())
    }

    /// Create a throwaway git repository under the system temp dir and
    /// return its root path. Used by the sidecar's read-only `ToolContext`
    /// when the configured workspace is not a git repo (e.g. in tests) so
    /// that `GitSensor::new` always has a valid `.git` to open.
    fn ensure_stub_git_repo() -> Result<std::path::PathBuf, String> {
        use std::sync::Mutex;
        use std::sync::OnceLock;

        static STUB: OnceLock<Mutex<Result<std::path::PathBuf, String>>> = OnceLock::new();
        let cell = STUB.get_or_init(|| Mutex::new(Err("init pending".into())));
        let mut guard = cell.lock().expect("stub repo lock");
        if let Ok(path) = guard.as_ref() {
            return Ok(path.clone());
        }
        let dir = std::env::temp_dir().join("lain-sidecar-stub-git");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create stub dir: {e}"))?;
        git2::Repository::init(&dir).map_err(|e| format!("git init stub: {e}"))?;
        *guard = Ok(dir.clone());
        Ok(dir)
    }

    /// Primary dispatcher for all MCP tools
    pub async fn call(
        &self,
        name: &str,
        arguments: Option<&Map<String, Value>>,
    ) -> Result<String, LainError> {
        // Background execution support
        if let Some(args) = arguments {
            if let Some(bg) = args.get("background") {
                if bg.as_bool().unwrap_or(false) {
                    let mut owned = args.clone();
                    owned.remove("background");
                    let exec = self.clone();
                    let name_owned = name.to_string();

                    const MAX_CONCURRENT_JOBS: usize = 10;
                    {
                        let guard = self.jobs.lock();
                        let running = guard
                            .values()
                            .filter(|j| matches!(j.state, JobState::Running))
                            .count();
                        if running >= MAX_CONCURRENT_JOBS {
                            return Err(LainError::Mcp(format!(
                                "Too many concurrent jobs (max {})",
                                MAX_CONCURRENT_JOBS
                            )));
                        }
                    }

                    let job_id = Uuid::new_v4().to_string();
                    let job = JobInfo {
                        id: job_id.clone(),
                        created_at: std::time::SystemTime::now(),
                        state: JobState::Running,
                    };

                    {
                        let mut guard = self.jobs.lock();
                        guard.insert(job_id.clone(), job.clone());
                    }

                    let jobs_registry = Arc::clone(&self.jobs);
                    let webhooks = Arc::clone(&self.job_webhooks);
                    let job_id_clone = job_id.clone();
                    task::spawn(async move {
                        let res = exec.call_inner(&name_owned, Some(&owned)).await;
                        {
                            let mut guard = jobs_registry.lock();
                            if let Some(j) = guard.get_mut(&job_id_clone) {
                                match &res {
                                    Ok(out) => {
                                        j.state = JobState::Completed {
                                            success: true,
                                            output: Some(out.clone()),
                                            error: None,
                                        }
                                    }
                                    Err(e) => {
                                        j.state = JobState::Completed {
                                            success: false,
                                            output: None,
                                            error: Some(e.to_string()),
                                        }
                                    }
                                }
                            }
                        } // guard dropped here — must release before webhook/persist

                        let hooks = {
                            let h = webhooks.lock().await;
                            h.clone()
                        };
                        if !hooks.is_empty() {
                            let client = Client::new();
                            let payload = match &res {
                                Ok(out) => {
                                    json!({ "job_id": job_id_clone, "state": "completed", "output": out })
                                }
                                Err(e) => {
                                    json!({ "job_id": job_id_clone, "state": "failed", "error": e.to_string() })
                                }
                            };
                            for url in hooks {
                                let _ = client.post(&url).json(&payload).send().await;
                            }
                        }
                        if let Err(e) =
                            Self::persist_jobs_snapshot(Arc::clone(&jobs_registry)).await
                        {
                            tracing::warn!("job snapshot not persisted: {e}");
                        }
                    });

                    return Ok(format!("{{\"job_id\":\"{}\"}}", job_id));
                }
            }
        }

        return self.call_inner(name, arguments).await;
    }

    async fn call_inner(
        &self,
        name: &str,
        arguments: Option<&Map<String, Value>>,
    ) -> Result<String, LainError> {
        let args = arguments.cloned().unwrap_or_default();

        // Special executor methods — not registered as ToolHandlers
        match name {
            "get_health" => return self.get_health().await,
            "get_capabilities" => return self.get_capabilities(),
            "get_agent_strategy" => return self.get_agent_strategy(),
            "install_language_server" => {
                // Two arg shapes are accepted by the same tool name:
                //   1. { "language": "rust" } — legacy single install.
                //   2. { "extensions": ["rs", "py", "go", "auto"] } —
                //      batched install; `"auto"` resolves to whatever
                //      languages the tracked files use.
                // When both are present, `extensions` wins (it's a
                // superset of `language`). When neither is present,
                // we report the error explicitly instead of falling
                // back to a misleading empty install.
                let lang = arguments
                    .and_then(|a| a.get("language").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let exts: Option<Vec<String>> = arguments
                    .and_then(|a| a.get("extensions").and_then(|v| v.as_array()))
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    });
                return match exts {
                    Some(list) if !list.is_empty() => self.install_language_servers(&list).await,
                    _ => self.install_language_server(lang).await,
                };
            }
            "register_job_webhook" => {
                let url = arguments
                    .and_then(|a| a.get("url").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let mut hooks = self.job_webhooks.lock().await;
                if !hooks.contains(&url.to_string()) {
                    hooks.push(url.to_string());
                }
                return Ok(format!("Webhook registered: {}", url));
            }
            "get_job_status" => {
                let job_id = arguments
                    .and_then(|a| a.get("job_id").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let guard = self.jobs.lock();
                match guard.get(job_id) {
                    Some(job) => return Ok(serde_json::to_string(job).unwrap_or_default()),
                    None => return Err(LainError::NotFound(format!("Job not found: {}", job_id))),
                }
            }
            "debug_sleep" => {
                let secs = args.get("secs").and_then(|v| v.as_u64()).unwrap_or(1);
                tokio::time::sleep(tokio::time::Duration::from_secs(secs)).await;
                return Ok(format!("Slept for {} second(s)", secs));
            }
            _ => {}
        }

        // Delegate to inventory-based registry
        ToolRegistry::dispatch(&self.ctx, name, &args).await
    }

    /// `pub(crate)` (not just called from `call_inner`) so the MCP
    /// handler's capability-change notification can serialize the exact
    /// same projection instead of computing a second one.
    pub(crate) fn get_capabilities(&self) -> Result<String, LainError> {
        use crate::server::readiness::{
            Capabilities, Capability, CapabilityState, IndexState, SCHEMA_VERSION,
        };

        fn structural_state(index_state: IndexState) -> CapabilityState {
            match index_state {
                IndexState::Ready => CapabilityState::Ready,
                IndexState::WarmingUp => CapabilityState::WarmingUp,
                IndexState::UnavailableError => CapabilityState::UnavailableError,
            }
        }

        /// How bad a structural `CapabilityState` is, for picking the
        /// worst-of across repos when aggregating. Only `Ready` /
        /// `WarmingUp` / `UnavailableError` are ever produced by
        /// `structural_state` above; `StaleUsable` and
        /// `UnavailableOptional` rank alongside `WarmingUp` for
        /// completeness even though they don't occur here today.
        fn badness(state: CapabilityState) -> u8 {
            use CapabilityState::*;
            match state {
                Ready => 0,
                WarmingUp | StaleUsable | UnavailableOptional => 1,
                UnavailableError => 2,
            }
        }

        fn capabilities_for(
            structural: CapabilityState,
            retry_after_ms: Option<u64>,
            semantic_stub: bool,
        ) -> Capabilities {
            let mut symbols = Capability::new(structural, false);
            let mut call_graph = Capability::new(structural, false);
            if structural == CapabilityState::WarmingUp {
                symbols.retry_after_ms = retry_after_ms;
                call_graph.retry_after_ms = retry_after_ms;
            }
            let semantic_search = if semantic_stub {
                Capability::new(CapabilityState::UnavailableOptional, true)
            } else {
                Capability::new(structural, true)
            };
            Capabilities {
                symbols,
                call_graph,
                git_history: Capability::new(CapabilityState::Ready, false),
                semantic_search,
            }
        }

        let semantic_stub = self.ctx.embedder.is_stub();

        // Federation mode: each repo owns its own RepoHealth, so
        // `capabilities` (kept for backward compatibility) becomes the
        // worst-of aggregate across every loaded repo, and a new
        // `repositories` array (sorted by id, so the shape is
        // deterministic) carries each repo's own state. AGENT_UX_ROADMAP.md
        // Milestone 4 step 8.
        if let Some(fed) = self.ctx.federation.as_ref() {
            use crate::server::federation::readiness::repo_health_to_snapshot;

            // M4 step 8 deep-fields: per-repo `indexed_signal`,
            // `last_indexed_commit`, `last_indexed_at_unix_ms`,
            // `outstanding_files`, `staleness` — sourced from
            // `FederatedIndex::per_repo_readiness` so the snapshot
            // path and the wire payload can't drift.
            let readiness = fed.per_repo_readiness();
            let readiness_by_id: std::collections::HashMap<RepoId, _> = readiness
                .into_iter()
                .map(|r| (r.repo_id.clone(), r))
                .collect();

            let mut repos = fed.list_repos();
            repos.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));

            let mut worst = CapabilityState::Ready;
            let mut worst_retry_after_ms: Option<u64> = None;
            let repositories: Vec<serde_json::Value> = repos
                .iter()
                .map(|(id, health)| {
                    let snapshot = repo_health_to_snapshot(*health);
                    let structural = structural_state(snapshot.state);
                    if badness(structural) > badness(worst) {
                        worst = structural;
                        worst_retry_after_ms = snapshot.retry_after_ms;
                    }
                    let caps = capabilities_for(structural, snapshot.retry_after_ms, semantic_stub);
                    let r = readiness_by_id.get(id);
                    serde_json::json!({
                        "id": id.as_str(),
                        "capabilities": caps,
                        "indexed_signal": r.map(|r| r.indexed_signal).unwrap_or(false),
                        "last_indexed_commit": r.and_then(|r| r.last_indexed_commit.clone()),
                        "last_indexed_at_unix_ms": r.and_then(|r| r.last_indexed_at_unix_ms),
                        "outstanding_files": r.map(|r| r.outstanding_files).unwrap_or(0),
                        "staleness": r.map(|r| serde_json::to_value(r.staleness).ok())
                            .and_then(|v| v)
                            .unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect();
            let aggregate = capabilities_for(worst, worst_retry_after_ms, semantic_stub);

            return serde_json::to_string(&serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "server_version": env!("CARGO_PKG_VERSION"),
                "repository": self.ctx.workspace.file_name().map(|name| name.to_string_lossy()),
                "capabilities": aggregate,
                "repositories": repositories,
            }))
            .map_err(Into::into);
        }

        let lifecycle = self.ctx.readiness.snapshot();
        let structural = structural_state(lifecycle.state);
        let capabilities = capabilities_for(structural, lifecycle.retry_after_ms, semantic_stub);
        serde_json::to_string(&serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "server_version": env!("CARGO_PKG_VERSION"),
            "repository": self.ctx.workspace.file_name().map(|name| name.to_string_lossy()),
            "capabilities": capabilities,
            "freshness": {
                "head": lifecycle.target_commit,
                "indexed_commit": lifecycle.indexed_commit,
                "working_tree_overlay": lifecycle.state == IndexState::Ready,
            },
            "indexing": lifecycle,
        }))
        .map_err(Into::into)
    }

    pub async fn get_health(&self) -> Result<String, LainError> {
        let (nodes, edges) = self.ctx.graph.get_stats();
        let last_commit = self
            .ctx
            .graph
            .get_last_commit()?
            .unwrap_or_else(|| "None".to_string());
        let overlay_stats = self.ctx.overlay.stats();

        let embedder_status = if self.ctx.embedder.is_stub() {
            "Not loaded (semantic search unavailable)".to_string()
        } else {
            format!("Loaded ({}d embeddings)", self.ctx.embedder.embedding_dim())
        };

        // Live LSP-failure count: sum every repo's
        // `last_overlay_lsp_failures` counter at call time. This
        // includes watcher-driven refreshes (which never touch the
        // cached `RefreshOutcome::lsp_failures_last_cycle`) so the
        // banner reflects the actual current state of the
        // federation, not the most recent sync_state result.
        //
        // Falls back to the cached `outcome.lsp_failures_last_cycle`
        // when the federation isn't wired in (single-repo executor
        // without a `FederatedIndex`).
        let live_lsp_failures: u32 = match self.ctx.federation.as_ref() {
            Some(fed) => fed
                .list_repos()
                .into_iter()
                .filter_map(|(id, _)| fed.get_repo(&id))
                .map(|repo| repo.last_overlay_lsp_failures())
                .sum(),
            None => 0,
        };

        // Surface the resolved workspace so callers can confirm
        // `--workspace auto` (or any other resolution path) picked the right
        // repo. This is the field MCP clients read back to verify the server
        // is indexing the project they expected.
        let workspace_display = self.ctx.workspace.display().to_string();

        // "X commits behind HEAD" — without it the bare SHA is a
        // confident-but-meaningless number. Run `git rev-list --count`
        // against the workspace; on failure (no git, not a repo) fall
        // back to the SHA-only display.
        let commit_status = match std::process::Command::new("git")
            .args(["-C", self.ctx.workspace.to_str().unwrap_or(".")])
            .args(["rev-list", "--count", &format!("{}..HEAD", last_commit)])
            .output()
        {
            Ok(out) if out.status.success() => {
                let count = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if count == "0" {
                    format!("{} (current)", last_commit)
                } else {
                    format!("{} ({} commits behind HEAD)", last_commit, count)
                }
            }
            _ => last_commit.clone(),
        };

        // Status reflects the last refresh outcome. Printing
        // `Operational ✅` beside a re-index failure in the same
        // payload is how a two-day-old graph went unnoticed.
        let degraded = self.ctx.last_outcome.lock().is_degraded();
        let status = if degraded {
            "Degraded ⚠ (serving a stale graph — see the warning below)"
        } else {
            "Operational ✅"
        };
        let mut output = format!(
            "## Lain Server Health\n\n- **Workspace:** {}\n- **Build:** {}\n- **Status:** {}\n- **Static Nodes:** {}\n- **Static Edges:** {}\n- **Volatile Nodes (Overlay):** {}\n- **Last Enriched Commit:** {}\n- **NLP Model:** {}\n",
            workspace_display,
            crate::server::build_info::summary(),
            status,
            nodes,
            edges,
            overlay_stats.node_count,
            commit_status,
            embedder_status
        );

        // Last refresh outcome (from the spawn in run_stdio / run_http).
        // Step 1 of the staleness fix: the re-index failure was previously
        // invisible because it only went to tracing::warn and stderr,
        // neither of which a stdio MCP client surfaces to the model.
        // This is the in-tool-output visibility path.
        if let Some(line) = self.ctx.last_outcome.lock().banner_line() {
            output.push_str(&format!("- **{line}**\n"));
        }

        // LSP-failure banner from the most recent `sync_overlay` cycle.
        // Independent of `banner_line()`: an `Ok` refresh can still
        // leave overlay gaps when the LSP returned no symbols for some
        // files (cold start, missing language server, etc.). The
        // per-file cause is logged at `tracing::warn!` by
        // `RepoIndex::process_overlay_change`; this banner is the
        // aggregate count operators see in `get_health` without
        // having to grep logs.
        //
        // Uses the live count summed across the federation's repos at
        // call time (so watcher-driven refreshes, which never touch
        // the cached `RefreshOutcome::lsp_failures_last_cycle`, are
        // included). The cached field is the value non-`get_health`
        // callers see.
        if live_lsp_failures > 0 {
            output.push_str(&format!(
                "- **⚠ overlay refresh: {live_lsp_failures} file(s) skipped due to LSP \
                 unavailability; overlay coverage is partial this cycle**\n"
            ));
        }

        // Edge-type histogram — operators need this to tell whether the
        // call graph (Calls, Uses) is populated, or whether the indexer
        // only produced the cheaper-to-extract structural edges
        // (Contains, CoChangedWith, etc.). Without it, "every impact
        // query returns nothing" looks like a tool bug but is in fact
        // a missing-data bug; the histogram makes the data
        // visible. Sorted alphabetically by EdgeType Debug name for
        // stable output across runs.
        let edge_hist = self.ctx.graph.edge_counts_by_type();
        if !edge_hist.is_empty() {
            output.push_str("\n### Edge counts by type\n");
            for (kind, count) in &edge_hist {
                output.push_str(&format!("- **{kind}**: {count}\n"));
            }
        }

        output.push_str("\n### Language Support\n");
        let langs = {
            let lsp = self.ctx.lsp_pool.next();
            let lsp_guard = lsp.lock().await;
            lsp_guard.get_supported_languages()
        };

        let mut seen_binaries = std::collections::HashSet::new();
        for (_, binary, available) in langs {
            if seen_binaries.contains(&binary) {
                continue;
            }
            seen_binaries.insert(binary.clone());

            let status = if available { "✅" } else { "❌ (Missing)" };
            output.push_str(&format!("- **{}**: {}\n", binary, status));
        }

        Ok(output)
    }

    async fn install_language_server(&self, language: &str) -> Result<String, LainError> {
        info!("Requesting installation of LSP for: {}", language);

        // Run synchronously so callers see the real result, not a fake
        // success message. Background install was hiding all install failures.
        let lsp = Arc::clone(&self.ctx.lsp_pool.next());
        let mut lsp_guard = lsp.lock().await;
        lsp_guard.install_server(language).await.map_err(|e| {
            error!("LSP installation for {} failed: {}", language, e);
            e
        })
    }

    /// Batched LSP installation. Each entry in `extensions` runs
    /// through [`LspMultiplexer::install_servers`] — a single
    /// failure never blocks the rest of the batch, and the response
    /// carries a per-entry outcome enum (`installed`,
    /// `already_installed`, `unknown_ext`, `no_install_cmd`,
    /// `failed`) so an agent can decide whether to retry.
    ///
    /// Special entry `"auto"` is resolved against the workspace's
    /// tracked files: every distinct extension that maps to a known
    /// language server becomes a candidate. The dedup hits the
    /// same registry lookup `install_server` would, so an operator
    /// running `install_language_servers(["auto"])` gets the same
    /// set as if they had hand-listed every language their
    /// repo uses.
    async fn install_language_servers(&self, extensions: &[String]) -> Result<String, LainError> {
        let resolved: Vec<String> = if extensions.iter().any(|e| e == "auto") {
            // `auto` expands to the workspace's tracked-file extensions.
            // We need a tracked-file list and the registry. The
            // `install_language_server` tool was previously single-tier
            // and not stdio-context-aware; here we go through git's
            // `get_all_tracked_files` (which is the same path the indexer
            // uses, so we share its git config + cache).
            self.resolve_auto_extensions()
        } else {
            extensions.to_vec()
        };

        info!(
            "Requesting batched LSP install for: {:?} (after auto-resolve)",
            resolved
        );
        let refs: Vec<&str> = resolved.iter().map(|s| s.as_str()).collect();
        let lsp = Arc::clone(&self.ctx.lsp_pool.next());
        let mut lsp_guard = lsp.lock().await;
        let results = lsp_guard.install_servers(&refs).await;

        // Pretty-print: one line per entry. Operators get a quick
        // visual scan; agents can JSON-parse if they want a structured
        // shape (the tool envelope preserves the raw array).
        let mut out = String::new();
        out.push_str(&format!("Install batch ({} request(s)):\n", results.len()));
        for r in &results {
            out.push_str(&format!(
                "  - {:>10}  {:?}  {}\n",
                r.ext, r.status, r.message
            ));
        }
        Ok(out)
    }

    /// Walk git-tracked files for the workspace and return the
    /// sorted, deduplicated set of extensions the LSP registry
    /// recognises. Errors during git discovery (e.g. a non-git
    /// workspace) bubble up as a typed `LainError::Config` so the
    /// operator gets a clear message instead of a silently-empty
    /// batch.
    fn resolve_auto_extensions(&self) -> Vec<String> {
        let workspace = self.ctx.workspace.clone();
        let lsp = Arc::clone(&self.ctx.lsp_pool.next());
        // Probe registry first (synchronous, just reads the inner HashMap).
        let known = lsp
            .try_lock()
            .map(|g| g.known_extensions())
            .unwrap_or_default();
        let git_sensor = crate::git::GitSensor::new(&workspace);
        let tracked = match git_sensor {
            Ok(g) => g.get_all_tracked_files().unwrap_or_default(),
            Err(e) => {
                tracing::warn!(
                    "install_language_servers([auto]): git sensor unavailable: {e}; returning []"
                );
                return Vec::new();
            }
        };
        crate::server::lsp::detect_extensions_from_files(&tracked, &known)
    }

    fn get_agent_strategy(&self) -> Result<String, LainError> {
        // Build strategy from registered tool capabilities
        let tools = ToolRegistry::definitions();
        let mut sections = vec![
            "# AI Agent Strategy Guide for Lain\n".to_string(),
            "Lain is a code analysis engine that maintains a graph of your codebase. Use it to understand architecture, trace dependencies, and assess impact before making changes.\n".to_string(),
            "## Core Philosophy\n".to_string(),
            "- **Start broad, zoom deep**: Use layered maps and anchors to find the right part, then blast radius to understand ripple effects.\n".to_string(),
            "- **Pattern edges over names**: Named queries like `get_call_chain` and `semantic_search` find connections that keyword search misses.\n".to_string(),
            "- **Offline-first**: All analysis runs on local data. No LLM API needed for structural queries.\n".to_string(),
            "\n## Recommended Tool Sequence\n\n".to_string(),
        ];

        let mut readonly = Vec::new();
        let mut structural = Vec::new();
        let mut mutating = Vec::new();

        let excluded = [
            "get_health",
            "get_agent_strategy",
            "install_language_server",
            "query_graph",
        ];
        let readonly_set = [
            "explore_architecture",
            "list_entry_points",
            "compare_modules",
            "architectural_observations",
            "trace_dependency",
            "get_call_chain",
            "navigate_to_anchor",
            "get_layered_map",
            "get_master_map",
            "semantic_search",
            "find_anchors",
            "get_anchor_score",
            "get_context_depth",
            "find_dead_code",
            "explain_symbol",
            "suggest_refactor_targets",
            "get_context_for_prompt",
            "get_code_snippet",
            "get_call_sites",
            "find_untested_functions",
            "get_test_template",
            "find_test_file",
            "get_coverage_summary",
            "get_cross_runtime_callers",
            "describe_schema",
        ];
        let structural_set = [
            "add_comment",
            "tag_node",
            "update_node_metadata",
            "insert_reference_edge",
        ];

        for t in tools.iter().filter(|t| !excluded.contains(&t.name)) {
            if readonly_set.contains(&t.name) {
                readonly.push(t);
            } else if structural_set.contains(&t.name) {
                structural.push(t);
            } else {
                mutating.push(t);
            }
        }

        sections.push("### Read-Only (Safe — No State Changes)\n".to_string());
        for t in &readonly {
            sections.push(format!("- **{}**: {}\n", t.name, t.description));
        }
        sections.push("\n### Structural Write (Modifies Graph)\n".to_string());
        for t in &structural {
            sections.push(format!("- **{}**: {}\n", t.name, t.description));
        }
        sections.push("\n### Mutating (Executes Commands / Side Effects)\n".to_string());
        for t in &mutating {
            sections.push(format!("- **{}**: {}\n", t.name, t.description));
        }

        sections.push("\n## Decision Flow\n\n".to_string());
        sections.push(
            "1. **Explore unknown area**: `get_layered_map` or `architectural_observations`\n"
                .to_string(),
        );
        sections.push(
            "2. **Find specific symbol**: `trace_dependency` or `semantic_search`\n".to_string(),
        );
        sections
            .push("3. **Assess change risk**: `get_blast_radius` before modifying\n".to_string());
        sections.push(
            "4. **Understand coupling**: `get_coupling_radar` for hidden co-change patterns\n"
                .to_string(),
        );
        sections.push(
            "5. **Find anchors**: `find_anchors` to identify stable architectural roots\n"
                .to_string(),
        );
        sections.push(
            "6. **Complex queries**: Use `query_graph` for multi-hop traversals\n".to_string(),
        );

        sections.push(
            "\n*Use tools incrementally (N+1 approach) to avoid context window overflow.*\n"
                .to_string(),
        );

        sections.push("\n---\n\n## Federation Mode (for org-wide questions)\n".to_string());
        sections.push(
            "When the user's question spans multiple repos (e.g. \"who else uses this function?\", \
             \"what depends on this service?\"), switch to federation mode by launching the server \
             with `lain server --config repos.yaml` instead of single-workspace mode (`lain mcp`, \
             run from inside the repo).\n"
                .to_string(),
        );
        sections.push("\n### Federation Tools\n".to_string());
        sections.push("- **list_repos**: list all indexed repos with health\n".to_string());
        sections.push("- **get_repo_info**: get a single repo's details\n".to_string());
        sections.push("- **get_federation_health**: get federation-wide stats (repo counts, node/edge totals, memory estimate)\n".to_string());
        sections.push("- **search_org**: search symbols across all repos\n".to_string());
        sections.push("- **get_cross_repo_blast_radius** / **get_cross_repo_blast_radius_for_repo**: cross-repo symbol blast radius\n".to_string());

        sections.push("\n### `repo_id` Resolution Rule\n".to_string());
        sections.push("1. If `repo_id` is explicit → use it.\n".to_string());
        sections.push(
            "2. If `symbol` is given and resolves to a unique repo → use that.\n".to_string(),
        );
        sections.push("3. If 1 repo is registered → use it.\n".to_string());
        sections.push(
            "4. Otherwise → `Config(\"multiple repos; specify repo_id or symbol\")`.\n".to_string(),
        );

        // Workspace mode: appends a section that explains the 4 new
        // workspace tools + the resolution rule when a workspace is
        // active. The current section is unconditional — operators who
        // haven't configured workspaces see the same text; the tools
        // are simply not registered for them.
        sections.push("\n---\n\n## Workspace Mode (scoped subset of repos)\n".to_string());
        sections.push(
            "When the server is started with `--workspace <name>`, only that workspace's \
             repos are loaded. Use these tools to learn the scope and reason about it:\n"
                .to_string(),
        );
        sections
            .push("- **list_workspaces**: list known workspaces + which is active\n".to_string());
        sections.push(
            "- **get_active_workspace**: which workspace the server holds right now\n".to_string(),
        );
        sections.push("- **get_workspace(name)**: full detail on one workspace\n".to_string());
        sections.push(
            "- **get_workspace_graph(filter?)**: node + edge data for the dashboard view\n"
                .to_string(),
        );
        sections.push(
            "\nThe 6 federation tools (`list_repos`, `search_org`, `get_cross_repo_blast_radius`, etc.) \
             operate over the active workspace's repo subset. `get_repo_info(<repo_id>)` returns `NotFound` \
             if the repo isn't in the active workspace — that's correct, not a bug.\n"
                .to_string(),
        );
        sections.push("\n### Detection\n".to_string());
        sections.push(
            "If `list_workspaces` appears in your tool list, you're in workspace mode. Call \
             `get_active_workspace` to learn which subset is loaded before issuing broad queries \
             like `search_org`.\n"
                .to_string(),
        );

        Ok(sections.join(""))
    }

    // `augment_knowledge` lived here: an on-demand LSP reference fetch
    // for a symbol that has no `Calls` edges yet. It had no caller and no
    // test, so it never ran once. It also duplicates the resolve phase
    // (`resolve_call_edges`), and wiring it into a query path would put
    // an unbounded language-server round trip inside a tool call.
    //
    // When a file genuinely has definitions and no call edges that is an
    // indexing gap, and `find_dead_code` already reports it as one rather
    // than silently guessing. Fixing that belongs in the resolve phase,
    // not in a lazy side-channel that no code path reaches.
}

#[doc(hidden)]
pub fn create_test_executor_with_graph(graph: crate::graph::GraphDatabase) -> ToolExecutor {
    use std::path::{Path, PathBuf};
    let overlay = crate::overlay::VolatileOverlay::new();
    let embedder = crate::nlp::NlpEmbedder::new_stub();
    let git = Arc::new(parking_lot::Mutex::new(
        crate::git::GitSensor::new(Path::new(".")).unwrap_or_else(|_| {
            crate::git::GitSensor::new(Path::new("/tmp")).expect("fallback git sensor")
        }),
    ));
    let lsp_pool = Arc::new(
        crate::lsp::LspPool::new(Path::new("."), 2, &crate::tuning::RuntimeConfig::default())
            .expect("lsp pool"),
    );
    let tuning = Arc::new(crate::tuning::TuningConfig::default());
    let cross_encoder = crate::nlp::CrossEncoder::from_dir(Path::new("/nonexistent"));
    ToolExecutor::new(ToolExecutorConfig {
        graph,
        overlay,
        embedder,
        cross_encoder,
        git,
        lsp_pool,
        tuning,
        workspace: PathBuf::from("."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_agent_strategy_mentions_federation_tools() {
        let strategy = build_test_strategy();
        // The strategy must mention each federation tool name (including the
        // `_for_repo` variant of `get_cross_repo_blast_radius`).
        for tool in [
            "list_repos",
            "get_repo_info",
            "get_federation_health",
            "search_org",
            "get_cross_repo_blast_radius",
            "get_cross_repo_blast_radius_for_repo",
        ] {
            assert!(
                strategy.contains(tool),
                "strategy must mention federation tool {}: \n{}",
                tool,
                strategy,
            );
        }
        // The strategy must explain the repo_id resolution rule.
        assert!(
            strategy.contains("repo_id") || strategy.contains("repo id"),
            "strategy must mention repo_id resolution",
        );
        // The strategy must explain single-workspace vs federation.
        assert!(
            strategy.contains("federation") || strategy.contains("Federation"),
            "strategy must mention federation mode",
        );
    }

    fn build_test_strategy() -> String {
        let temp_dir = tempfile::tempdir().unwrap();
        let graph = crate::graph::GraphDatabase::new(&temp_dir.path().join("graph.bin")).unwrap();
        let exec = create_test_executor_with_graph(graph);
        exec.get_agent_strategy()
            .expect("get_agent_strategy should succeed")
    }

    /// A read-only executor (sidecar, or the doctor MCP probe) never runs
    /// `await_startup_reindex` — nothing else would ever call
    /// `ReadinessHandle::ready()` on it. Before this fix, leaving the
    /// handle at its `warming_up` default meant the central gate added in
    /// `dispatch_tool_call` would deny every graph-dependent tool call to
    /// a sidecar forever, since a sidecar never indexes.
    #[test]
    fn read_only_executor_starts_ready_not_warming_up() {
        let graph = crate::graph::GraphDatabase::empty_read_only();
        let overlay = crate::overlay::VolatileOverlay::new();
        let executor = ToolExecutor::new_read_only(graph, overlay, std::path::PathBuf::from("."));
        let snapshot = executor.ctx.readiness.snapshot();
        assert_eq!(snapshot.state, crate::server::readiness::IndexState::Ready);
        assert!(
            crate::server::readiness::gate_tool_call("find_anchors", &snapshot, true).is_none()
        );
    }

    /// AGENT_UX_ROADMAP.md M4 step 8: in federation mode, `get_capabilities`
    /// must report both each repo's own state (`repositories`) and a
    /// worst-of aggregate under the existing `capabilities` key, not the
    /// one process-global handle a federation server never advances.
    #[tokio::test]
    async fn get_capabilities_reports_per_repo_state_and_worst_of_aggregate_in_federation_mode() {
        use crate::federation::federated_index::FederatedIndex;
        use crate::federation::graph_backend::PetgraphBackend;
        use crate::federation::repo_id::RepoId;
        use crate::federation::repo_source::WorkspaceDirSource;
        use crate::server::federation::health::RepoHealth;

        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));

        let mut src_dirs = Vec::new();
        for name in ["alpha", "beta"] {
            let src_dir = tempfile::tempdir().unwrap();
            git2::Repository::init(src_dir.path()).unwrap();
            let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
                WorkspaceDirSource::new(RepoId::new(name).unwrap(), src_dir.path().to_path_buf())
                    .unwrap(),
            );
            fed.add_repo(src, tmp.path()).await.unwrap();
            src_dirs.push(src_dir);
        }
        fed.get_repo(&RepoId::new("alpha").unwrap())
            .unwrap()
            .set_health(RepoHealth::Ready);
        fed.get_repo(&RepoId::new("beta").unwrap())
            .unwrap()
            .set_health(RepoHealth::Indexing);

        let graph = crate::graph::GraphDatabase::empty_read_only();
        let overlay = crate::overlay::VolatileOverlay::new();
        let mut executor =
            ToolExecutor::new_read_only(graph, overlay, std::path::PathBuf::from("."));
        executor.ctx.federation = Some(Arc::new(fed));

        let json_text = executor.get_capabilities().unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_text).unwrap();

        // Aggregate must reflect the worst repo (beta, still indexing).
        assert_eq!(value["capabilities"]["symbols"]["state"], "warming_up");

        let repos = value["repositories"].as_array().unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0]["id"], "alpha");
        assert_eq!(repos[0]["capabilities"]["symbols"]["state"], "ready");
        assert_eq!(repos[1]["id"], "beta");
        assert_eq!(repos[1]["capabilities"]["symbols"]["state"], "warming_up");

        drop(src_dirs);
    }
}
