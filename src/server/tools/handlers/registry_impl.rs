//! `ToolHandler` implementations for all tools — auto-discovered via `inventory`.
//!
//! Each `impl ToolHandler` block calls `inventory::submit!(ToolHandlerEntry(&handler))`
//! at the end, registering the tool with the global registry.
//!
//! Adding a new tool: implement the handler here. No central edit needed.

use crate::error::LainError;
use crate::server::tools::handlers;
use crate::server::tools::registry::{ToolCapability, ToolContext, ToolHandler, ToolHandlerEntry};
use crate::server::tools::utils::{bool_arg, required_str_arg, str_arg, u32_arg, usize_arg};
use async_trait::async_trait;
use inventory;
use serde_json::{Map, Value};

/// Build the `(sessions, port, ttl)` triple the UI-session handlers need
/// to emit an interactive `/ui/...` link. Returns `None` when no HTTP
/// transport is serving (stdio mode, `diagnostics_port == 0`) —
/// emitting a link there produces a dead URL.
///
/// The TTL rides along because the handlers have no other route to the
/// tuning config; it was a literal `600` in two places while
/// `ingestion.ui_session_ttl_secs` carried the same default and no reader.
fn ui_link(ctx: &ToolContext) -> crate::server::tools::UiLink<'_> {
    let port = ctx
        .diagnostics_port
        .load(std::sync::atomic::Ordering::Relaxed);
    if port == 0 {
        None
    } else {
        Some((
            &ctx.ui_sessions,
            port,
            std::time::Duration::from_secs(ctx.tuning.ingestion.ui_session_ttl_secs),
        ))
    }
}

// ─── Handler macros ────────────────────────────────────────────────────────────

/// The four static-metadata methods of a `ToolHandler`, so an impl
/// block holds only `call`. `$cap` is a `ToolCapability` variant, left
/// explicit on every tool: a forgotten capability must not default to
/// read-only.
macro_rules! tool_meta {
    ($name:expr, $description:expr, $schema:expr, $cap:ident) => {
        fn name(&self) -> &'static str {
            $name
        }
        fn description(&self) -> &'static str {
            $description
        }
        fn input_schema(&self) -> &'static str {
            $schema
        }
        fn capability(&self) -> ToolCapability {
            ToolCapability::$cap
        }
    };
}

// Arg-extraction helpers (`str_arg`, `required_str_arg`, `usize_arg`,
// `bool_arg`, `u32_arg`, `str_arg`) are imported from
// `crate::server::tools::utils` so handler modules and integration
// tests share one canonical set.

// ─── Architecture handlers ─────────────────────────────────────────────────────

// ─── Architecture Domain ───────────────────────────────────────────────────────

pub struct ExploreArchitectureHandler;
#[async_trait]
impl ToolHandler for ExploreArchitectureHandler {
    tool_meta!(
        "explore_architecture",
        "Returns a high-level tree of files and modules up to a specific depth",
        r#"{"type":"object","properties":{"max_depth":{"type":"integer"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let max_depth = usize_arg(args, "max_depth").unwrap_or(2);
        handlers::architecture::explore_architecture(&ctx.graph, &ctx.overlay, max_depth)
    }
}
// ─── Explain Dispatch (Tier 3 — dynamic-dispatch mitigation) ──────────────

pub struct ExplainDispatchHandler;
#[async_trait]
impl ToolHandler for ExplainDispatchHandler {
    tool_meta!(
        "explain_dispatch",
        "Synthesises static callers, heuristic edges, runtime edges, and co-change \
         partners for a symbol and returns a single verdict. Use this instead of \
         `get_blast_radius` when an empty blast radius might mean 'static graph \
         cannot see the dispatcher' rather than 'no callers'. The verdict field \
         is `insufficient_evidence` exactly when every signal is empty — that \
         is the case Tier 1 teaches agents to refuse to treat as safe.",
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::explain_dispatch::explain_dispatch(&ctx.graph, &ctx.overlay, &symbol).await
    }
}
inventory::submit!(ToolHandlerEntry(&ExplainDispatchHandler));

inventory::submit!(ToolHandlerEntry(&ExploreArchitectureHandler));

pub struct ListEntryPointsHandler;
#[async_trait]
impl ToolHandler for ListEntryPointsHandler {
    tool_meta!(
        "list_entry_points",
        "Use this when asking 'where does execution start?': main/App-style entry \
         points and top-level routes.",
        r#"{"type":"object","properties":{},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        _args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::architecture::list_entry_points(&ctx.graph, &ctx.overlay)
    }
}
inventory::submit!(ToolHandlerEntry(&ListEntryPointsHandler));

pub struct CompareModulesHandler;
#[async_trait]
impl ToolHandler for CompareModulesHandler {
    tool_meta!(
        "compare_modules",
        "Compares stability and coupling metrics between two modules",
        r#"{"type":"object","properties":{"module_a":{"type":"string"},"module_b":{"type":"string"}},"required":["module_a","module_b"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let module_a = required_str_arg(args, "module_a")?;
        let module_b = required_str_arg(args, "module_b")?;
        handlers::architecture::compare_modules(&ctx.graph, &ctx.overlay, &module_a, &module_b)
    }
}
inventory::submit!(ToolHandlerEntry(&CompareModulesHandler));

