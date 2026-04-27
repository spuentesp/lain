//! Modular tool execution system
//!
//! Follows SOLID and DRY principles by delegating logic to specialized handlers.

pub mod definitions;
pub mod utils;
pub mod handlers;
pub mod registry;
#[cfg(test)]
pub mod proptest_helpers;
#[cfg(test)]
pub mod utils_tests;

use crate::error::LainError;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::nlp::NlpEmbedder;
use crate::overlay::VolatileOverlay;
use crate::tools::registry::{ToolContext, ToolRegistry};
use crate::tuning::TuningConfig;
use crate::tools::utils::get_str_arg;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, error};
use uuid::Uuid;
use tokio::task;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use reqwest::Client;

pub use definitions::ToolDefinition;
use utils::*;

#[derive(Clone, Serialize, Deserialize)]
pub enum JobState {
    Running,
    Completed { success: bool, output: Option<String>, error: Option<String> },
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
    jobs: Arc<AsyncMutex<HashMap<String, JobInfo>>>,
    job_webhooks: Arc<AsyncMutex<Vec<String>>>,
    pub tuning: Arc<TuningConfig>,
}

impl ToolExecutor {
    pub fn graph(&self) -> &GraphDatabase { &self.ctx.graph }
    pub fn overlay(&self) -> &VolatileOverlay { &self.ctx.overlay }
    pub fn embedder(&self) -> &NlpEmbedder { &self.ctx.embedder }
    pub fn ui_sessions(&self) -> &AsyncMutex<HashMap<String, UiSession>> { &self.ctx.ui_sessions }
}

pub const DIAGNOSTICS_PORT: u16 = 9999;

impl ToolExecutor {
    pub fn new(
        graph: GraphDatabase,
        overlay: VolatileOverlay,
        embedder: NlpEmbedder,
        git: Arc<Mutex<GitSensor>>,
        lsp_pool: Arc<LspPool>,
        tuning: Arc<TuningConfig>,
    ) -> Self {
        let jobs_registry = Arc::new(AsyncMutex::new(HashMap::<String, JobInfo>::new()));
        let webhooks = Arc::new(AsyncMutex::new(Vec::new()));

        let ctx = ToolContext::new(
            graph,
            overlay,
            embedder,
            git,
            lsp_pool,
            Arc::clone(&tuning),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AsyncMutex::new(HashMap::new())),
            Arc::clone(&jobs_registry),
            Arc::clone(&webhooks),
        );

        // Snapshot persistence (optional, for resumeability)
        let jobs_path = std::env::var("LAIN_JOB_STORE").unwrap_or_else(|_| ".lain/jobs.json".into());
        if let Ok(contents) = std::fs::read_to_string(&jobs_path) {
            let jobs_registry = Arc::clone(&jobs_registry);
            task::spawn(async move {
                if let Ok(vec) = serde_json::from_str::<Vec<JobInfo>>(&contents) {
                    let mut guard = jobs_registry.lock().await;
                    for j in vec {
                        guard.insert(j.id.clone(), j);
                    }
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

    async fn persist_jobs_snapshot(jobs: Arc<AsyncMutex<HashMap<String, JobInfo>>>) -> Result<(), ()> {
        let path = std::env::var("LAIN_JOB_STORE").unwrap_or_else(|_| ".lain/jobs.json".into());
        let guard = jobs.lock().await;
        let vec: Vec<JobInfo> = guard.values().cloned().collect();
        if let Ok(json) = serde_json::to_string(&vec) {
            let _ = std::fs::write(&path, json);
        }
        Ok(())
    }

    /// Primary dispatcher for all MCP tools
    pub async fn call(&self, name: &str, arguments: Option<&Map<String, Value>>) -> Result<String, LainError> {
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
                        let guard = self.jobs.lock().await;
                        let running = guard.values().filter(|j| matches!(j.state, JobState::Running)).count();
                        if running >= MAX_CONCURRENT_JOBS {
                            return Err(LainError::Mcp(format!("Too many concurrent jobs (max {})", MAX_CONCURRENT_JOBS)));
                        }
                    }

                    let job_id = Uuid::new_v4().to_string();
                    let job = JobInfo { id: job_id.clone(), created_at: std::time::SystemTime::now(), state: JobState::Running };

                    {
                        let mut guard = self.jobs.lock().await;
                        guard.insert(job_id.clone(), job.clone());
                    }

                    let jobs_registry = Arc::clone(&self.jobs);
                    let webhooks = Arc::clone(&self.job_webhooks);
                    let job_id_clone = job_id.clone();
                    task::spawn(async move {
                        let res = exec.call_inner(&name_owned, Some(&owned)).await;
                        {
                            let mut guard = jobs_registry.lock().await;
                            if let Some(j) = guard.get_mut(&job_id_clone) {
                                match &res {
                                    Ok(out) => j.state = JobState::Completed { success: true, output: Some(out.clone()), error: None },
                                    Err(e) => j.state = JobState::Completed { success: false, output: None, error: Some(e.to_string()) },
                                }
                            }
                        } // guard dropped here — must release before webhook/persist

                        let hooks = { let h = webhooks.lock().await; h.clone() };
                        if !hooks.is_empty() {
                            let client = Client::new();
                            let payload = match &res {
                                Ok(out) => json!({ "job_id": job_id_clone, "state": "completed", "output": out }),
                                Err(e) => json!({ "job_id": job_id_clone, "state": "failed", "error": e.to_string() }),
                            };
                            for url in hooks {
                                let _ = client.post(&url).json(&payload).send().await;
                            }
                        }
                        let _ = Self::persist_jobs_snapshot(Arc::clone(&jobs_registry)).await;
                    });

                    return Ok(format!("{{\"job_id\":\"{}\"}}", job_id));
                }
            }
        }

        return self.call_inner(name, arguments).await;
    }

