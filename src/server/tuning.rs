//! Tuning configuration — algorithm constants loaded from .lain/tuning.toml.
//! Hot-reloadable at runtime via the set_tuning_config tool.
//!
//! Config file: .lain/tuning.toml (TOML format)

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Tuning parameters for graph construction and query ranking.
/// Loaded from .lain/tuning.toml in the workspace root.
#[derive(Clone, Debug, Serialize, Deserialize)]
/// Every field defaults, so a `tuning.toml` may set only the keys it
/// cares about. Without container-level `serde(default)` a partial file
/// failed to parse outright, `load_tuning_config` logged a warning most
/// operators never see, and *every* setting silently reverted — so a
/// file setting one key was worse than no file at all.
#[serde(default)]
pub struct TuningConfig {
    /// Semantic search: minimum cosine similarity to include a result.
    /// Range: [0.0, 1.0]. Higher = more precise, lower = more recall.
    pub semantic_similarity_threshold: f32,
    /// Semantic search: weight for anchor_score in hybrid ranking.
    /// hybrid = similarity + anchor_weight * anchor_score.
    /// Range: [0.0, 1.0]. Higher = favor structurally important nodes.
    pub anchor_weight: f32,
    /// Semantic search: weight for token-overlap (lexical) score in the
    /// final ranking. 0.0 = pure semantic (default, preserves existing
    /// behavior); 0.3 = strong lexical boost. Lets exact-term queries
    /// ("Tokenizer", "GraphDatabase::save") surface when cosine
    /// similarity alone is borderline.
    /// Final score = (1 - lexical_weight) * sim + lexical_weight * lex.
    pub lexical_weight: f32,
    /// Semantic search: string prepended to the user's query before
    /// embedding. Empty = no prefix (default, preserves MiniLM behavior).
    /// BGE retrieval models expect
    /// "Represent this sentence for searching relevant passages: "
    /// prepended to short queries for optimal alignment with their
    /// document embeddings. Set this to use BGE-style asymmetric
    /// retrieval. Documents are NOT prefixed.
    pub query_prefix: String,
    /// Semantic search: number of top bi-encoder candidates to rerank
    /// with the cross-encoder. 0 = disabled (default, preserves
    /// existing behavior). Each rerank adds ~50ms per candidate, so
    /// 20 is a reasonable upper bound (1s extra latency for a much
    /// more accurate top-K).
    pub cross_encoder_top_k: usize,
    /// Ingestion: ceiling on cross-boundary coupling edges.
    /// Set to 0 to disable pattern edges.
    pub max_pattern_edges: usize,
    /// Ingestion: controls parallel scanning and memory usage.
    pub ingestion: IngestionConfig,
    /// Execution: timeouts for command/tool execution.
    pub runtime: RuntimeConfig,
    /// Multiplayer: session lifetimes and presence-state locking.
    #[serde(default)]
    pub presence: PresenceConfig,
}

impl Default for TuningConfig {
    fn default() -> Self {
        Self {
            semantic_similarity_threshold: 0.3,
            anchor_weight: 0.3,
            lexical_weight: 0.0,
            query_prefix: String::new(),
            cross_encoder_top_k: 20,
            max_pattern_edges: 200,
            ingestion: IngestionConfig::default(),
            runtime: RuntimeConfig::default(),
            presence: PresenceConfig::default(),
        }
    }
}

/// Ingestion pipeline tuning — affects scanning, embedding, and graph construction.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
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
    /// NLP: cap on intra-op threads per embedding call. 0 = auto-detect
    /// (uses min(system cores, 4) — 4 is enough for bge-small/bge-base
    /// inference; more threads doesn't help and burns CPU).
    /// Higher values help on machines with many idle cores; lower values
    /// help when sharing the box with other workloads.
    pub nlp_max_threads: usize,
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
            nlp_max_threads: 0, // 0 = auto-detect (min(cores, 4))
            ui_session_ttl_secs: 600,
            default_query_limit: 100,
        }
    }
}