pub struct ArchitecturalObservationsHandler;
#[async_trait]
impl ToolHandler for ArchitecturalObservationsHandler {
    tool_meta!("architectural_observations", "Analyzes the codebase for architectural patterns, cross-boundary couplings, and high-fan-out modules", r#"{"type":"object","properties":{"min_fan_out":{"type":"integer"},"min_pattern_files":{"type":"integer"}},"required":[]}"#, ReadOnly);
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let min_fan_out = usize_arg(args, "min_fan_out").unwrap_or(50);
        let min_pattern_files = usize_arg(args, "min_pattern_files").unwrap_or(3);
        handlers::architecture::architectural_observations(
            &ctx.graph,
            min_fan_out,
            min_pattern_files,
        )
    }
}
inventory::submit!(ToolHandlerEntry(&ArchitecturalObservationsHandler));

/// AGENT_UX_ROADMAP.md Milestone 5: one-call bootstrap context
/// for a fresh agent. Returns a stable JSON payload with
/// repository / architecture / capabilities / recommended_actions
/// sections; the M6 semantic tools (find_symbol, assess_change,
/// find_related, search_code) are listed with `available: false`
/// until M6 lands so an agent doesn't call a missing tool.
pub struct UnderstandRepositoryHandler;
#[async_trait]
impl ToolHandler for UnderstandRepositoryHandler {
    tool_meta!(
        "understand_repository",
        "One-call bootstrap context: repository identity, top anchors, entry points, \
         capability states, and the intent->tool mapping the agent should reach for \
         first. AGENT_UX_ROADMAP.md Milestone 5. Useful when an agent just connected and \
         hasn't yet explored the codebase. Symbol-level detail is `get_context`.",
        r#"{"type":"object","properties":{"budget_tokens":{"type":"integer","description":"Soft token budget for the payload; default 3000."}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let budget_tokens = usize_arg(args, "budget_tokens");
        handlers::architecture::understand_repository(
            &ctx.workspace,
            &ctx.graph,
            &ctx.overlay,
            &ctx.git,
            &ctx.readiness,
            budget_tokens,
            !ctx.embedder.is_stub(),
        )
    }
}
inventory::submit!(ToolHandlerEntry(&UnderstandRepositoryHandler));

// ─── Navigation Domain ─────────────────────────────────────────────────────────

pub struct TraceDependencyHandler;
#[async_trait]
impl ToolHandler for TraceDependencyHandler {
    tool_meta!(
        "trace_dependency",
        "Recursively finds everything a symbol depends on",
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::navigation::trace_dependency(&ctx.graph, &ctx.overlay, &symbol)
    }
}
inventory::submit!(ToolHandlerEntry(&TraceDependencyHandler));

pub struct GetCallChainHandler;
#[async_trait]
impl ToolHandler for GetCallChainHandler {
    tool_meta!(
        "get_call_chain",
        "Use this when you need the exact call path between two symbols (`from` to \
         `to`). Traces within one repository — pass `repo_id` when a federation \
         holds both ends. For what-breaks impact use `get_blast_radius`.",
        r#"{"type":"object","properties":{"from":{"type":"string","description":"symbol name the path starts at (the caller)"},"to":{"type":"string","description":"symbol name the path ends at (the callee)"}},"required":["from","to"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let from = required_str_arg(args, "from")?;
        let to = required_str_arg(args, "to")?;
        handlers::navigation::get_call_chain(
            &ctx.graph,
            &ctx.overlay,
            ctx.federation.as_deref(),
            &from,
            &to,
            ui_link(ctx),
        )
        .await
    }
}
inventory::submit!(ToolHandlerEntry(&GetCallChainHandler));

pub struct NavigateToAnchorHandler;
#[async_trait]
impl ToolHandler for NavigateToAnchorHandler {
    tool_meta!(
        "navigate_to_anchor",
        "Finds the most foundational 'Anchor' node that controls a given leaf function",
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::navigation::navigate_to_anchor(&ctx.graph, &ctx.overlay, &symbol)
    }
}
inventory::submit!(ToolHandlerEntry(&NavigateToAnchorHandler));

pub struct GetLayeredMapHandler;
#[async_trait]
impl ToolHandler for GetLayeredMapHandler {
    tool_meta!(
        "get_layered_map",
        "Returns a 'slice' of the architecture at a specific depth from the entry point",
        r#"{"type":"object","properties":{"layer":{"type":"integer"},"granularity":{"type":"string"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let layer = usize_arg(args, "layer").unwrap_or(0);
        let granularity = str_arg(args, "granularity");
        handlers::navigation::get_layered_map(&ctx.graph, &ctx.overlay, layer, &granularity)
    }
}
inventory::submit!(ToolHandlerEntry(&GetLayeredMapHandler));