    async fn call_inner(&self, name: &str, arguments: Option<&Map<String, Value>>) -> Result<String, LainError> {
        let args = arguments.cloned().unwrap_or_default();

        // Special executor methods — not registered as ToolHandlers
        match name {
            "get_health" => return self.get_health().await,
            "get_agent_strategy" => return self.get_agent_strategy(),
            "install_language_server" => {
                let lang = get_str_arg(arguments, "language");
                return self.install_language_server(lang).await;
            }
            "register_job_webhook" => {
                let url = get_str_arg(arguments, "url");
                let mut hooks = self.job_webhooks.lock().await;
                if !hooks.contains(&url.to_string()) {
                    hooks.push(url.to_string());
                }
                return Ok(format!("Webhook registered: {}", url));
            }
            "get_job_status" => {
                let job_id = get_str_arg(arguments, "job_id");
                let guard = self.jobs.lock().await;
                match guard.get(job_id) {
                    Some(job) => return Ok(serde_json::to_string(job).unwrap_or_default()),
                    None => return Err(LainError::NotFound(format!("Job not found: {}", job_id))),
                }
            }
            "debug_sleep" => {
                let secs = args.get("secs").and_then(|v| v.as_u64()).unwrap_or(1) as u64;
                tokio::time::sleep(tokio::time::Duration::from_secs(secs as u64)).await;
                return Ok(format!("Slept for {} second(s)", secs));
            }
            _ => {}
        }

        // Delegate to inventory-based registry
        ToolRegistry::dispatch(&self.ctx, name, &args).await
    }

    pub async fn get_health(&self) -> Result<String, LainError> {
        let (nodes, edges) = self.ctx.graph.get_stats();
        let last_commit = self.ctx.graph.get_last_commit()?.unwrap_or_else(|| "None".to_string());
        let overlay_stats = self.ctx.overlay.stats();

        let embedder_status = if self.ctx.embedder.is_stub() {
            "Not loaded (semantic search unavailable)".to_string()
        } else {
            format!("Loaded ({}d embeddings)", self.ctx.embedder.embedding_dim())
        };

        let mut output = format!(
            "## Lain Server Health\n\n- **Status:** Operational ✅\n- **Static Nodes:** {}\n- **Static Edges:** {}\n- **Volatile Nodes (Overlay):** {}\n- **Last Enriched Commit:** {}\n- **NLP Model:** {}\n",
            nodes, edges, overlay_stats.node_count, last_commit, embedder_status
        );

        output.push_str("\n### Language Support\n");
        let langs = {
            let lsp = self.ctx.lsp_pool.next();
            let lsp_guard = lsp.lock().await;
            lsp_guard.get_supported_languages()
        };
        
        let mut seen_binaries = std::collections::HashSet::new();
        for (_, binary, available) in langs {
            if seen_binaries.contains(&binary) { continue; }
            seen_binaries.insert(binary.clone());
            
            let status = if available { "✅" } else { "❌ (Missing)" };
            output.push_str(&format!("- **{}**: {}\n", binary, status));
        }

        Ok(output)
    }

