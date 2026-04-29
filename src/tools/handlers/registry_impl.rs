//! `ToolHandler` implementations for all tools — auto-discovered via `inventory`.
//!
//! Each `impl ToolHandler` block calls `inventory::submit!(ToolHandlerEntry(&handler))`
//! at the end, registering the tool with the global registry.
//!
//! Adding a new tool: implement the handler here. No central edit needed.

use crate::error::LainError;
use crate::tools::handlers;
use crate::tools::registry::{ToolCapability, ToolContext, ToolHandler, ToolHandlerEntry};
use async_trait::async_trait;
use inventory;
use serde_json::{Map, Value};

// ─── Helper macros ────────────────────────────────────────────────────────────

/// Helper to extract a string argument from the args map.
fn str_arg(args: &Map<String, Value>, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Helper to extract a required string argument.
fn required_str_arg(args: &Map<String, Value>, key: &str) -> Result<String, LainError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| LainError::NotFound(format!("Missing required argument: {}", key)))
}

/// Helper to extract an optional usize argument.
fn usize_arg(args: &Map<String, Value>, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as usize)
}

/// Helper to extract an optional bool argument.
fn bool_arg(args: &Map<String, Value>, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

/// Helper to extract an optional u32 argument.
fn u32_arg(args: &Map<String, Value>, key: &str) -> Option<u32> {
    args.get(key).and_then(|v| v.as_u64()).map(|n| n as u32)
}

/// Helper to extract an optional string argument (returns empty string if missing).
fn opt_str_arg(args: &Map<String, Value>, key: &str) -> String {
    str_arg(args, key)
}

// ─── Architecture handlers ─────────────────────────────────────────────────────

// ─── Architecture Domain ───────────────────────────────────────────────────────

pub struct ExploreArchitectureHandler;
#[async_trait]
impl ToolHandler for ExploreArchitectureHandler {
    fn name(&self) -> &'static str {
        "explore_architecture"
    }
    fn description(&self) -> &'static str {
        "Returns a high-level tree of files and modules up to a specific depth"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"max_depth":{"type":"integer"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let max_depth = usize_arg(args, "max_depth").unwrap_or(2);
        handlers::architecture::explore_architecture(&ctx.graph, &ctx.overlay, max_depth)
    }
}
inventory::submit!(ToolHandlerEntry(&ExploreArchitectureHandler));

pub struct ListEntryPointsHandler;
#[async_trait]
impl ToolHandler for ListEntryPointsHandler {
    fn name(&self) -> &'static str {
        "list_entry_points"
    }
    fn description(&self) -> &'static str {
        "Identifies the architectural hearts of the system (main, App, etc.)"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "compare_modules"
    }
    fn description(&self) -> &'static str {
        "Compares stability and coupling metrics between two modules"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"module_a":{"type":"string"},"module_b":{"type":"string"}},"required":["module_a","module_b"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "architectural_observations"
    }
    fn description(&self) -> &'static str {
        "Analyzes the codebase for architectural patterns, cross-boundary couplings, and high-fan-out modules"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"min_fan_out":{"type":"integer"},"min_pattern_files":{"type":"integer"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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

// ─── Navigation Domain ─────────────────────────────────────────────────────────

pub struct TraceDependencyHandler;
#[async_trait]
impl ToolHandler for TraceDependencyHandler {
    fn name(&self) -> &'static str {
        "trace_dependency"
    }
    fn description(&self) -> &'static str {
        "Recursively finds everything a symbol depends on"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "get_call_chain"
    }
    fn description(&self) -> &'static str {
        "Finds the exact path of function calls between two points"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"}},"required":["from","to"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
            &from,
            &to,
            Some(&ctx.ui_sessions),
        )
    }
}
inventory::submit!(ToolHandlerEntry(&GetCallChainHandler));