pub struct GetMasterMapHandler;
#[async_trait]
impl ToolHandler for GetMasterMapHandler {
    tool_meta!("get_master_map", "Get a high-level Staleness Report showing when each module was last synced from LSP and Git", r#"{"type":"object","properties":{},"required":[]}"#, ReadOnly);
    async fn call(
        &self,
        ctx: &ToolContext,
        _args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::architecture::get_master_map(&ctx.graph, &ctx.overlay)
    }
}
inventory::submit!(ToolHandlerEntry(&GetMasterMapHandler));

// ─── Search Domain ────────────────────────────────────────────────────────────

pub struct SemanticSearchHandler;
#[async_trait]
impl ToolHandler for SemanticSearchHandler {
    tool_meta!(
        "semantic_search",
        "Find code by intent/concept using local NLP vectors (e.g., 'Where is auth handled?')",
        r#"{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        if ctx.embedder.is_stub() {
            // Never name a command that does not exist: `lain
            // install-embeddings` returned `error: unrecognized
            // subcommand`, so an agent that followed the instruction
            // got an error and then had to decide whether to trust the
            // next thing lain told it. These three paths are real.
            return Err(LainError::Unavailable(
                "Semantic search unavailable: NLP model not loaded. Get one with \
                 `install.sh --download-model`, point the LAIN_EMBEDDING_MODEL env var \
                 at a model directory containing model.onnx + tokenizer.json, or place \
                 one in `.lain/models/`."
                    .to_string(),
            ));
        }
        let query = required_str_arg(args, "query")?;
        let limit = usize_arg(args, "limit").unwrap_or(10);
        handlers::search::semantic_search(
            &ctx.workspace,
            &ctx.graph,
            &ctx.overlay,
            &ctx.embedder,
            &ctx.cross_encoder,
            &ctx.embedding_cache,
            &ctx.tuning,
            &query,
            limit,
        )
    }
}
inventory::submit!(ToolHandlerEntry(&SemanticSearchHandler));

// ─── Impact Domain ───────────────────────────────────────────────────────────

pub struct GetBlastRadiusHandler;
#[async_trait]
impl ToolHandler for GetBlastRadiusHandler {
    tool_meta!(
        "get_blast_radius",
        "Use this when you want to know what breaks if you change a symbol: direct \
         and transitive dependents. For an exact A-to-B call path use \
         `get_call_chain`; for a pre-edit risk verdict use `assess_change`; for \
         call-site dispatch honesty use `explain_dispatch`.",
        r#"{"type":"object","properties":{"symbol":{"type":"string"},"include_coupling":{"type":"boolean"},"include_weak_edges":{"type":"boolean","description":"Include heuristic callers (dynamic dispatch / bus / router) with confidence >= LAIN_HEURISTIC_MIN_CONFIDENCE. Default false."}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        let include_coupling = bool_arg(args, "include_coupling").unwrap_or(false);
        let include_weak_edges = bool_arg(args, "include_weak_edges").unwrap_or(false);
        let mut out = handlers::impact::get_blast_radius(
            &ctx.graph,
            &ctx.overlay,
            &ctx.workspace,
            &symbol,
            include_coupling,
            include_weak_edges,
            ui_link(ctx),
        )
        .await?;
        out.push_str(&open_annotations_for_symbol(ctx, &symbol));
        Ok(out)
    }
}
inventory::submit!(ToolHandlerEntry(&GetBlastRadiusHandler));

pub struct GetCouplingRadarHandler;
#[async_trait]
impl ToolHandler for GetCouplingRadarHandler {
    tool_meta!(
        "get_coupling_radar",
        "Use this when asking 'what changes together?': hidden coupling between files \
         from historical git co-change. `find_related` adds graph and semantic \
         neighbours to the same question.",
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::impact::get_coupling_radar(&ctx.graph, &ctx.overlay, &symbol, ui_link(ctx)).await
    }
}
inventory::submit!(ToolHandlerEntry(&GetCouplingRadarHandler));

// ─── Metrics Domain ───────────────────────────────────────────────────────────

pub struct FindAnchorsHandler;
#[async_trait]
impl ToolHandler for FindAnchorsHandler {
    tool_meta!(
        "find_anchors",
        "Use this when asking 'what should I read first?': the most foundational, \
         stable components by corpus-wide anchor score.",
        r#"{"type":"object","properties":{"limit":{"type":"integer"},"include_tests":{"type":"boolean","description":"By default, anchors in `tests/` and `scripts/` are filtered out — those paths are heavily called by tests/scripts and inflate the score above real architectural pillars. Pass `true` to include them."}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let limit = usize_arg(args, "limit").unwrap_or(10);
        // B8 (2026-10-04): default to filtering test/script paths
        // so the top anchors are real architectural pillars, not
        // test fixtures whose score is inflated by being called
        // from many other tests. Opt in with `include_tests=true`.
        let include_tests = args
            .get("include_tests")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        handlers::metrics::find_anchors(&ctx.graph, &ctx.overlay, limit, include_tests)
    }
}
inventory::submit!(ToolHandlerEntry(&FindAnchorsHandler));

