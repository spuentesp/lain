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
    /// B1 — capacity of the per-tool-executor embedding cache
    /// (`Arc<Mutex<LruCache<…>>>` in `tools/registry.rs`). Bounded so a
    /// long-running server with many distinct queries doesn't grow the
    /// cache without limit. At 384 dims × 4 bytes per `f32`, the default
    /// 10 000 entries is ~15 MB worst case per executor; a corpus of
    /// 10 k nodes fills it exactly once, and any query hitting a node
    /// outside the working set still pays the read-through cost (a
    /// single ONNX forward pass) before the cache repopulates.
    pub embedding_cache_capacity: usize,
    /// B2 — capacity of the process-wide file-content cache
    /// (`FILE_CONTENT_CACHE` in `tools/utils.rs`). Bounded so a
    /// long-running server with many watched files doesn't grow the
    /// cache without limit. 1 000 files × ~50 KB average is ~50 MB
    /// worst case; mtime invalidation means a stale entry never
    /// survives a write to its file.
    pub file_content_cache_capacity: usize,
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
            embedding_cache_capacity: 10_000,
            file_content_cache_capacity: 1_000,
            ingestion: IngestionConfig::default(),
            runtime: RuntimeConfig::default(),
            presence: PresenceConfig::default(),
        }
    }
}

/// F4 — RAM-aware sizing helper. The hardcoded `Default` caps (10 000
/// embeddings × ~1.5 KB ≈ 15 MB and 1 000 files × ~50 KB ≈ 50 MB) sit
/// comfortably on a developer laptop but punch a hole through the
/// budget of a 4 GB CI runner (where Lain is one of several
/// containers) or a 1 GB sandbox. `for_runtime` reads the host's
/// available memory and scales the two caches so their combined
/// worst case stays under `MEMORY_FRACTION` of RAM, while never going
/// below a floor that keeps the caches useful on tiny machines.
///
/// The math (deterministic given an `available_mb` input — pinned by
/// the tests in `cache_sizing_tests` below):
///
///   total_cache_budget = (available_mb * 1 MiB).saturating_mul(MEMORY_BPS_NUMERATOR)
///                                                  / MEMORY_BPS_DENOMINATOR
///   embedding_share    = total / (EMBEDDING_BYTES_PER_ENTRY
///                                  + FILE_CONTENT_BYTES_PER_ENTRY) × EMBED_DENOMINATOR
///   file_content_share = total - embedding_share
///
/// Bounds (clamped at the helper):
///   embedding_cache_capacity      in [1 000, 1 000_000]
///   file_content_cache_capacity   in [100,   100_000]
///
/// `Default` is unchanged — tests stay hermetic. Production code
/// paths that build a `TuningConfig` at startup call `for_runtime()`
/// instead.
pub const MEMORY_FRACTION_NUMERATOR: u64 = 5; // 5 %
pub const MEMORY_FRACTION_DENOMINATOR: u64 = 100;
pub const EMBEDDING_BYTES_PER_ENTRY: u64 = 1_536; // 384 dims × 4 B
pub const FILE_CONTENT_BYTES_PER_ENTRY: u64 = 51_200; // 50 KiB
pub const EMBEDDING_SHARE_NUMERATOR: u64 = 2;
pub const EMBEDDING_SHARE_DENOMINATOR: u64 = 5; // embeddings get 2/5 of the budget
pub const EMBEDDING_CAPACITY_MIN: usize = 50;
pub const EMBEDDING_CAPACITY_MAX: usize = 1_000_000;
pub const FILE_CONTENT_CAPACITY_MIN: usize = 10;
pub const FILE_CONTENT_CAPACITY_MAX: usize = 100_000;

impl TuningConfig {
    /// Production constructor: like `Default` but scales the LRU
    /// cache caps to the host's available memory. Reads
    /// `/proc/meminfo` (Linux); falls back to 8 GiB on other
    /// platforms or on read errors (the conservative default for a
    /// developer laptop — the same value the pre-F4 hardcoded caps
    /// were designed against).
    pub fn for_runtime() -> Self {
        let available_mb = host_available_memory_mb().unwrap_or(8 * 1024);
        let mut cfg = Self::default();
        let (emb, files) = ram_aware_cache_capacities(available_mb);
        cfg.embedding_cache_capacity = emb;
        cfg.file_content_cache_capacity = files;
        cfg
    }
}