    async fn install_language_server(&self, language: &str) -> Result<String, LainError> {
        info!("Requesting installation of LSP for: {}", language);

        let lsp = Arc::clone(&self.ctx.lsp_pool.next());
        let lang = language.to_string();

        tokio::spawn(async move {
            let mut lsp_guard = lsp.lock().await;
            if let Err(e) = lsp_guard.install_server(&lang).await {
                error!("LSP installation for {} failed: {}", lang, e);
            }
        });

        Ok(format!("Installation of LSP for '{}' started in background. Check 'get_health' in a minute to see if it's available.", language))
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
            "get_health", "get_agent_strategy", "install_language_server", "query_graph"
        ];
        let readonly_set = [
            "explore_architecture", "list_entry_points", "compare_modules", "architectural_observations",
            "trace_dependency", "get_call_chain", "navigate_to_anchor", "get_layered_map", "get_master_map",
            "semantic_search", "find_anchors", "get_anchor_score", "get_context_depth",
            "find_dead_code", "explain_symbol", "suggest_refactor_targets",
            "get_context_for_prompt", "get_code_snippet", "get_call_sites",
            "find_untested_functions", "get_test_template", "find_test_file", "get_coverage_summary",
            "get_cross_runtime_callers", "describe_schema"
        ];
        let structural_set = [
            "add_comment", "tag_node", "update_node_metadata", "insert_reference_edge"
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
        sections.push("1. **Explore unknown area**: `get_layered_map` or `architectural_observations`\n".to_string());
        sections.push("2. **Find specific symbol**: `trace_dependency` or `semantic_search`\n".to_string());
        sections.push("3. **Assess change risk**: `get_blast_radius` before modifying\n".to_string());
        sections.push("4. **Understand coupling**: `get_coupling_radar` for hidden co-change patterns\n".to_string());
        sections.push("5. **Find anchors**: `find_anchors` to identify stable architectural roots\n".to_string());
        sections.push("6. **Complex queries**: Use `query_graph` for multi-hop traversals\n".to_string());

        sections.push("\n*Use tools incrementally (N+1 approach) to avoid context window overflow.*\n".to_string());
        Ok(sections.join(""))
    }

    /// On-demand ingestion of references for a specific symbol (Augmentation)
    pub async fn augment_knowledge(&self, symbol_name: &str) -> Result<(), LainError> {
        let node = if let Some(n) = self.ctx.overlay.get_node(symbol_name) {
            Some(n)
        } else {
            self.ctx.graph.get_node(symbol_name)?
        };

        let Some(target_node) = node else { return Ok(()); };

        if let Ok(edges) = self.ctx.graph.get_edges_from(&target_node.id) {
            if edges.iter().any(|e| e.edge_type == crate::schema::EdgeType::Calls) {
                return Ok(());
            }
        }

        info!("Augmenting knowledge for '{}' on-demand", symbol_name);

        let refs = {
            let lsp = self.ctx.lsp_pool.next();
            let mut lsp = lsp.lock().await;
            lsp.get_references(
                std::path::Path::new(&target_node.path),
                target_node.line_start.unwrap_or(0),
                0
            ).await.unwrap_or_default()
        };

        for r in refs {
            let path_str = r.path.to_string_lossy().to_string();
            if let Some(source_node) = resolve_node_at_location(&self.ctx.graph, &self.ctx.overlay, &path_str, r.line) {
                if source_node.id != target_node.id {
                    let edge = crate::schema::GraphEdge::new(
                        crate::schema::EdgeType::Calls,
                        source_node.id.clone(),
                        target_node.id.clone()
                    );
                    self.ctx.graph.insert_edge(&edge)?;
                }
            } else {
                let mut ghost_node = crate::schema::GraphNode::new(
                    crate::schema::NodeType::Function,
                    format!("unknown:{}", r.line),
                    path_str.clone()
                );
                ghost_node.is_hydrated = false;
                self.ctx.graph.upsert_node(ghost_node.clone())?;

                let edge = crate::schema::GraphEdge::new(
                    crate::schema::EdgeType::Calls,
                    ghost_node.id.clone(),
                    target_node.id.clone()
                );
                self.ctx.graph.insert_edge(&edge)?;
            }
        }

        Ok(())
    }
}

#[doc(hidden)]
pub fn create_test_executor_with_graph(graph: crate::graph::GraphDatabase) -> ToolExecutor {
    use std::path::Path;
    let overlay = crate::overlay::VolatileOverlay::new();
    let embedder = crate::nlp::NlpEmbedder::new_stub();
    let git = Arc::new(parking_lot::Mutex::new(
        crate::git::GitSensor::new(Path::new(".")).unwrap_or_else(|_| {
            crate::git::GitSensor::new(Path::new("/tmp")).expect("fallback git sensor")
        }),
    ));
    let lsp_pool = Arc::new(
        crate::lsp::LspPool::new(Path::new("."), 2).expect("lsp pool"),
    );
    let tuning = Arc::new(crate::tuning::TuningConfig::default());
    ToolExecutor::new(graph, overlay, embedder, git, lsp_pool, tuning)
}