pub struct GetAnchorScoreHandler;
#[async_trait]
impl ToolHandler for GetAnchorScoreHandler {
    tool_meta!(
        "get_anchor_score",
        "Returns the architectural stability score for a specific symbol",
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::metrics::get_anchor_score(&ctx.graph, &ctx.overlay, &symbol)
    }
}
inventory::submit!(ToolHandlerEntry(&GetAnchorScoreHandler));

pub struct GetContextDepthHandler;
#[async_trait]
impl ToolHandler for GetContextDepthHandler {
    tool_meta!(
        "get_context_depth",
        "Calculates layers of abstraction from the entry point for a symbol",
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::metrics::get_context_depth(&ctx.graph, &ctx.overlay, &symbol)
    }
}
inventory::submit!(ToolHandlerEntry(&GetContextDepthHandler));

pub struct FindDeadCodeHandler;
#[async_trait]
impl ToolHandler for FindDeadCodeHandler {
    tool_meta!(
        "find_dead_code",
        "Use this when asking 'what is unused?': nodes with zero incoming callers, \
         test code excluded.",
        r#"{"type":"object","properties":{"like":{"type":"string","description":"Filter dead code semantically (e.g., \"auth handler\")"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let like = args.get("like").and_then(|v| v.as_str());
        handlers::metrics::find_dead_code(
            &ctx.workspace,
            &ctx.graph,
            &ctx.overlay,
            like,
            &ctx.embedder,
            &ctx.embedding_cache,
        )
    }
}
inventory::submit!(ToolHandlerEntry(&FindDeadCodeHandler));

pub struct ExplainSymbolHandler;
#[async_trait]
impl ToolHandler for ExplainSymbolHandler {
    tool_meta!(
        "explain_symbol",
        "Combines signatures, docstrings, and metrics into a human-readable architectural summary",
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        let mut out = handlers::metrics::explain_symbol(
            &ctx.workspace,
            &ctx.graph,
            &ctx.overlay,
            &ctx.occupancy,
            &symbol,
        )?;
        out.push_str(&open_annotations_for_symbol(ctx, &symbol));
        Ok(out)
    }
}
inventory::submit!(ToolHandlerEntry(&ExplainSymbolHandler));

pub struct SuggestRefactorTargetsHandler;
#[async_trait]
impl ToolHandler for SuggestRefactorTargetsHandler {
    tool_meta!(
        "suggest_refactor_targets",
        "Identifies 'God Objects' and high-debt refactor targets based on complexity and stability",
        r#"{"type":"object","properties":{"limit":{"type":"integer"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let limit = usize_arg(args, "limit").unwrap_or(5);
        handlers::metrics::suggest_refactor_targets(&ctx.graph, &ctx.overlay, limit)
    }
}
inventory::submit!(ToolHandlerEntry(&SuggestRefactorTargetsHandler));

// ─── System Domain ────────────────────────────────────────────────────────────

pub struct QueryGraphHandler;
#[async_trait]
impl ToolHandler for QueryGraphHandler {
    tool_meta!(
        "query_graph",
        "Execute a query against the graph using a JSON ops array",
        r#"{"type":"object","properties":{"query":{"type":"object"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::query::query_graph(
            &ctx.workspace,
            &ctx.graph,
            &ctx.embedder,
            &ctx.embedding_cache,
            &ctx.presence,
            &ctx.occupancy,
            Some(args),
            ctx.tuning.ingestion.default_query_limit,
        )
    }
}
inventory::submit!(ToolHandlerEntry(&QueryGraphHandler));