/// Multiplayer tuning — session lifetimes and the locks around shared
/// presence state.
///
/// These were compile-time constants scattered across `presence`,
/// `attribution` and `state_lock`, which meant an operator whose agents
/// or filesystem behaved differently had no way to adjust them. Every
/// other timeout in lain is tunable; these are now too.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PresenceConfig {
    /// How long an interactive agent may go without proof of life.
    ///
    /// Was 60s, which is shorter than a single LLM turn: an agent that
    /// claimed a file, reasoned about it, and came back found its
    /// session expired and its claims silently released. Any
    /// authenticated tool call now counts as a heartbeat, so this is a
    /// backstop for a departed agent rather than a liveness treadmill.
    pub interactive_session_ttl_secs: u64,
    /// Same, for background (cron / CI) agents. Kept short: they are
    /// scripted, so heartbeating on a schedule is something they can
    /// actually do, and a wedged one should give its claims back fast.
    pub background_session_ttl_secs: u64,
    /// How long a claim inferred by the attribution watcher survives
    /// without being re-observed. Inferred claims are a guess; a wrong
    /// one must heal itself rather than stick until the session dies.
    pub inferred_claim_ttl_secs: u64,
    /// How long to retry the presence state-file lock before proceeding
    /// without it. The layer is advisory: losing a concurrent write is
    /// a nuisance, wedging an agent's tool call is not.
    pub state_lock_acquire_timeout_ms: u64,
    /// Gap between attempts while waiting for that lock.
    pub state_lock_retry_interval_ms: u64,
    /// A state lock older than this is presumed abandoned by a dead
    /// holder and may be taken over.
    pub state_lock_stale_after_secs: u64,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            interactive_session_ttl_secs: 600,
            background_session_ttl_secs: 60,
            inferred_claim_ttl_secs: 120,
            state_lock_acquire_timeout_ms: 2000,
            state_lock_retry_interval_ms: 20,
            state_lock_stale_after_secs: 10,
        }
    }
}

/// Runtime tuning — timeouts and limits for command execution and LSP operations.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
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
        match toml::from_str::<TuningConfig>(&contents) {
            Ok(config) => {
                tracing::info!("Loaded tuning config from {:?}", path);
                return config;
            }
            Err(e) => tracing::warn!("Failed to parse tuning.toml at {:?}: {}", path, e),
        }
    }
    tracing::info!("No tuning.toml found, using defaults");
    TuningConfig::default()
}

// `save_tuning_config` lived here: a writer for `.lain/tuning.toml`
// with no caller and no test. `tuning.toml` is authored by hand (the
// README documents editing it directly) and only ever read back by
// `load_tuning_config`, so nothing in the product ever wrote one.

#[cfg(test)]
mod partial_config_tests {
    //! A `tuning.toml` must be able to set one key.
    //!
    //! Without container-level `serde(default)` a partial file failed to
    //! parse, `load_tuning_config` logged a warning to a stream most
    //! operators never read, and every setting silently reverted to its
    //! default — so the documented workflow ("set `query_prefix` in
    //! `.lain/tuning.toml`") quietly did nothing.
    use super::*;

    #[test]
    fn a_single_key_file_keeps_every_other_default() {
        let cfg: TuningConfig = toml::from_str(r#"query_prefix = "Represent: ""#).unwrap();
        assert_eq!(cfg.query_prefix, "Represent: ");
        assert_eq!(cfg.semantic_similarity_threshold, 0.3);
        assert_eq!(cfg.presence.interactive_session_ttl_secs, 600);
    }

    #[test]
    fn a_partial_nested_table_keeps_its_siblings() {
        let cfg: TuningConfig =
            toml::from_str("[presence]\ninteractive_session_ttl_secs = 900\n").unwrap();
        assert_eq!(cfg.presence.interactive_session_ttl_secs, 900);
        assert_eq!(cfg.presence.background_session_ttl_secs, 60);
        assert_eq!(cfg.presence.inferred_claim_ttl_secs, 120);
    }

    #[test]
    fn an_empty_file_is_the_default_config() {
        let cfg: TuningConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.presence.interactive_session_ttl_secs, 600);
        assert_eq!(cfg.ingestion.lsp_pool_size, IngestionConfig::default().lsp_pool_size);
        assert_eq!(
            cfg.runtime.default_test_timeout_secs,
            RuntimeConfig::default().default_test_timeout_secs
        );
    }
}