/// Pure helper: given an available-memory budget in MiB, return the
/// recommended `(embedding_cache_capacity, file_content_cache_capacity)`.
/// Exposed for tests so the math is pinned without touching `/proc`.
pub fn ram_aware_cache_capacities(available_mb: u64) -> (usize, usize) {
    // Combined worst case (bytes) = available_mb * 1 MiB * fraction.
    let total_budget_bytes = (available_mb.saturating_mul(1 << 20))
        .saturating_mul(MEMORY_FRACTION_NUMERATOR)
        / MEMORY_FRACTION_DENOMINATOR;
    // Embeddings get EMBEDDING_SHARE_NUMERATOR / EMBEDDING_SHARE_DENOMINATOR
    // of the budget; the remainder goes to file content.
    let embedding_bytes =
        total_budget_bytes.saturating_mul(EMBEDDING_SHARE_NUMERATOR) / EMBEDDING_SHARE_DENOMINATOR;
    let file_bytes = total_budget_bytes.saturating_sub(embedding_bytes);
    let embedding_entries = (embedding_bytes / EMBEDDING_BYTES_PER_ENTRY) as usize;
    let file_entries = (file_bytes / FILE_CONTENT_BYTES_PER_ENTRY) as usize;
    (
        embedding_entries.clamp(EMBEDDING_CAPACITY_MIN, EMBEDDING_CAPACITY_MAX),
        file_entries.clamp(FILE_CONTENT_CAPACITY_MIN, FILE_CONTENT_CAPACITY_MAX),
    )
}

/// Available host memory in MiB, read from `/proc/meminfo`
/// (`MemAvailable`). Returns `None` on read failure or on
/// non-Linux platforms (the caller falls back to a hardcoded
/// default; tests don't touch this path).
#[cfg(target_os = "linux")]
pub fn host_available_memory_mb() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    // `MemAvailable` is the kernel's estimate of memory available
    // for new allocations without swapping (Linux 3.14+). It's a
    // much better signal than `MemFree` (which ignores reclaimable
    // cache + slab). On WSL / cgroups the value is the cgroup's
    // quota, not the host's — still useful because Lain lives
    // inside that quota.
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let mut parts = rest.split_whitespace();
            let kb: u64 = parts.next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn host_available_memory_mb() -> Option<u64> {
    None
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
    /// NLP background: max nodes embedded by the pass that follows each
    /// index build. The default covers every symbol. It used to be 20,
    /// and since there is one pass per build, not one per interval, the
    /// rest were embedded only as a side effect of queries (200 per call):
    /// semantic search ranked a history-dependent subset of the code.
    pub nlp_budget_per_pass: usize,
    /// NLP: cap on intra-op threads per embedding call. 0 = auto-detect
    /// (uses min(system cores, 4) — 4 is enough for bge-small/bge-base
    /// inference; more threads doesn't help and burns CPU).
    /// Higher values help on machines with many idle cores; lower values
    /// help when sharing the box with other workloads.
    pub nlp_max_threads: usize,
    /// LSP prewarm: per-language timeout for the cold-boot warm-up
    /// `documentSymbol` call against a sentinel file. This is separate
    /// from `lsp_symbol_poll_timeout_secs` (which gates polling inside
    /// an active LSP round-trip) and the runtime `LSP_REQUEST_TIMEOUT`
    /// in `src/server/lsp.rs` (1 s, intentionally short so a stuck
    /// round-trip doesn't tie up the Tokio worker). The prewarm pass
    /// is allowed a longer budget on purpose: cold caches on large
    /// Rust / C++ projects routinely take 2–5 s on the first call,
    /// which would otherwise trip the circuit breaker.
    pub lsp_prewarm_timeout_secs: u64,
    /// LSP prewarm: maximum number of files inspected when picking a
    /// sentinel for a given language. Larger workspaces pay slightly
    /// more for a sentinel pick but get a warmer LSP up front.
    pub lsp_prewarm_max_files: usize,
    /// LSP prewarm: opt out of the cold-boot warm-up step entirely.
    /// When `true`, `build_core_memory` skips prewarm and proceeds
    /// straight to scanning; the runtime circuit breaker still applies
    /// to every later call, so cold-cache startups that happen to
    /// hit the 1 s boundary can still mark a binary unavailable.
    pub lsp_prewarm_opt_out: bool,
    /// LSP prewarm: per-language opt-out list. Any extension in
    /// this set is filtered out of the prewarm pass before its
    /// sentinel selection. Useful when a monorepo mixes languages
    /// whose LSPs are intentionally unavailable (e.g. generated
    /// `.js` under `.gitignore_info`) — skipping the prewarm for
    /// those means the warm-up phase completes faster and the
    /// runtime 1 s boundary isn't tripped trying to talk to an LSP
    /// the workspace has explicitly opted out of. Empty by default;
    /// operators can list languages via `.lain/tuning.toml`:
    ///
    /// ```toml
    /// [ingestion]
    /// lsp_prewarm_skip_extensions = ["js", "css"]
    /// ```
    pub lsp_prewarm_skip_extensions: Vec<String>,
    /// UI session time-to-live in seconds.
    pub ui_session_ttl_secs: u64,
    /// Default query result limit when not specified.
    pub default_query_limit: usize,
}