pub struct DescribeSchemaHandler;
#[async_trait]
impl ToolHandler for DescribeSchemaHandler {
    tool_meta!("describe_schema", "Returns the graph schema (node types, edge types, example queries) for LLM session initialization", r#"{"type":"object","properties":{},"required":[]}"#, ReadOnly);
    async fn call(
        &self,
        _ctx: &ToolContext,
        _args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::query::describe_schema()
    }
}
inventory::submit!(ToolHandlerEntry(&DescribeSchemaHandler));

pub struct GetCrossRuntimeCallersHandler;
#[async_trait]
impl ToolHandler for GetCrossRuntimeCallersHandler {
    tool_meta!("get_cross_runtime_callers", "Find all protocol-level callers for a symbol (HTTP routes, gRPC services, GraphQL resolvers)", r#"{"type":"object","properties":{"node_id":{"type":"string"}},"required":["node_id"]}"#, ReadOnly);
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let node_id = required_str_arg(args, "node_id")?;
        handlers::cross_runtime::get_cross_runtime_callers(&ctx.graph, &ctx.overlay, &node_id)
    }
}
inventory::submit!(ToolHandlerEntry(&GetCrossRuntimeCallersHandler));

pub struct RunEnrichmentHandler;
#[async_trait]
impl ToolHandler for RunEnrichmentHandler {
    tool_meta!(
        "run_enrichment",
        "Triggers a full architectural scan and enrichment pass",
        r#"{"type":"object","properties":{},"required":[]}"#,
        StructuralWrite
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        _args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::enrichment::run_enrichment(&ctx.graph, &ctx.git, &ctx.tuning.ingestion)
    }
}
inventory::submit!(ToolHandlerEntry(&RunEnrichmentHandler));

pub struct SyncStateHandler;
#[async_trait]
impl ToolHandler for SyncStateHandler {
    tool_meta!(
        "sync_state",
        "Forces a re-sync of the graph with the current Git HEAD state",
        r#"{"type":"object","properties":{"repo_id":{"type":"string","description":"Repository to re-sync. Required when the server hosts more than one repository — the server rejects an unscoped call with a `requires scoping` config error naming the registered repo ids. Optional when the server hosts a single repository."}},"required":[]}"#,
        StructuralWrite
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        _args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::enrichment::sync_state(
            &ctx.graph,
            &ctx.git,
            &ctx.tuning.ingestion,
            &ctx.jobs,
            &ctx.last_outcome,
            ctx.federation.as_ref(),
        )
    }
}
inventory::submit!(ToolHandlerEntry(&SyncStateHandler));

// ─── Execution Domain ───────────────────────────────────────────────────────────

pub struct RunBuildHandler;
#[async_trait]
impl ToolHandler for RunBuildHandler {
    tool_meta!(
        "run_build",
        "Runs cargo build (optionally release) and returns build output and status",
        r#"{"type":"object","properties":{"cwd":{"type":"string"},"release":{"type":"boolean"}},"required":[]}"#,
        Mutating
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let cwd = run_cwd(ctx, args)?;
        let release = bool_arg(args, "release").unwrap_or(false);
        handlers::execution::run_build(
            &ctx.graph,
            &ctx.overlay,
            Some(&cwd),
            release,
            &ctx.tuning.runtime,
        )
        .await
    }
}
inventory::submit!(ToolHandlerEntry(&RunBuildHandler));

pub struct RunTestsHandler;
#[async_trait]
impl ToolHandler for RunTestsHandler {
    tool_meta!(
        "run_tests",
        "Runs cargo test with optional filter and returns test results",
        r#"{"type":"object","properties":{"cwd":{"type":"string"},"filter":{"type":"string"},"timeout_secs":{"type":"integer"}},"required":[]}"#,
        Mutating
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let cwd = run_cwd(ctx, args)?;
        let filter = if str_arg(args, "filter").is_empty() {
            None
        } else {
            Some(str_arg(args, "filter"))
        };
        let timeout_secs = usize_arg(args, "timeout_secs");
        handlers::execution::run_tests(
            &ctx.graph,
            &ctx.overlay,
            Some(&cwd),
            filter.as_deref(),
            timeout_secs,
            &ctx.tuning.runtime,
        )
        .await
    }
}
inventory::submit!(ToolHandlerEntry(&RunTestsHandler));

pub struct RunClippyHandler;
#[async_trait]
impl ToolHandler for RunClippyHandler {
    tool_meta!("run_clippy", "Runs cargo clippy with optional auto-fix and returns lint results with architectural context on failure", r#"{"type":"object","properties":{"cwd":{"type":"string"},"fix":{"type":"boolean"}},"required":[]}"#, Mutating);
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let cwd = run_cwd(ctx, args)?;
        let fix = bool_arg(args, "fix").unwrap_or(false);
        handlers::execution::run_clippy(
            &ctx.graph,
            &ctx.overlay,
            Some(&cwd),
            fix,
            &ctx.tuning.runtime,
        )
        .await
    }
}
inventory::submit!(ToolHandlerEntry(&RunClippyHandler));

// ─── Context Domain ───────────────────────────────────────────────────────────

pub struct GetContextForPromptHandler;
#[async_trait]
impl ToolHandler for GetContextForPromptHandler {
    tool_meta!(
        "get_context_for_prompt",
        "Builds LLM-optimized context for a symbol with signature, docstring, and relationships",
        r#"{"type":"object","properties":{"symbol":{"type":"string"},"max_tokens":{"type":"integer"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        let max_tokens = usize_arg(args, "max_tokens");
        handlers::context::get_context_for_prompt(&ctx.graph, &ctx.overlay, &symbol, max_tokens)
    }
}
inventory::submit!(ToolHandlerEntry(&GetContextForPromptHandler));

pub struct GetCodeSnippetHandler;
#[async_trait]
impl ToolHandler for GetCodeSnippetHandler {
    tool_meta!(
        "get_code_snippet",
        "Reads a file with surrounding context around a specific line",
        r#"{"type":"object","properties":{"path":{"type":"string"},"line":{"type":"integer"},"context_lines":{"type":"integer"}},"required":["path"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let path = required_str_arg(args, "path")?;
        let line = u32_arg(args, "line");
        let context_lines = usize_arg(args, "context_lines");
        handlers::context::get_code_snippet(
            &ctx.graph,
            &ctx.overlay,
            &ctx.workspace,
            &path,
            line,
            context_lines,
        )
    }
}
inventory::submit!(ToolHandlerEntry(&GetCodeSnippetHandler));

pub struct GetCallSitesHandler;
#[async_trait]
impl ToolHandler for GetCallSitesHandler {
    tool_meta!(
        "get_call_sites",
        "Finds all callers of a given symbol",
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::context::get_call_sites(&ctx.workspace, &ctx.graph, &ctx.overlay, &symbol)
    }
}
inventory::submit!(ToolHandlerEntry(&GetCallSitesHandler));

// ─── GitOps Domain ─────────────────────────────────────────────────────────────

pub struct GetFileDiffHandler;
#[async_trait]
impl ToolHandler for GetFileDiffHandler {
    tool_meta!(
        "get_file_diff",
        "Shows uncommitted changes (staged and unstaged)",
        r#"{"type":"object","properties":{"path":{"type":"string"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let path = if str_arg(args, "path").is_empty() {
            None
        } else {
            Some(str_arg(args, "path"))
        };
        handlers::gitops::get_file_diff(&ctx.git, path.as_deref())
    }
}
inventory::submit!(ToolHandlerEntry(&GetFileDiffHandler));

