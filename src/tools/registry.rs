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
use crate::tools::UiSession;
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
    pub git: Arc<Mutex<GitSensor>>,
    pub lsp_pool: Arc<LspPool>,
    pub tuning: Arc<TuningConfig>,
    pub embedding_cache: Arc<Mutex<std::collections::HashMap<String, Vec<f32>>>>,
    pub ui_sessions: Arc<AsyncMutex<std::collections::HashMap<String, UiSession>>>,
    pub jobs: Arc<AsyncMutex<std::collections::HashMap<String, crate::tools::JobInfo>>>,
    pub job_webhooks: Arc<AsyncMutex<Vec<String>>>,
    pub diagnostics_port: u16,
}

impl ToolContext {
    pub fn new(
        graph: GraphDatabase,
        overlay: VolatileOverlay,
        embedder: NlpEmbedder,
        git: Arc<Mutex<GitSensor>>,
        lsp_pool: Arc<LspPool>,
        tuning: Arc<TuningConfig>,
        embedding_cache: Arc<Mutex<std::collections::HashMap<String, Vec<f32>>>>,
        ui_sessions: Arc<AsyncMutex<std::collections::HashMap<String, UiSession>>>,
        jobs: Arc<AsyncMutex<std::collections::HashMap<String, crate::tools::JobInfo>>>,
        job_webhooks: Arc<AsyncMutex<Vec<String>>>,
    ) -> Self {
        Self {
            graph,
            overlay,
            embedder,
            git,
            lsp_pool,
            tuning,
            embedding_cache,
            ui_sessions,
            jobs,
            job_webhooks,
            diagnostics_port: crate::tools::DIAGNOSTICS_PORT,
        }
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
        for entry in iter::<ToolHandlerEntry>() {
            if entry.0.name() == name {
                return entry.0.call(ctx, args).await;
            }
        }
        Err(LainError::NotFound(format!("Unknown tool: {}", name)))
    }

    /// Collect all tool definitions for MCP schema registration.
    pub fn definitions() -> Vec<crate::tools::definitions::ToolDefinition> {
        iter::<ToolHandlerEntry>()
            .map(|entry| {
                let schema: Value = serde_json::from_str(entry.0.input_schema())
                    .unwrap_or_else(|_| serde_json::json!({}));
                crate::tools::definitions::ToolDefinition {
                    name: entry.0.name(),
                    description: entry.0.description(),
                    input_schema: schema,
                }
            })
            .collect()
    }
}

// ─── Truncation policy ───────────────────────────────────────────────────────

/// A truncation policy lets a tool declare multiple representations at
/// different verbosity levels. The framework picks the cheapest one that
/// fits the available output budget.
///
/// `full`     — complete result, no truncation
/// `summary`  — one-line summary per item (typically ≤80 chars each)
/// `compact`  — a single-line headline ("N items, first 5 shown")
#[derive(Clone)]
pub struct TruncationPolicy<F, S, C> {
    pub full: F,
    pub summary: S,
    pub compact: C,
}

impl<F, S, C> TruncationPolicy<F, S, C>
where
    C: Fn() -> String,
{
    /// Pick the appropriate representation given a byte budget.
    /// Returns `compact` if the full result exceeds `budget`,
    /// otherwise the full result. Callers can then check `summary`.
    pub fn select<'a>(&self, full_result: &'a str, budget: usize) -> TruncatedOutput<'a> {
        if full_result.len() > budget {
            TruncatedOutput::Compact((self.compact)())
        } else {
            TruncatedOutput::Full(full_result)
        }
    }
}

pub enum TruncatedOutput<'a> {
    Full(&'a str),
    Compact(String),
    Summary(&'a str),
}

impl<'a> TruncatedOutput<'a> {
    pub fn as_str(&self) -> &str {
        match self {
            TruncatedOutput::Full(s) => s,
            TruncatedOutput::Compact(s) => s,
            TruncatedOutput::Summary(s) => s,
        }
    }
}