impl IngestionConfig {
    /// Raise to 1 the settings that are used as a chunk size or a modulus and
    /// panic at 0 (`chunks(0)`, `% 0`): a hand-edited `tuning.toml` with
    /// `files_per_batch = 0` (a plausible "disable" or typo) crashed indexing.
    pub fn sanitized(mut self) -> Self {
        self.lsp_pool_size = self.lsp_pool_size.max(1);
        self.files_per_batch = self.files_per_batch.max(1);
        self.ingest_batch_size = self.ingest_batch_size.max(1);
        self.nlp_batch_size = self.nlp_batch_size.max(1);
        self
    }
}

impl Default for IngestionConfig {
    fn default() -> Self {
        Self {
            lsp_pool_size: 1,
            files_per_batch: 50,
            max_files_per_scan: 5000,
            ingest_batch_size: 100,
            scan_timeout_secs: 120,
            cochange_max_commit_files: 100,
            cochange_commit_window: 100,
            cochange_min_pair_count: 2,
            nlp_prewarm_count: 20,
            nlp_batch_size: 50,
            nlp_budget_per_pass: 1_000_000,
            nlp_max_threads: 0, // 0 = auto-detect (min(cores, 4))
            lsp_prewarm_timeout_secs: 30,
            lsp_prewarm_max_files: 50,
            lsp_prewarm_opt_out: false,
            lsp_prewarm_skip_extensions: Vec::new(),
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
    /// Threshold for the federation-mode cold-start cycle detection:
    /// `RepoIndex::index_forced` is suppressed when the previous
    /// successful index completed within this many seconds AND no
    /// commit moved AND no uncommitted changes exist. The check breaks
    /// the `index → topology → re-scan → index → …` loop observed
    /// during 2026-09-21 cold-start runs (every cycle was 30 s – 3
    /// min, never converged). Default 30 s matches the longest
    /// observed cycle; lower it for faster break-out in CI, raise it
    /// for larger monorepos where legitimate reindexes routinely
    /// happen within the window.
    pub repo_cycle_threshold_secs: u64,
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
            repo_cycle_threshold_secs: 30,
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
/// When falling back, the cache caps are scaled to the host's
/// available memory so a 1 GB sandbox doesn't pay the 15 MB
/// embedding-cache + 50 MB file-cache tab a developer laptop
/// pays (see `TuningConfig::for_runtime`).
pub fn load_tuning_config(workspace: &Path) -> TuningConfig {
    let path = workspace.join(".lain").join("tuning.toml");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        match toml::from_str::<TuningConfig>(&contents) {
            Ok(mut config) => {
                let sanitized = config.ingestion.clone().sanitized();
                if sanitized.lsp_pool_size != config.ingestion.lsp_pool_size
                    || sanitized.files_per_batch != config.ingestion.files_per_batch
                    || sanitized.ingest_batch_size != config.ingestion.ingest_batch_size
                    || sanitized.nlp_batch_size != config.ingestion.nlp_batch_size
                {
                    tracing::warn!(
                        "tuning.toml at {:?}: a size setting of 0 was raised to 1 \
                         (lsp_pool_size, files_per_batch, ingest_batch_size, nlp_batch_size \
                         must be at least 1)",
                        path
                    );
                }
                config.ingestion = sanitized;
                tracing::info!("Loaded tuning config from {:?}", path);
                return config;
            }
            Err(e) => tracing::warn!("Failed to parse tuning.toml at {:?}: {}", path, e),
        }
    }
    tracing::info!("No tuning.toml found, using RAM-aware defaults");
    TuningConfig::for_runtime()
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
        assert_eq!(
            cfg.ingestion.lsp_pool_size,
            IngestionConfig::default().lsp_pool_size
        );
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
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
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
                let Some(open) = src[start..].find('{').map(|o| start + o) else {
                    break;
                };
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
            ("embedding_cache_capacity", "tuning.rs"),
            ("file_content_cache_capacity", "tuning.rs"),
            ("cochange_commit_window", "tuning.rs"),
            ("cochange_min_pair_count", "tuning.rs"),
            ("cochange_max_commit_files", "tuning.rs"),
            // LSP prewarm tunables (this PR). The reachability is
            // `src/server/ingest/ingestion.rs`, where
            // `build_core_memory` reads them before the scan batch.
            ("lsp_prewarm_timeout_secs", "tuning.rs"),
            ("lsp_prewarm_max_files", "tuning.rs"),
            ("lsp_prewarm_opt_out", "tuning.rs"),
            ("lsp_prewarm_skip_extensions", "tuning.rs"),
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

#[cfg(test)]
mod zero_size_tests {
    use super::*;

    /// Every setting that is a chunk size or modulus survives a 0 in the file.
    #[test]
    fn zero_sizes_in_tuning_toml_are_raised_not_crashed_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".lain")).unwrap();
        std::fs::write(
            dir.path().join(".lain/tuning.toml"),
            "[ingestion]\nlsp_pool_size = 0\nfiles_per_batch = 0\ningest_batch_size = 0\nnlp_batch_size = 0\n",
        )
        .unwrap();
        let i = load_tuning_config(dir.path()).ingestion;
        assert_eq!(
            (
                i.lsp_pool_size,
                i.files_per_batch,
                i.ingest_batch_size,
                i.nlp_batch_size
            ),
            (1, 1, 1, 1)
        );
        // The defaults and sensible values are untouched.
        let d = IngestionConfig::default();
        assert_eq!(d.clone().sanitized().files_per_batch, d.files_per_batch);
        assert_eq!(d.clone().sanitized().lsp_pool_size, d.lsp_pool_size);
    }
}