#[cfg(test)]
mod knob_reachability_tests {
    //! Every documented tuning knob must be read by something.
    //!
    //! Seven of them were not. `.lain/tuning.toml` accepted
    //! `default_command_timeout_secs`, `lsp_symbol_poll_timeout_secs`,
    //! `lsp_symbol_poll_interval_ms`, `max_pattern_edges`,
    //! `ui_session_ttl_secs`, `default_query_limit` and
    //! `ready_threshold`, and nothing anywhere read any of them — in
    //! several cases because the value they were meant to control was
    //! still a literal a few files away, with the same number in it.
    //! Editing the documented setting did nothing, silently.

    use std::path::Path;

    /// Source of every production `.rs` file, with `#[cfg(test)]` blocks
    /// and `*_tests.rs` files excluded — a knob "read" only by its own
    /// round-trip test is still dead in production.
    fn production_sources() -> Vec<(String, String)> {
        fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                    out.push(p);
                }
            }
        }
        fn strip_tests(src: &str) -> String {
            let mut out = String::new();
            let mut i = 0;
            while let Some(rel) = src[i..].find("#[cfg(test)]") {
                let start = i + rel;
                out.push_str(&src[i..start]);
                let Some(open) = src[start..].find('{').map(|o| start + o) else { break };
                let (mut depth, mut k) = (0usize, open);
                for (idx, ch) in src[open..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                k = open + idx;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                i = k + 1;
            }
            out.push_str(&src[i.min(src.len())..]);
            out
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        files
            .into_iter()
            .filter(|p| {
                !p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("_tests.rs"))
            })
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                Some((p.display().to_string(), strip_tests(&text)))
            })
            .collect()
    }

    /// A knob counts as read when it appears somewhere other than the
    /// file that declares it.
    fn is_read(knob: &str, declaring_file: &str, sources: &[(String, String)]) -> bool {
        sources.iter().any(|(path, text)| {
            !path.ends_with(declaring_file)
                && text.lines().any(|l| {
                    let t = l.trim();
                    !t.starts_with("//") && !t.starts_with("///") && t.contains(knob)
                })
        })
    }

    #[test]
    fn every_tuning_knob_is_read_by_production_code() {
        let sources = production_sources();
        assert!(!sources.is_empty());

        let knobs: &[(&str, &str)] = &[
            ("max_pattern_edges", "tuning.rs"),
            ("ui_session_ttl_secs", "tuning.rs"),
            ("default_query_limit", "tuning.rs"),
            ("default_command_timeout_secs", "tuning.rs"),
            ("default_test_timeout_secs", "tuning.rs"),
            ("lsp_symbol_poll_timeout_secs", "tuning.rs"),
            ("lsp_symbol_poll_interval_ms", "tuning.rs"),
            ("lsp_pool_size", "tuning.rs"),
            ("nlp_max_threads", "tuning.rs"),
            ("cochange_commit_window", "tuning.rs"),
            ("cochange_min_pair_count", "tuning.rs"),
            ("cochange_max_commit_files", "tuning.rs"),
            // Lives in `federation/config.rs`, same failure mode.
            ("ready_threshold", "federation/config.rs"),
            ("max_concurrent_indexers", "federation/config.rs"),
        ];

        let mut unread = Vec::new();
        for (knob, decl) in knobs {
            if !is_read(knob, decl, &sources) {
                unread.push(*knob);
            }
        }

        assert!(
            unread.is_empty(),
            "these tuning knobs are accepted from `.lain/tuning.toml` and read \
             by nothing — setting them does nothing, silently:\n  {}",
            unread.join("\n  ")
        );
    }
}
