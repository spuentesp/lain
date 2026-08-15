//! Data types for error decoration

use std::path::PathBuf;

/// Severity level for parsed diagnostics
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

impl Default for Severity {
    fn default() -> Self {
        Severity::Error
    }
}

/// A parsed error/warning from command output
#[derive(Debug, Clone)]
pub struct ParsedError {
    pub path: PathBuf,
    pub line: u32,
    pub column: Option<u32>,
    pub severity: Severity,
    pub message: String,
    pub code: Option<String>,
}

/// Per-error enriched data (cheap to compute)
#[derive(Debug, Clone)]
pub struct EnrichedError {
    pub error: ParsedError,
    pub symbol: Option<String>,
    pub anchor_score: Option<f32>,
}

/// Aggregate summary of all failures
#[derive(Debug, Clone)]
pub struct FailureSummary {
    pub affected_files: Vec<PathBuf>,
    pub affected_symbols: Vec<String>,
    pub combined_blast_radius: Vec<String>,
    pub co_change_partners: Vec<(String, usize)>,
    pub architectural_note: Option<String>,
}

/// Full enrichment report
#[derive(Debug, Clone)]
pub struct EnrichedReport {
    pub errors: Vec<EnrichedError>,
    pub summary: FailureSummary,
}