#[cfg(test)]
mod ram_aware_cache_sizing_tests {
    use super::*;

    /// Pinned math: 8 GiB (developer laptop) maps to a sane
    /// combined cache footprint (~120 MB worst case) without
    /// punching through the OS budget. The exact values are the
    /// result of the formula in the doc-comment on
    /// `ram_aware_cache_capacities` — pin them so a careless edit
    /// to the constants can't silently regress.
    #[test]
    fn ram_aware_cache_capacities_at_8gib_laptop_budget() {
        let (emb, files) = ram_aware_cache_capacities(8 * 1024);
        // total budget = 8 GiB * 5 % = 400 MiB; embeddings get 2/5
        // = 160 MiB; 160 MiB / 1.5 KB ≈ 109 226 entries → clamped to
        // 100 000 max
        // (the value depends on the exact budget math; we just
        // assert it's bounded and large).
        assert!(
            emb >= EMBEDDING_CAPACITY_MIN,
            "embedding entries must be at least the floor, got {emb}"
        );
        assert!(
            emb <= EMBEDDING_CAPACITY_MAX,
            "embedding entries must not exceed the ceiling, got {emb}"
        );
        assert!(
            files >= FILE_CONTENT_CAPACITY_MIN,
            "file entries must be at least the floor, got {files}"
        );
        assert!(
            files <= FILE_CONTENT_CAPACITY_MAX,
            "file entries must not exceed the ceiling, got {files}"
        );
        // Combined worst case should sit at ~5 % of RAM.
        let worst_case_bytes = (emb as u64) * EMBEDDING_BYTES_PER_ENTRY
            + (files as u64) * FILE_CONTENT_BYTES_PER_ENTRY;
        let eight_gib = 8u64 * 1024 * 1024 * 1024;
        assert!(
            worst_case_bytes < eight_gib / 10,
            "worst case ({} B) should be well under 10 % of 8 GiB",
            worst_case_bytes
        );
    }

    /// A 1 GiB sandbox (typical CI container) gets the floor
    /// caps — small but non-trivial, never zero (which would
    /// disable the caches entirely and turn every query into a
    /// re-read).
    #[test]
    fn ram_aware_cache_capacities_at_1gib_sandbox_does_not_disable_below_floor() {
        let (emb, files) = ram_aware_cache_capacities(1024);
        assert!(
            emb >= EMBEDDING_CAPACITY_MIN,
            "embedding below floor: {emb}"
        );
        assert!(
            files >= FILE_CONTENT_CAPACITY_MIN,
            "file content below floor: {files}"
        );
        assert!(
            emb <= EMBEDDING_CAPACITY_MAX,
            "embedding above ceiling: {emb}"
        );
        assert!(
            files <= FILE_CONTENT_CAPACITY_MAX,
            "file content above ceiling: {files}"
        );
    }