pub struct NavigateToAnchorHandler;
#[async_trait]
impl ToolHandler for NavigateToAnchorHandler {
    fn name(&self) -> &'static str {
        "navigate_to_anchor"
    }
    fn description(&self) -> &'static str {
        "Finds the most foundational 'Anchor' node that controls a given leaf function"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "get_layered_map"
    }
    fn description(&self) -> &'static str {
        "Returns a 'slice' of the architecture at a specific depth from the entry point"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"layer":{"type":"integer"},"granularity":{"type":"string"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let layer = usize_arg(args, "layer").unwrap_or(0);
        let granularity = opt_str_arg(args, "granularity");
        handlers::navigation::get_layered_map(&ctx.graph, &ctx.overlay, layer, &granularity)
    }
}
inventory::submit!(ToolHandlerEntry(&GetLayeredMapHandler));

pub struct GetMasterMapHandler;
#[async_trait]
impl ToolHandler for GetMasterMapHandler {
    fn name(&self) -> &'static str {
        "get_master_map"
    }
    fn description(&self) -> &'static str {
        "Get a high-level Staleness Report showing when each module was last synced from LSP and Git"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "semantic_search"
    }
    fn description(&self) -> &'static str {
        "Find code by intent/concept using local NLP vectors (e.g., 'Where is auth handled?')"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        if ctx.embedder.is_stub() {
            return Err(LainError::Unavailable(
                "Semantic search unavailable: NLP model not loaded. Install embeddings with: lain install-embeddings".to_string(),
            ));
        }
        let query = required_str_arg(args, "query")?;
        let limit = usize_arg(args, "limit").unwrap_or(10);
        handlers::search::semantic_search(
            &ctx.graph,
            &ctx.overlay,
            &ctx.embedder,
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
    fn name(&self) -> &'static str {
        "get_blast_radius"
    }
    fn description(&self) -> &'static str {
        "Calculates the transitive impact and ripple effect of changing a symbol"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"},"include_coupling":{"type":"boolean"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        let include_coupling = bool_arg(args, "include_coupling").unwrap_or(false);
        handlers::impact::get_blast_radius(
            &ctx.graph,
            &ctx.overlay,
            &symbol,
            include_coupling,
            Some(&ctx.ui_sessions),
        )
    }
}
inventory::submit!(ToolHandlerEntry(&GetBlastRadiusHandler));

pub struct GetCouplingRadarHandler;
#[async_trait]
impl ToolHandler for GetCouplingRadarHandler {
    fn name(&self) -> &'static str {
        "get_coupling_radar"
    }
    fn description(&self) -> &'static str {
        "Identifies 'Hidden Coupling' between files based on historical Git co-change patterns"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::impact::get_coupling_radar(
            &ctx.graph,
            &ctx.overlay,
            &symbol,
            Some(&ctx.ui_sessions),
        )
    }
}
inventory::submit!(ToolHandlerEntry(&GetCouplingRadarHandler));

// ─── Metrics Domain ───────────────────────────────────────────────────────────

pub struct FindAnchorsHandler;
#[async_trait]
impl ToolHandler for FindAnchorsHandler {
    fn name(&self) -> &'static str {
        "find_anchors"
    }
    fn description(&self) -> &'static str {
        "Lists the top 10 most foundational/stable components in the codebase"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"limit":{"type":"integer"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let limit = usize_arg(args, "limit").unwrap_or(10);
        handlers::metrics::find_anchors(&ctx.graph, &ctx.overlay, limit)
    }
}
inventory::submit!(ToolHandlerEntry(&FindAnchorsHandler));

pub struct GetAnchorScoreHandler;
#[async_trait]
impl ToolHandler for GetAnchorScoreHandler {
    fn name(&self) -> &'static str {
        "get_anchor_score"
    }
    fn description(&self) -> &'static str {
        "Returns the architectural stability score for a specific symbol"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "get_context_depth"
    }
    fn description(&self) -> &'static str {
        "Calculates layers of abstraction from the entry point for a symbol"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "find_dead_code"
    }
    fn description(&self) -> &'static str {
        "Identifies reachable nodes with zero incoming callers or usages"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"like":{"type":"string","description":"Filter dead code semantically (e.g., \"auth handler\")"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let like = args.get("like").and_then(|v| v.as_str());
        handlers::metrics::find_dead_code(
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
    fn name(&self) -> &'static str {
        "explain_symbol"
    }
    fn description(&self) -> &'static str {
        "Combines signatures, docstrings, and metrics into a human-readable architectural summary"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::metrics::explain_symbol(&ctx.graph, &ctx.overlay, &symbol)
    }
}
inventory::submit!(ToolHandlerEntry(&ExplainSymbolHandler));