pub struct GetCommitHistoryHandler;
#[async_trait]
impl ToolHandler for GetCommitHistoryHandler {
    tool_meta!(
        "get_commit_history",
        "Shows recent commit history with author and message",
        r#"{"type":"object","properties":{"limit":{"type":"integer"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let limit = usize_arg(args, "limit");
        handlers::gitops::get_commit_history(&ctx.git, limit)
    }
}
inventory::submit!(ToolHandlerEntry(&GetCommitHistoryHandler));

pub struct GetBranchStatusHandler;
#[async_trait]
impl ToolHandler for GetBranchStatusHandler {
    tool_meta!(
        "get_branch_status",
        "Shows current branch and git status",
        r#"{"type":"object","properties":{},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        _args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::gitops::get_branch_status(&ctx.git)
    }
}
inventory::submit!(ToolHandlerEntry(&GetBranchStatusHandler));

// ─── Testing Domain ───────────────────────────────────────────────────────────

pub struct FindUntestedFunctionsHandler;
#[async_trait]
impl ToolHandler for FindUntestedFunctionsHandler {
    tool_meta!(
        "find_untested_functions",
        "Identifies functions that may lack test coverage based on call graph analysis",
        r#"{"type":"object","properties":{"limit":{"type":"integer"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let limit = usize_arg(args, "limit");
        handlers::testing::find_untested_functions(&ctx.graph, &ctx.overlay, limit)
    }
}
inventory::submit!(ToolHandlerEntry(&FindUntestedFunctionsHandler));

pub struct GetTestTemplateHandler;
#[async_trait]
impl ToolHandler for GetTestTemplateHandler {
    tool_meta!(
        "get_test_template",
        "Generates a test scaffold for a given function or type",
        r#"{"type":"object","properties":{"function_name":{"type":"string"}},"required":["function_name"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let function_name = required_str_arg(args, "function_name")?;
        handlers::testing::get_test_template(&ctx.graph, &ctx.overlay, &function_name)
    }
}
inventory::submit!(ToolHandlerEntry(&GetTestTemplateHandler));

pub struct GetCoverageSummaryHandler;
#[async_trait]
impl ToolHandler for GetCoverageSummaryHandler {
    tool_meta!(
        "get_coverage_summary",
        "Provides a structural estimate of code coverage based on call graph connectivity",
        r#"{"type":"object","properties":{"module_path":{"type":"string"}},"required":[]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let module_path = if str_arg(args, "module_path").is_empty() {
            None
        } else {
            Some(str_arg(args, "module_path"))
        };
        handlers::testing::get_coverage_summary(&ctx.graph, &ctx.overlay, module_path.as_deref())
    }
}
inventory::submit!(ToolHandlerEntry(&GetCoverageSummaryHandler));

// ─── Semantic Domain (M6) ────────────────────────────────────────────────────

/// AGENT_UX_ROADMAP.md Milestone 6: `find_symbol` ("Where is X?").
/// Use this before `explain_symbol` / `get_context` / `assess_change`
/// to disambiguate names that occur in multiple files; the
/// underlying call sites accept a `path` argument for that.
/// Reach for the low-level `query_graph` with a `find` op for
/// structural searches; this tool is the cheap lexical index
/// path. No semantic model needed.
pub struct FindSymbolHandler;
#[async_trait]
impl ToolHandler for FindSymbolHandler {
    tool_meta!(
        "find_symbol",
        "Returns every graph node matching `name`, optionally narrowed \
         by `path_hint` (substring) and `type_filter` (function / \
         struct / trait / module / file). Use this when an agent says \
         'where is X?' and the low-level tools would otherwise force \
         a per-name lookup per match. Cost: cheap (graph index hit).",
        r#"{"type":"object","properties":{"name":{"type":"string"},"path_hint":{"type":"string"},"type_filter":{"type":"string","enum":["function","method","class","struct","interface","trait","enum","module","file"]}},"required":["name"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::semantic::find_symbol(&ctx.graph, &ctx.overlay, args)
    }
}
inventory::submit!(ToolHandlerEntry(&FindSymbolHandler));