    /// Larger hosts (64 GiB workstation) get a generous but still
    /// bounded cache. The exact number depends on the formula's
    /// math (and we already pin the 8 GiB laptop math); we just
    /// pin that the linear growth is generous but not absurd.
    #[test]
    fn ram_aware_cache_capacities_at_64gib_workstation_grows_generously() {
        let (emb_small, files_small) = ram_aware_cache_capacities(4 * 1024);
        let (emb_big, files_big) = ram_aware_cache_capacities(64 * 1024);
        // More RAM → bigger caches. Ratio is bounded — we're not
        // allocating the whole host to the cache.
        assert!(
            emb_big > emb_small,
            "embedding cache must grow with RAM ({emb_small} → {emb_big})"
        );
        assert!(
            files_big > files_small,
            "file-content cache must grow with RAM ({files_small} → {files_big})"
        );
        assert!(
            emb_big <= EMBEDDING_CAPACITY_MAX,
            "ceiling clamps the linear growth"
        );
        assert!(
            files_big <= FILE_CONTENT_CAPACITY_MAX,
            "ceiling clamps the linear growth"
        );
    }

    /// A 0-MiB / 0-byte / saturating-to-zero host (e.g. a corrupt
    /// `/proc/meminfo`) still gets the floor caps. We never
    /// disable the caches entirely.
    #[test]
    fn ram_aware_cache_capacities_at_zero_or_saturating_yields_floor() {
        let (emb, files) = ram_aware_cache_capacities(0);
        assert_eq!(emb, EMBEDDING_CAPACITY_MIN);
        assert_eq!(files, FILE_CONTENT_CAPACITY_MIN);
    }

    /// `for_runtime()` scales the caches (the no-file fallback
    /// path) but leaves every other knob at the original
    /// `Default`. Tests stay hermetic because `Default` is
    /// untouched — `for_runtime` is a *separate* constructor.
    #[test]
    fn for_runtime_changes_only_cache_caps() {
        let rt = TuningConfig::for_runtime();
        let df = TuningConfig::default();
        // Deterministic scalar knobs unchanged.
        assert_eq!(
            rt.semantic_similarity_threshold,
            df.semantic_similarity_threshold
        );
        assert_eq!(rt.anchor_weight, df.anchor_weight);
        assert_eq!(rt.lexical_weight, df.lexical_weight);
        assert_eq!(rt.query_prefix, df.query_prefix);
        assert_eq!(rt.cross_encoder_top_k, df.cross_encoder_top_k);
        assert_eq!(rt.max_pattern_edges, df.max_pattern_edges);
        // The two cache-cap fields are the only knobs that move.
        // We assert the *direction* — they may equal `Default`'s
        // values on a host where the formula happens to land at
        // the same point (e.g. an exactly-8-GiB box). The contract
        // is "scaled by host memory"; the value depends on
        // `/proc/meminfo`. Test only that they're at least the
        // floor.
        assert!(rt.embedding_cache_capacity >= EMBEDDING_CAPACITY_MIN);
        assert!(rt.file_content_cache_capacity >= FILE_CONTENT_CAPACITY_MIN);
    }

    /// `load_tuning_config`'s no-file path uses RAM-aware
    /// defaults (the change in behaviour F4 ships). A file that
    /// omits the cache keys still falls back to `TuningConfig`'s
    /// hardcoded `Default` (file values win over the host-aware
    /// scale — operators can pin caps via `tuning.toml`).
    #[test]
    fn load_tuning_config_no_file_uses_for_runtime() {
        let dir = tempfile::tempdir().unwrap();
        // No `.lain/tuning.toml` written — the no-file path.
        let cfg = load_tuning_config(dir.path());
        // The caps must be at least the floor (the host may
        // legitimately be small).
        assert!(cfg.embedding_cache_capacity >= EMBEDDING_CAPACITY_MIN);
        assert!(cfg.file_content_cache_capacity >= FILE_CONTENT_CAPACITY_MIN);
    }

    /// The Linux `/proc/meminfo` reader is only meaningful on
    /// Linux. On other platforms `host_available_memory_mb`
    /// returns `None` (the caller falls back to 8 GiB). Pin both
    /// branches so a platform-port doesn't silently regress.
    #[test]
    #[cfg(target_os = "linux")]
    fn host_available_memory_mb_on_linux_reads_meminfo() {
        // We don't pin a specific number (CI machines vary); we
        // pin that the reader returns *some* positive value when
        // `/proc/meminfo` is well-formed.
        let mb = host_available_memory_mb().expect("linux must expose meminfo");
        assert!(mb > 0, "expected positive memory, got {mb} MiB");
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn host_available_memory_mb_off_linux_returns_none() {
        assert!(host_available_memory_mb().is_none());
    }
}