pub struct SuggestRefactorTargetsHandler;
#[async_trait]
impl ToolHandler for SuggestRefactorTargetsHandler {
    fn name(&self) -> &'static str {
        "suggest_refactor_targets"
    }
    fn description(&self) -> &'static str {
        "Identifies 'God Objects' and high-debt refactor targets based on complexity and stability"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"limit":{"type":"integer"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "query_graph"
    }
    fn description(&self) -> &'static str {
        "Execute a query against the graph using a JSON ops array"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"query":{"type":"object"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::query::query_graph(&ctx.graph, &ctx.embedder, &ctx.embedding_cache, Some(args))
    }
}
inventory::submit!(ToolHandlerEntry(&QueryGraphHandler));

pub struct DescribeSchemaHandler;
#[async_trait]
impl ToolHandler for DescribeSchemaHandler {
    fn name(&self) -> &'static str {
        "describe_schema"
    }
    fn description(&self) -> &'static str {
        "Returns the graph schema (node types, edge types, example queries) for LLM session initialization"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "get_cross_runtime_callers"
    }
    fn description(&self) -> &'static str {
        "Find all protocol-level callers for a symbol (HTTP routes, gRPC services, GraphQL resolvers)"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"node_id":{"type":"string"}},"required":["node_id"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "run_enrichment"
    }
    fn description(&self) -> &'static str {
        "Triggers a full architectural scan and enrichment pass"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::StructuralWrite
    }
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
    fn name(&self) -> &'static str {
        "sync_state"
    }
    fn description(&self) -> &'static str {
        "Forces a re-sync of the graph with the current Git HEAD state"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::StructuralWrite
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        _args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        handlers::enrichment::sync_state(&ctx.graph, &ctx.git, &ctx.tuning.ingestion)
    }
}
inventory::submit!(ToolHandlerEntry(&SyncStateHandler));

// ─── Execution Domain ───────────────────────────────────────────────────────────

pub struct RunBuildHandler;
#[async_trait]
impl ToolHandler for RunBuildHandler {
    fn name(&self) -> &'static str {
        "run_build"
    }
    fn description(&self) -> &'static str {
        "Runs cargo build (optionally release) and returns build output and status"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"cwd":{"type":"string"},"release":{"type":"boolean"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let cwd = if opt_str_arg(args, "cwd").is_empty() {
            None
        } else {
            Some(opt_str_arg(args, "cwd"))
        };
        let release = bool_arg(args, "release").unwrap_or(false);
        handlers::execution::run_build(&ctx.graph, &ctx.overlay, cwd.as_deref(), release).await
    }
}
inventory::submit!(ToolHandlerEntry(&RunBuildHandler));

pub struct RunTestsHandler;
#[async_trait]
impl ToolHandler for RunTestsHandler {
    fn name(&self) -> &'static str {
        "run_tests"
    }
    fn description(&self) -> &'static str {
        "Runs cargo test with optional filter and returns test results"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"cwd":{"type":"string"},"filter":{"type":"string"},"timeout_secs":{"type":"integer"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let cwd = if opt_str_arg(args, "cwd").is_empty() {
            None
        } else {
            Some(opt_str_arg(args, "cwd"))
        };
        let filter = if opt_str_arg(args, "filter").is_empty() {
            None
        } else {
            Some(opt_str_arg(args, "filter"))
        };
        let timeout_secs = usize_arg(args, "timeout_secs");
        handlers::execution::run_tests(
            &ctx.graph,
            &ctx.overlay,
            cwd.as_deref(),
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
    fn name(&self) -> &'static str {
        "run_clippy"
    }
    fn description(&self) -> &'static str {
        "Runs cargo clippy with optional auto-fix and returns lint results with architectural context on failure"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"cwd":{"type":"string"},"fix":{"type":"boolean"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::Mutating
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let cwd = if opt_str_arg(args, "cwd").is_empty() {
            None
        } else {
            Some(opt_str_arg(args, "cwd"))
        };
        let fix = bool_arg(args, "fix").unwrap_or(false);
        handlers::execution::run_clippy(&ctx.graph, &ctx.overlay, cwd.as_deref(), fix).await
    }
}
inventory::submit!(ToolHandlerEntry(&RunClippyHandler));