/// AGENT_UX_ROADMAP.md Milestone 6: `get_context` ("Explain X").
/// Composes `explain_symbol`, `get_call_sites`, `trace_dependency`,
/// and `get_code_snippet`. Use this instead of calling each
/// individually — the section layout is consistent so the agent
/// can pattern-match the headers (`## Definition`, `## Callers`,
/// `## Callees`, `## Source`).
pub struct GetContextHandler;
#[async_trait]
impl ToolHandler for GetContextHandler {
    tool_meta!(
        "get_context",
        "One-call dossier for a symbol: definition, callers, \
         callees, and source body excerpt. Use this when an agent \
         says 'explain X' or 'what is X?' and the goal is a single \
         Markdown payload the agent can quote back. Cost: medium; \
         scales with `depth`. Low-level alternative: call each of \
         explain_symbol / get_call_sites / trace_dependency \
         individually. Repo-level orientation is `understand_repository`; \
         call-site dispatch honesty is `explain_dispatch`.",
        r#"{"type":"object","properties":{"symbol":{"type":"string"},"depth":{"type":"integer","minimum":0,"maximum":3}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::semantic::get_context(
            &ctx.workspace,
            &ctx.graph,
            &ctx.overlay,
            &ctx.occupancy,
            args,
        )
        .await
    }
}
inventory::submit!(ToolHandlerEntry(&GetContextHandler));

/// AGENT_UX_ROADMAP.md Milestone 6: `find_related` ("What is
/// connected to X?"). Composes `trace_dependency`,
/// `get_coupling_radar`, and `semantic_search` (when an NLP model
/// is loaded; the semantic section degrades gracefully without
/// one). Use this instead of calling the three composition
/// pieces individually.
pub struct FindRelatedHandler;
#[async_trait]
impl ToolHandler for FindRelatedHandler {
    tool_meta!(
        "find_related",
        "Graph neighbors, co-change partners, and (optional) \
         semantic neighbors for a symbol in one call. Use this when \
         an agent says 'what is connected to X?' or wants to \
         understand blast radius without committing to a single \
         change yet. The semantic section is omitted when no \
         embedding model is loaded; the co-change section is \
         gated by `include_coupling=false`. Cost: medium.",
        r#"{"type":"object","properties":{"symbol":{"type":"string"},"include_coupling":{"type":"boolean"},"limit":{"type":"integer"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::semantic::find_related(
            &ctx.graph,
            &ctx.overlay,
            &ctx.workspace,
            &ctx.embedder,
            &ctx.cross_encoder,
            &ctx.embedding_cache,
            &ctx.tuning,
            args,
            ui_link(ctx),
        )
        .await
    }
}
inventory::submit!(ToolHandlerEntry(&FindRelatedHandler));

/// AGENT_UX_ROADMAP.md Milestone 6: `assess_change` ("What breaks
/// if I change X?"). Composes `get_blast_radius`,
/// `get_call_sites`, `find_untested_functions`, and
/// `get_coupling_radar`. Use this before any actual edit; the
/// `risk` summary at the end is the single-line verdict an agent
/// can quote back to a human.
pub struct AssessChangeHandler;
#[async_trait]
impl ToolHandler for AssessChangeHandler {
    tool_meta!(
        "assess_change",
        "Pre-edit impact assessment: direct + transitive \
         dependents, untested dependents, co-change partners, and \
         a one-line risk verdict (low / medium / high). Use this \
         when an agent says 'what breaks if I change X?' or wants \
         to evaluate a change before editing. Cost: medium; scales \
         with the depth of the call graph.",
        r#"{"type":"object","properties":{"symbol":{"type":"string"},"depth":{"type":"string"},"include_tests":{"type":"boolean"},"limit":{"type":"integer"}},"required":["symbol"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::semantic::assess_change(
            &ctx.graph,
            &ctx.overlay,
            &ctx.workspace,
            args,
            ui_link(ctx),
        )
        .await
    }
}
inventory::submit!(ToolHandlerEntry(&AssessChangeHandler));

