//! Tuning configuration — algorithm constants loaded from .lain/tuning.toml.
//! Hot-reloadable at runtime via the set_tuning_config tool.
//!
//! Config file: .lain/tuning.toml (TOML format)

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Tuning parameters for graph construction and query ranking.
/// Loaded from .lain/tuning.toml in the workspace root.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TuningConfig {
    /// Semantic search: minimum cosine similarity to include a result.
    /// Range: [0.0, 1.0]. Higher = more precise, lower = more recall.
    pub semantic_similarity_threshold: f32,
    /// Semantic search: weight for anchor_score in hybrid ranking.
    /// hybrid = similarity + anchor_weight * anchor_score.
    /// Range: [0.0, 1.0]. Higher = favor structurally important nodes.
    pub anchor_weight: f32,
    /// Ingestion: ceiling on cross-boundary coupling edges.
    /// Set to 0 to disable pattern edges.
    pub max_pattern_edges: usize,
    /// Ingestion: controls parallel scanning and memory usage.
    pub ingestion: IngestionConfig,
    /// Execution: timeouts for command/tool execution.
    pub runtime: RuntimeConfig,
}

impl Default for TuningConfig {
    fn default() -> Self {
        Self {
            semantic_similarity_threshold: 0.3,
            anchor_weight: 0.3,
            max_pattern_edges: 200,
            ingestion: IngestionConfig::default(),
            runtime: RuntimeConfig::default(),
        }
    }
}

/// Ingestion pipeline tuning — affects scanning, embedding, and graph construction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IngestionConfig {
    /// Number of concurrent LSP language servers for parallel file analysis.
    /// Higher = more parallel scanning, more memory/CPU.
    pub lsp_pool_size: usize,
    /// Number of files scanned per batch task (reduces task-spawning overhead).
    pub files_per_batch: usize,
    /// Maximum files scanned per ingestion run (caps scan time on large repos).
    pub max_files_per_scan: usize,
    /// Incremental flush interval: nodes/edges written to graph between batch joins.
    /// Higher = less frequent writes, more memory pressure.
    pub ingest_batch_size: usize,
    /// Scan phase timeout before aborting stuck tasks.
    pub scan_timeout_secs: u64,
    /// Co-change analysis: skip commits touching more than this many files.
    /// Prevents O(N^2) pair explosion on mega-commits.
    pub cochange_max_commit_files: usize,
    /// Co-change analysis: number of recent commits to analyze.
    pub cochange_commit_window: usize,
    /// Co-change analysis: minimum co-change count to create an edge.
    pub cochange_min_pair_count: usize,
    /// NLP pre-warm: number of top-anchor nodes embedded before background queue.
    pub nlp_prewarm_count: usize,
    /// NLP background: nodes embedded per batch chunk.
    pub nlp_batch_size: usize,
    /// NLP background: max nodes embedded per interval pass (backpressure).
    pub nlp_budget_per_pass: usize,
    /// UI session time-to-live in seconds.
    pub ui_session_ttl_secs: u64,
    /// Default query result limit when not specified.
    pub default_query_limit: usize,
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            lsp_pool_size: 4,
            files_per_batch: 50,
            max_files_per_scan: 5000,
            ingest_batch_size: 100,
            scan_timeout_secs: 120,
            cochange_max_commit_files: 100,
            cochange_commit_window: 100,
            cochange_min_pair_count: 2,
            nlp_prewarm_count: 20,
            nlp_batch_size: 50,
            nlp_budget_per_pass: 20,
            ui_session_ttl_secs: 600,
            default_query_limit: 100,
        }
    }
}

/// Runtime tuning — timeouts and limits for command execution and LSP operations.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Default timeout for arbitrary command execution (seconds).
    pub default_command_timeout_secs: u64,
    /// Default timeout for test execution (seconds).
    pub default_test_timeout_secs: u64,
    /// LSP symbol poll timeout for document analysis (seconds).
    pub lsp_symbol_poll_timeout_secs: u64,
    /// LSP symbol poll tick interval (milliseconds).
    pub lsp_symbol_poll_interval_ms: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            default_command_timeout_secs: 60,
            default_test_timeout_secs: 300,
            lsp_symbol_poll_timeout_secs: 2,
            lsp_symbol_poll_interval_ms: 50,
        }
    }
}

/// Load full tuning config from .lain/tuning.toml in workspace.
/// Falls back to defaults if the file doesn't exist or is malformed.
pub fn load_tuning_config(workspace: &Path) -> TuningConfig {
    let path = workspace.join(".lain").join("tuning.toml");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        if let Ok(config) = toml::from_str::<TuningConfig>(&contents) {
            tracing::info!("Loaded tuning config from {:?}", path);
            return config;
        }
    }
    tracing::info!("No tuning.toml found, using defaults");
    TuningConfig::default()
}

/// Save tuning config to .lain/tuning.toml, creating the directory if needed.
pub fn save_tuning_config(workspace: &Path, config: &TuningConfig) -> std::io::Result<()> {
    let dir = workspace.join(".lain");
    std::fs::create_dir_all(&dir)?;
    let contents = toml::to_string_pretty(config).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(dir.join("tuning.toml"), contents)?;
    tracing::info!("Saved tuning config to {:?}", dir.join("tuning.toml"));
    Ok(())
}