// ─── Context Domain ───────────────────────────────────────────────────────────

pub struct GetContextForPromptHandler;
#[async_trait]
impl ToolHandler for GetContextForPromptHandler {
    fn name(&self) -> &'static str {
        "get_context_for_prompt"
    }
    fn description(&self) -> &'static str {
        "Builds LLM-optimized context for a symbol with signature, docstring, and relationships"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"},"max_tokens":{"type":"integer"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "get_code_snippet"
    }
    fn description(&self) -> &'static str {
        "Reads a file with surrounding context around a specific line"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"path":{"type":"string"},"line":{"type":"integer"},"context_lines":{"type":"integer"}},"required":["path"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let path = required_str_arg(args, "path")?;
        let line = u32_arg(args, "line");
        let context_lines = usize_arg(args, "context_lines");
        handlers::context::get_code_snippet(&ctx.graph, &ctx.overlay, &path, line, context_lines)
    }
}
inventory::submit!(ToolHandlerEntry(&GetCodeSnippetHandler));

pub struct GetCallSitesHandler;
#[async_trait]
impl ToolHandler for GetCallSitesHandler {
    fn name(&self) -> &'static str {
        "get_call_sites"
    }
    fn description(&self) -> &'static str {
        "Finds all callers of a given symbol"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let symbol = required_str_arg(args, "symbol")?;
        handlers::context::get_call_sites(&ctx.graph, &ctx.overlay, &symbol)
    }
}
inventory::submit!(ToolHandlerEntry(&GetCallSitesHandler));

// ─── GitOps Domain ─────────────────────────────────────────────────────────────

pub struct GetFileDiffHandler;
#[async_trait]
impl ToolHandler for GetFileDiffHandler {
    fn name(&self) -> &'static str {
        "get_file_diff"
    }
    fn description(&self) -> &'static str {
        "Shows uncommitted changes (staged and unstaged)"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"path":{"type":"string"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let path = if opt_str_arg(args, "path").is_empty() {
            None
        } else {
            Some(opt_str_arg(args, "path"))
        };
        handlers::gitops::get_file_diff(&ctx.git, path.as_deref())
    }
}
inventory::submit!(ToolHandlerEntry(&GetFileDiffHandler));

pub struct GetCommitHistoryHandler;
#[async_trait]
impl ToolHandler for GetCommitHistoryHandler {
    fn name(&self) -> &'static str {
        "get_commit_history"
    }
    fn description(&self) -> &'static str {
        "Shows recent commit history with author and message"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"limit":{"type":"integer"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "get_branch_status"
    }
    fn description(&self) -> &'static str {
        "Shows current branch and git status"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "find_untested_functions"
    }
    fn description(&self) -> &'static str {
        "Identifies functions that may lack test coverage based on call graph analysis"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"limit":{"type":"integer"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "get_test_template"
    }
    fn description(&self) -> &'static str {
        "Generates a test scaffold for a given function or type"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"function_name":{"type":"string"}},"required":["function_name"]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
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
    fn name(&self) -> &'static str {
        "get_coverage_summary"
    }
    fn description(&self) -> &'static str {
        "Provides a structural estimate of code coverage based on call graph connectivity"
    }
    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"module_path":{"type":"string"}},"required":[]}"#
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn call(
        &self,
        ctx: &ToolContext,
        args: &Map<String, Value>,
    ) -> Result<String, LainError> {
        let module_path = if opt_str_arg(args, "module_path").is_empty() {
            None
        } else {
            Some(opt_str_arg(args, "module_path"))
        };
        handlers::testing::get_coverage_summary(&ctx.graph, &ctx.overlay, module_path.as_deref())
    }
}
inventory::submit!(ToolHandlerEntry(&GetCoverageSummaryHandler));