/// AGENT_UX_ROADMAP.md Milestone 6: `search_code` ("Find code that
/// does Y"). Mode-dispatched: `lexical` (graph name index),
/// `semantic` (NLP model — needs `SemanticRequired`), or `auto`
/// (default; tries semantic first, falls back to lexical and
/// records the fallback in the response).
pub struct SearchCodeHandler;
#[async_trait]
impl ToolHandler for SearchCodeHandler {
    tool_meta!(
        "search_code",
        "Find code by name, intent, or pattern. `mode=lexical` \
         (default) uses the graph name index and works without an \
         embedding model; `mode=semantic` uses local ONNX \
         embeddings and requires a loaded model (`install.sh \
         --download-model`). `mode=auto` (the default) tries \
         semantic first and falls back to lexical; the response \
         records `fell_back=true` so the agent can tell. Cost: \
         cheap for lexical, medium for semantic.",
        r#"{"type":"object","properties":{"query":{"type":"string"},"mode":{"type":"string","enum":["lexical","semantic","auto"]},"limit":{"type":"integer"}},"required":["query"]}"#,
        ReadOnly
    );
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let mut text = handlers::semantic::search_code(
            &ctx.workspace,
            &ctx.graph,
            &ctx.overlay,
            &ctx.embedder,
            &ctx.cross_encoder,
            &ctx.embedding_cache,
            &ctx.tuning,
            args,
        )?;
        // Semantic answers come from whatever the background pass has
        // embedded so far; say so while it runs, or an agent takes a
        // partial ranking as final.
        if text.contains("mode=semantic") {
            if let Some(e) = ctx.readiness.snapshot().embeddings.filter(|e| e.running) {
                text.push_str(&format!(
                    "\n\n⏳ Semantic index still building: {} of {} symbols embedded. \
                     Rankings may change until it finishes; `get_capabilities` shows \
                     semantic_search as warming_up until then.",
                    e.embedded, e.total
                ));
            }
        }
        Ok(text)
    }
}
inventory::submit!(ToolHandlerEntry(&SearchCodeHandler));

// ─── Cross-cutting: open-annotations appendix for explain/blast ────────────
//
// `explain_symbol` and `get_blast_radius` both surface an
// `### Open annotations` section so the agent's first call about a
// symbol surfaces the human notes left there ("Auto-include in `explain_symbol` / `get_blast_radius` markdown").
// This helper does the lookup against the live annotation registry
// attached to the executor's `ToolContext` and formats the appendix
// the way `annotation_tools::format_open_annotations_section` lays it
// out. Returning an empty string when no annotations match keeps the
// wire contract unchanged for the common case.

/// Render the `### Open annotations` section to append to
/// `explain_symbol` / `get_blast_radius` output for the given
/// symbol. Returns an empty string when:
/// - the executor has no live annotation registry wired (default
///   temp-dir backend has no rows for any repo), OR
/// - the active federation has no repos to attribute annotations to
///   (single-workspace mode without a federation), OR
/// - the lookup itself finds no open annotations on the symbol.
///
/// Lookups happen by `AnnotationTarget::Symbol { symbol }`. The
/// target filter inside the annotation store is exact-match by
/// `(kind, target)`, so a caller asking about `orchestrate` will
/// only see annotations whose target was explicitly `Symbol
/// { "orchestrate" }`. That is intentional — auto-including
/// annotations on file- or repo-typed targets would change the
/// semantic of "what is this symbol about" in surprising ways.
fn open_annotations_for_symbol(ctx: &ToolContext, symbol: &str) -> String {
    use crate::federation::repo_id::RepoId;
    use crate::server::annotations::AnnotationTarget;
    use crate::server::mcp::annotation_tools::{
        format_open_annotations_section, summaries_for_targets_in_registry,
    };

    // Federation-mode only: a single-repo federation or a multi-repo
    // one with the dispatcher's `repo_id` injection lands here with
    // `ctx.federation = Some(_)`. Single-workspace executors carry
    // `federation = None` and have no per-repo store to attribute
    // annotations to, so the lookup is skipped — the same trade-off
    // the dedicated annotation MCP tools already make via
    // `target_to_repo`.
    let Some(fed) = ctx.federation.as_ref() else {
        return String::new();
    };
    let Some((rid, _)) = fed.list_repos().first().cloned() else {
        return String::new();
    };
    let repo = RepoId::new(rid.as_str()).unwrap_or(rid);
    let targets = [AnnotationTarget::Symbol {
        symbol: symbol.to_string(),
    }];
    let summaries = summaries_for_targets_in_registry(&ctx.annotations, &repo, &targets);
    format_open_annotations_section(&summaries)
}

/// The directory a build/test/lint tool runs in: the workspace, or a
/// `cwd` inside it. A `cwd` elsewhere (`../other-project`, `/tmp/x`) ran
/// that directory's build and test scripts — arbitrary commands, reachable
/// by any MCP caller.
fn run_cwd(ctx: &ToolContext, args: &Map<String, Value>) -> Result<String, LainError> {
    let cwd = str_arg(args, "cwd");
    if cwd.is_empty() {
        return Ok(ctx.workspace.to_string_lossy().to_string());
    }
    crate::server::tools::handlers::context::resolve_against_workspace(&ctx.workspace, &cwd)
}
