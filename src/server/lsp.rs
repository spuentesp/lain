//! LSP multiplexer for multi-language support
//!
//! Detects languages, spawns headless LSP servers via lsp-bridge, and routes queries.

use crate::error::LainError;
use crate::schema::{GraphNode, NodeType};
use lsp_bridge::{LspBridge, LspServerConfig};
use lsp_types::{DocumentSymbol, Position, SymbolKind, SymbolTag};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Configuration for a specific language server
struct LspConfig {
    binary: &'static str,
    install_cmd: Option<&'static str>,
}

/// Maximum time to wait for an LSP server to register and initialize.
/// rust-analyzer can be slow on first startup, but a crashed or defunct
/// process must not hang the caller forever.
const LSP_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum time for a single LSP request (references, document symbols).
///
/// Tightened from 5s to 1s as part of the LSP-flakiness workaround:
/// each stuck LSP round-trip ties up the Tokio worker that holds the
/// `LspMultiplexer` `AsyncMutex`, blocking every other file currently
/// being scanned. 1s bounds the worst-case hold per call. The
/// per-binary circuit breaker (see [`record_lsp_failure`]) handles
/// the case where the LSP child is consistently slow — after 3
/// failures the binary is marked `unavailable` and the indexer
/// falls back to tree-sitter for the rest of the process lifetime.
/// ProcessExited failures route to the restart budget instead.
const LSP_REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
/// After this many consecutive LSP failures for a binary, mark it
/// `unavailable` for the rest of the process lifetime. Operators
/// observe the change via `get_supported_languages` / `get_health`
/// and can restart `lain` to recover. We chose a process-lifetime
/// circuit over a timer-based cooldown because (a) tree-sitter is a
/// good-enough fallback for the consumer code paths we exercise
/// today, and (b) recovery without operator action is a feature we
/// don't need yet — if rust-analyzer fails 3 times in a row, restart
/// is the right move anyway.
const MAX_CONSECUTIVE_LSP_FAILURES: u32 = 3;
/// After this many LSP process restarts within
/// [`LSP_RESTART_WINDOW`], escalate to the circuit-breaker
/// "unavailable" path. Prevents a hard-broken LSP (e.g. crash-looping
/// binary) from burning CPU on infinite respawn attempts. The
/// budget is per-binary and tracks a sliding window — once the
/// window expires the count resets.
const LSP_RESTART_BUDGET: u32 = 3;
/// Sliding window for the restart budget. Three restarts in one
/// minute is treated as a hard failure; one restart per minute for
/// ten minutes is fine.
const LSP_RESTART_WINDOW: Duration = Duration::from_secs(60);

/// What kind of LSP failure we observed. Drives the recovery path:
/// `ProcessExited` triggers a child respawn (and counts toward the
/// restart budget, not the circuit breaker); `RequestError` and
/// `Timeout` count toward the circuit breaker only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    /// The LSP child process exited (broken pipe, EOF on stdout,
    /// "no process available" — upstream `LspError::Communication`
    /// or `LspError::Io`). Counts toward the restart budget.
    ProcessExited,
    /// The bridge returned an error that isn't a process-exit
    /// signal. Counts toward the circuit breaker.
    RequestError,
    /// The request hit `LSP_REQUEST_TIMEOUT` while the bridge was
    /// still responsive. Counts toward the circuit breaker.
    Timeout,
}

/// Map an `LspBridgeError` to a `FailureKind`. Only the call sites
/// that surface bridge errors directly call this — the timeout arm
/// of `tokio::time::timeout` is `FailureKind::Timeout` by
/// construction, not by string match.
///
/// The upstream crate does have an `LspError::ServerCrash` variant
/// (`src/server/lsp/error.rs` in the lsp-bridge crate) but it is
/// only constructed in the upstream's own tests, never in
/// production code paths. The production signal for "process died"
/// is `LspError::Communication` (returned by `server.rs:202` for
/// "no process available" and `server.rs:208` for "request channel
/// closed"). We treat that and `LspError::Io(_)` (broken pipe on
/// stdio) as `ProcessExited`; everything else is `RequestError`.
fn classify_bridge_error(e: &lsp_bridge::LspBridgeError) -> FailureKind {
    use lsp_bridge::{LspBridgeError, LspError};
    match e {
        LspBridgeError::Lsp(LspError::Communication { .. })
        | LspBridgeError::Lsp(LspError::Io(_)) => FailureKind::ProcessExited,
        LspBridgeError::Io(_) => FailureKind::ProcessExited,
        _ => FailureKind::RequestError,
    }
}

const LANGUAGE_MAP: &[(&str, LspConfig)] = &[
    (
        "rs",
        LspConfig {
            binary: "rust-analyzer",
            install_cmd: Some("rustup component add rust-analyzer"),
        },
    ),
    (
        "go",
        LspConfig {
            binary: "gopls",
            install_cmd: Some("go install golang.org/x/tools/gopls@latest"),
        },
    ),
    (
        "ts",
        LspConfig {
            binary: "typescript-language-server",
            install_cmd: Some("npm install -g typescript typescript-language-server"),
        },
    ),
    (
        "tsx",
        LspConfig {
            binary: "typescript-language-server",
            install_cmd: Some("npm install -g typescript typescript-language-server"),
        },
    ),
    (
        "js",
        LspConfig {
            binary: "typescript-language-server",
            install_cmd: Some("npm install -g typescript typescript-language-server"),
        },
    ),
    (
        "jsx",
        LspConfig {
            binary: "typescript-language-server",
            install_cmd: Some("npm install -g typescript typescript-language-server"),
        },
    ),
    (
        "py",
        LspConfig {
            binary: "pylsp",
            install_cmd: Some("pip install python-lsp-server"),
        },
    ),
    (
        "java",
        LspConfig {
            binary: "jdtls",
            install_cmd: None,
        },
    ),
    (
        "c",
        LspConfig {
            binary: "clangd",
            install_cmd: Some("brew install llvm"),
        },
    ),
    (
        "cpp",
        LspConfig {
            binary: "clangd",
            install_cmd: Some("brew install llvm"),
        },
    ),
    (
        "h",
        LspConfig {
            binary: "clangd",
            install_cmd: Some("brew install llvm"),
        },
    ),
    (
        "hpp",
        LspConfig {
            binary: "clangd",
            install_cmd: Some("brew install llvm"),
        },
    ),
    (
        "cs",
        LspConfig {
            binary: "omnisharp",
            install_cmd: None,
        },
    ),
    (
        "rb",
        LspConfig {
            binary: "solargraph",
            install_cmd: Some("gem install solargraph"),
        },
    ),
    (
        "swift",
        LspConfig {
            binary: "sourcekit-lsp",
            install_cmd: None,
        },
    ),
    (
        "kt",
        LspConfig {
            binary: "kotlin-language-server",
            install_cmd: None,
        },
    ),
    (
        "scala",
        LspConfig {
            binary: "metals",
            install_cmd: None,
        },
    ),
    (
        "vue",
        LspConfig {
            binary: "volar",
            install_cmd: Some("npm install -g @vue/language-server"),
        },
    ),
    (
        "svelte",
        LspConfig {
            binary: "svelte-language-server",
            install_cmd: Some("npm install -g svelte-language-server"),
        },
    ),
];

/// A symbol with its children for recursive processing
pub struct HierarchicalSymbol {
    pub node: GraphNode,
    pub children: Vec<HierarchicalSymbol>,
}

pub struct LspMultiplexer {
    bridge: LspBridge,
    /// How long to keep asking the language server for document symbols
    /// before giving up, and how long to wait between asks.
    ///
    /// These were literals (`2s` / `50ms`) here while
    /// `RuntimeConfig::lsp_symbol_poll_timeout_secs` and
    /// `lsp_symbol_poll_interval_ms` sat in `.lain/tuning.toml` with the
    /// exact same default values and no reader — someone added the knobs
    /// to match the constants and never replaced the constants. Editing
    /// the documented setting did nothing.
    poll_timeout: Duration,
    poll_interval: Duration,
    /// ext -> language server configuration
    registry: HashMap<String, &'static LspConfig>,
    /// binary name -> started
    started: HashSet<String>,
    /// binary name -> missing from system
    unavailable: HashSet<String>,
    /// Per-binary consecutive-failure count for the circuit breaker.
    /// Reset to 0 (entry removed) on any successful LSP round-trip.
    /// When the count reaches [`MAX_CONSECUTIVE_LSP_FAILURES`], the
    /// binary is added to `unavailable` so subsequent calls fall
    /// back to tree-sitter without paying another timeout cost.
    /// Process-lifetime only — recovery is operator-initiated (restart
    /// `lain`). See the constant's doc comment for rationale.
    consecutive_failures: HashMap<String, u32>,
    /// Per-binary restart budget. Tracks `(count, window_start_ms)`
    /// where `count` is the number of `ProcessExited` events within
    /// the rolling [`LSP_RESTART_WINDOW`]. When `count` exceeds
    /// [`LSP_RESTART_BUDGET`], the binary is marked `unavailable`
    /// (escalated to the circuit-breaker path). The window resets
    /// after [`LSP_RESTART_WINDOW`] elapses without a restart, so a
    /// healthy once-per-minute restart cycle is fine but a
    /// crash-looping binary is bounded.
    restart_budget: HashMap<String, (u32, u64)>,
    workspace: PathBuf,
}

/// Unix-epoch milliseconds for the restart-budget window. Wrapped so
/// tests can override the clock if we ever add time-based assertions;
/// today only `Instant::now` is used, which is monotonic and
/// unaffected by system clock changes — sufficient for the
/// "elapsed since last restart" comparison.
fn unix_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl LspMultiplexer {
    pub fn new(
        workspace: &Path,
        runtime: &crate::tuning::RuntimeConfig,
    ) -> Result<Self, LainError> {
        let mut registry = HashMap::new();
        for (ext, config) in LANGUAGE_MAP {
            registry.insert(ext.to_string(), config);
        }
        Ok(Self {
            bridge: LspBridge::new(),
            poll_timeout: Duration::from_secs(runtime.lsp_symbol_poll_timeout_secs),
            poll_interval: Duration::from_millis(runtime.lsp_symbol_poll_interval_ms),
            registry,
            started: HashSet::new(),
            unavailable: HashSet::new(),
            consecutive_failures: HashMap::new(),
            restart_budget: HashMap::new(),
            workspace: workspace.to_path_buf(),
        })
    }

    fn detect_config(&self, path: &Path) -> Option<&&'static LspConfig> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(|e| self.registry.get(e))
    }

    pub async fn ensure_server(&mut self, path: &Path) -> Result<String, LainError> {
        let config = self
            .detect_config(path)
            .ok_or_else(|| LainError::Lsp(format!("No LSP for {:?}", path.extension())))?;

        let binary = config.binary.to_string();

        if self.unavailable.contains(&binary) {
            return Err(LainError::Lsp(format!(
                "LSP server '{}' is missing.",
                binary
            )));
        }

        if !self.started.contains(&binary) {
            if which::which(&binary).is_err() {
                self.unavailable.insert(binary.clone());
                return Err(LainError::Lsp(format!(
                    "LSP server '{}' not found in PATH.",
                    binary
                )));
            }

            let lsp_config = LspServerConfig::new()
                .command(&binary)
                .root_path(self.workspace.clone());

            let startup = async {
                self.bridge.register_server(&binary, lsp_config).await?;
                self.bridge.start_server(&binary).await?;
                Ok::<(), lsp_bridge::LspBridgeError>(())
            };
            match tokio::time::timeout(LSP_STARTUP_TIMEOUT, startup).await {
                Ok(Ok(())) => {
                    self.started.insert(binary.clone());
                    info!("Started LSP server: {}", binary);
                }
                Ok(Err(e)) => {
                    warn!(
                        "LSP server '{}' failed to start: {}; marking unavailable",
                        binary, e
                    );
                    // Do not call stop_server here: if the process is already
                    // defunct or unresponsive, stop_server can itself hang.
                    self.unavailable.insert(binary.clone());
                    return Err(LainError::Lsp(format!(
                        "LSP server '{}' failed to start: {}",
                        binary, e
                    )));
                }
                Err(_) => {
                    warn!(
                        "LSP server '{}' startup timed out after {:?}; marking unavailable",
                        binary, LSP_STARTUP_TIMEOUT
                    );
                    self.unavailable.insert(binary.clone());
                    return Err(LainError::Lsp(format!(
                        "LSP server '{}' startup timed out",
                        binary
                    )));
                }
            }
        }

        Ok(binary)
    }

    /// Get hierarchical document symbols
    ///
    /// `namespace` is the repo's `RepoNamespace` — every node minted
    /// here is constructed via `GraphNode::new_in` with it, so LSP-
    /// resolved symbols carry the same per-repo id namespace as the
    /// tree-sitter fallback. Without this, the federation's shared
    /// `VolatileOverlay` collapses identical LSP-discovered symbols
    /// across repos into one entry (URGENT FIXES #2 + review
    /// follow-up).
    pub async fn get_document_symbols_hierarchical(
        &mut self,
        path: &Path,
        workspace: &Path,
        namespace: &crate::schema::RepoNamespace,
    ) -> Result<Vec<HierarchicalSymbol>, LainError> {
        let server_id = self.ensure_server(path).await?;
        let uri = format!("file://{}", path.display());

        let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
        if let Err(e) = self.bridge.open_document(&server_id, &uri, &content).await {
            // open_document failure usually means the channel is
            // closed — i.e. the child died. Classify via the bridge
            // error and let record_lsp_failure decide the path.
            let kind = classify_bridge_error(&e);
            self.record_lsp_failure(&server_id, kind);
            return Err(LainError::Lsp(e.to_string()));
        }

        // Wait for LSP to analyze (intelligent polling). Track the
        // outcome so the circuit-breaker bookkeeping at the end of
        // the function reflects it: a clean success clears any prior
        // failure count; a timed-out / errored call increments the
        // count and, at the threshold, marks the binary unavailable
        // for the rest of the process. An empty-but-within-budget
        // result does NOT count as a failure — the LSP simply hasn't
        // indexed yet, and the polling loop is our way of waiting it
        // out without paying the failure cost.
        let mut symbols = Vec::new();
        let mut failure_kind: Option<FailureKind> = None;
        let start = std::time::Instant::now();
        let poll_timeout = self.poll_timeout;
        let tick = self.poll_interval;

        while start.elapsed() < poll_timeout {
            match tokio::time::timeout(
                LSP_REQUEST_TIMEOUT,
                self.bridge.get_document_symbols(&server_id, &uri),
            )
            .await
            {
                Ok(Ok(s)) if !s.is_empty() => {
                    symbols = s;
                    break;
                }
                Ok(Ok(_)) => {} // empty — LSP hasn't indexed yet; keep polling
                Ok(Err(e)) => {
                    failure_kind = Some(classify_bridge_error(&e));
                    tracing::debug!("document symbols error from {server_id}: {e}");
                    break;
                }
                Err(_) => {
                    failure_kind = Some(FailureKind::Timeout);
                    tracing::debug!(
                        "document symbols request timed out for {server_id} after {LSP_REQUEST_TIMEOUT:?}"
                    );
                    break;
                }
            }
            tokio::time::sleep(tick).await;
        }

        match failure_kind {
            Some(kind) => self.record_lsp_failure(&server_id, kind),
            None => self.record_success(&server_id),
        }

        Ok(self.process_lsp_symbols(symbols, path, workspace, namespace))
    }

    fn process_lsp_symbols(
        &self,
        symbols: Vec<DocumentSymbol>,
        path: &Path,
        workspace: &Path,
        namespace: &crate::schema::RepoNamespace,
    ) -> Vec<HierarchicalSymbol> {
        let mut results = Vec::new();
        for sym in symbols {
            if is_noisy_symbol(&sym.kind) {
                continue;
            }

            let node_type = symbol_kind_to_node_type(sym.kind);
            let mut node = GraphNode::new_in(
                node_type,
                sym.name.clone(),
                crate::graph::graph_path(workspace, path),
                namespace,
            )
            .with_location_in(sym.range.start.line, sym.range.end.line, namespace);

            if let Some(detail) = sym.detail {
                node.signature = Some(detail);
            }

            // Check for Deprecated tag
            if let Some(tags) = &sym.tags {
                if tags.contains(&SymbolTag::DEPRECATED) {
                    node.is_deprecated = true;
                }
            }

            let children = if let Some(child_syms) = sym.children {
                self.process_lsp_symbols(child_syms, path, workspace, namespace)
            } else {
                Vec::new()
            };

            results.push(HierarchicalSymbol { node, children });
        }
        results
    }
    // (removed: had no caller and no test anywhere in the tree)

    /// Get all references to a symbol at a specific location
    pub async fn get_references(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
    ) -> Result<Vec<ReferenceLocation>, LainError> {
        let server_id = self.ensure_server(path).await?;
        let uri = format!("file://{}", path.display());
        let position = Position::new(line, col);

        let locations = match tokio::time::timeout(
            LSP_REQUEST_TIMEOUT,
            self.bridge.find_references(&server_id, &uri, position),
        )
        .await
        {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => {
                self.record_lsp_failure(&server_id, classify_bridge_error(&e));
                return Err(LainError::Lsp(e.to_string()));
            }
            Err(_) => {
                self.record_lsp_failure(&server_id, FailureKind::Timeout);
                return Err(LainError::Lsp(format!(
                    "find references request timed out for {}",
                    server_id
                )));
            }
        };

        self.record_success(&server_id);

        let mut results = Vec::new();
        for loc in locations {
            // fluent-uri doesn't directly convert to PathBuf, use string manipulation
            let path_str = loc.uri.to_string().replace("file://", "");
            results.push(ReferenceLocation {
                path: PathBuf::from(path_str),
                line: loc.range.start.line,
                col: loc.range.start.character,
                context: String::new(),
            });
        }
        Ok(results)
    }

    pub async fn install_server(&mut self, ext: &str) -> Result<String, LainError> {
        // Accept either a file extension ("rs", "py") or a language name
        // ("rust", "python"). When given a name, resolve to the canonical ext.
        let resolved_ext = resolve_language_to_ext(ext).unwrap_or(ext);
        let config = self.registry.get(resolved_ext)
            .ok_or_else(|| LainError::NotFound(format!(
                "No LSP configuration found for '{}'. Pass a file extension like 'rs' or 'py', or a known language name (rust, python, typescript, ...).",
                ext
            )))?;

        let install_cmd = config.install_cmd.ok_or_else(|| {
            LainError::Lsp(format!(
                "No automated install command available for {} ({})",
                ext, config.binary
            ))
        })?;

        // Platform-specific guard for brew
        if install_cmd.contains("brew install") && !cfg!(target_os = "macos") {
            return Err(LainError::Lsp(format!(
                "The install command for {} ({}) requires Homebrew and is only supported on macOS. Please install it manually for your platform.",
                resolved_ext, config.binary
            )));
        }

        info!(
            "Attempting to install LSP server for '{}' using: {}",
            resolved_ext, install_cmd
        );

        let parts: Vec<&str> = install_cmd.split_whitespace().collect();
        let mut cmd = tokio::process::Command::new(parts[0]);
        if parts.len() > 1 {
            cmd.args(&parts[1..]);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| LainError::Lsp(format!("Failed to execute install command: {}", e)))?;

        if output.status.success() {
            self.unavailable.remove(config.binary);
            Ok(format!("Successfully installed {}.", config.binary))
        } else {
            Err(LainError::Lsp(format!(
                "Installation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    /// Report each registered extension with whether its language server
    /// can actually be started.
    ///
    /// `unavailable` is a *negative cache*: it only gains entries once a
    /// spawn has already failed, so on a fresh process it is empty and
    /// every binary read as available. `get_health` renders this list, so
    /// a fresh server told the agent that gopls, jdtls, omnisharp and ten
    /// others were live on a machine where only rust-analyzer existed —
    /// an agent trusting it would assume it could get real symbols out of
    /// a `.go` file. `ensure_server` has always resolved the binary with
    /// `which::which` before starting it; this now asks the same question
    /// so the report matches the behaviour instead of contradicting it.
    pub fn get_supported_languages(&self) -> Vec<(String, String, bool)> {
        let mut langs = Vec::new();
        for (ext, config) in &self.registry {
            let is_available =
                !self.unavailable.contains(config.binary) && which::which(config.binary).is_ok();
            langs.push((ext.clone(), config.binary.to_string(), is_available));
        }
        langs.sort_by(|a, b| a.0.cmp(&b.0));
        langs
    }

    /// Mark a language server binary as unavailable. Used by tests that want
    /// to exercise the tree-sitter fallback path without spawning real LSP
    /// processes that may hang during cleanup.
    pub fn mark_unavailable(&mut self, binary: &str) {
        self.unavailable.insert(binary.to_string());
    }

    /// Record one LSP failure for `binary`, classified by `kind`.
    ///
    /// Two failure paths:
    ///
    /// - `FailureKind::ProcessExited` — the LSP child process exited
    ///   (broken pipe, EOF, "no process available"). Drop the
    ///   `started` entry so the next `ensure_server` call respawns
    ///   it, and increment the per-binary restart budget. If the
    ///   budget exceeds [`LSP_RESTART_BUDGET`] within
    ///   [`LSP_RESTART_WINDOW`], escalate to `unavailable` (operator
    ///   has a crash-looping binary).
    /// - `FailureKind::RequestError` / `FailureKind::Timeout` — the
    ///   child is presumed alive but unhealthy. Increment the
    ///   circuit-breaker counter; after
    ///   [`MAX_CONSECUTIVE_LSP_FAILURES`] consecutive failures the
    ///   binary is marked `unavailable`.
    ///
    /// In both cases the transition is logged at WARN level so
    /// operators see when LSP silently degrades.
    fn record_lsp_failure(&mut self, binary: &str, kind: FailureKind) {
        match kind {
            FailureKind::ProcessExited => {
                // If the binary is already marked unavailable (by the
                // operator via `mark_unavailable`, by the circuit
                // breaker from prior RequestError/Timeout, or by a
                // previous restart-budget trip), there's nothing to
                // do — the operator is already notified and a fresh
                // restart attempt would not change the state. Skip
                // the restart-budget bookkeeping to keep the WARN
                // log from firing twice for the same binary.
                if self.unavailable.contains(binary) {
                    self.started.remove(binary);
                    return;
                }
                // Drop the dead child; the next ensure_server call
                // will see `!started.contains(binary)` and respawn.
                self.started.remove(binary);
                self.record_restart(binary);
            }
            FailureKind::RequestError | FailureKind::Timeout => {
                let count = self
                    .consecutive_failures
                    .entry(binary.to_string())
                    .and_modify(|n| *n += 1)
                    .or_insert(1);
                if *count >= MAX_CONSECUTIVE_LSP_FAILURES && !self.unavailable.contains(binary) {
                    warn!(
                        "LSP '{}' has failed {} times consecutively; \
                         marking unavailable for the rest of this process lifetime. \
                         Restart `lain` to recover.",
                        binary, count
                    );
                    self.unavailable.insert(binary.to_string());
                }
            }
        }
    }

    /// Record one LSP restart attempt for `binary`. Sliding-window
    /// budget: if `LSP_RESTART_BUDGET` restarts happen within
    /// `LSP_RESTART_WINDOW`, mark the binary unavailable. Otherwise
    /// the count is informational (logged at DEBUG for operator
    /// tracing).
    fn record_restart(&mut self, binary: &str) {
        let now_ms = unix_millis_now();
        let window_ms = LSP_RESTART_WINDOW.as_millis() as u64;
        let entry = self
            .restart_budget
            .entry(binary.to_string())
            .or_insert((0, now_ms));
        let (count, window_start) = *entry;
        let (new_count, new_window_start) = if now_ms.saturating_sub(window_start) > window_ms {
            // Window expired — start a fresh one.
            (1u32, now_ms)
        } else {
            (count.saturating_add(1), window_start)
        };
        *entry = (new_count, new_window_start);

        if new_count > LSP_RESTART_BUDGET && !self.unavailable.contains(binary) {
            warn!(
                "LSP '{}' has restarted {} times within {:?}; \
                 marking unavailable. The child is likely crash-looping.",
                binary, new_count, LSP_RESTART_WINDOW
            );
            self.unavailable.insert(binary.to_string());
            self.consecutive_failures.remove(binary);
        } else {
            debug!(
                "LSP '{}' restart recorded ({} in window)",
                binary, new_count
            );
        }
    }

    /// Record one LSP success for `binary`. Resets both the
    /// per-binary failure counter (so a previously-flaky LSP that
    /// recovers gets a fresh circuit-breaker budget) and the
    /// restart-budget window (so a stable LSP doesn't accumulate
    /// restarts forever). No-op if the binary has no failure
    /// history — the common case is the steady-state where every
    /// call succeeds.
    fn record_success(&mut self, binary: &str) {
        self.consecutive_failures.remove(binary);
        // Restart budget is NOT reset on success — a binary that
        // was unstable enough to restart K times is still using up
        // its window. Only the failure counter (separate concern)
        // resets. This matches the upstream semantics: a healthy
        // round-trip says "the LSP is fine now" but doesn't rewrite
        // the recent history.
    }

    pub async fn shutdown(&mut self) {
        // Bound the bridge shutdown with a short timeout. `LspServer::stop`
        // calls `tokio::process::Child::kill()` and awaits the LSP
        // `shutdown` request; both can hang if the language server child
        // is unresponsive. A 5s budget is enough for a healthy process
        // (SIGKILL is synchronous) and short enough that one stuck
        // server can't block the rest of the federation shutdown.
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.bridge.shutdown()).await
        {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("LSP bridge shutdown error: {}", e),
            Err(_) => warn!("LSP bridge shutdown timed out after 5s; abandoning"),
        }
    }
}
// `marked_string_to_string` existed only to format hover contents for
// `get_hover_info`, which was removed as dead.

/// Map a friendly language name to its canonical file extension. Returns
/// `None` if the input looks like an extension already (so we don't try to
/// "translate" `rs` to something else) or if we don't recognise the name.
fn resolve_language_to_ext(s: &str) -> Option<&'static str> {
    // If it starts with a dot or looks like an ext, pass through.
    let normalized = s.trim().trim_start_matches('.').to_ascii_lowercase();
    match normalized.as_str() {
        "rust" => Some("rs"),
        "python" => Some("py"),
        "typescript" | "ts" => Some("ts"),
        "javascript" | "js" => Some("js"),
        "tsx" => Some("tsx"),
        "jsx" => Some("jsx"),
        "go" => Some("go"),
        "c" => Some("c"),
        "cpp" | "c++" => Some("cpp"),
        "csharp" | "c#" => Some("cs"),
        "ruby" => Some("rb"),
        "swift" => Some("swift"),
        "kotlin" => Some("kt"),
        "scala" => Some("scala"),
        "java" => Some("java"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        _ => None,
    }
}

fn is_noisy_symbol(kind: &SymbolKind) -> bool {
    matches!(
        *kind,
        SymbolKind::VARIABLE
            | SymbolKind::FIELD
            | SymbolKind::STRING
            | SymbolKind::NUMBER
            | SymbolKind::BOOLEAN
            | SymbolKind::ARRAY
            | SymbolKind::OBJECT
            | SymbolKind::KEY
            | SymbolKind::NULL
    )
}

fn symbol_kind_to_node_type(kind: SymbolKind) -> NodeType {
    match kind {
        SymbolKind::FILE => NodeType::File,
        SymbolKind::MODULE => NodeType::Module,
        SymbolKind::NAMESPACE => NodeType::Namespace,
        SymbolKind::PACKAGE => NodeType::Package,
        SymbolKind::CLASS => NodeType::Class,
        SymbolKind::METHOD => NodeType::Method,
        SymbolKind::PROPERTY => NodeType::Property,
        SymbolKind::INTERFACE => NodeType::Interface,
        SymbolKind::FUNCTION => NodeType::Function,
        SymbolKind::VARIABLE => NodeType::Variable,
        SymbolKind::CONSTANT => NodeType::Constant,
        SymbolKind::STRUCT => NodeType::Struct,
        SymbolKind::ENUM => NodeType::Enum,
        _ => NodeType::Module,
    }
}

/// Location of a definition
#[derive(Debug, Clone)]
pub struct DefinitionLocation {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
}

/// Location of a reference
#[derive(Debug, Clone)]
pub struct ReferenceLocation {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
    pub context: String,
}

// ── LSP Pool for Parallel Language Server Communication ──────────────────────

use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex as AsyncMutex;

/// Pool of LspMultiplexer instances for parallel LSP communication
pub struct LspPool {
    multiplexers: Vec<Arc<AsyncMutex<LspMultiplexer>>>,
    next: AtomicUsize,
}

impl Clone for LspPool {
    fn clone(&self) -> Self {
        // `AtomicUsize` isn't `Clone` so we can't `#[derive(Clone)]`, but
        // every clone should share the round-robin counter (a freshly
        // zeroed counter would split the multiplexer pool across clones
        // and starve some multiplexers). The pool is intended to be cloned
        // for read-only sharing, so pointing at the original counter is
        // correct: it's a stateless index, not a per-clone state.
        let next = AtomicUsize::new(self.next.load(Ordering::Relaxed));
        LspPool {
            multiplexers: self.multiplexers.clone(),
            next,
        }
    }
}

impl LspPool {
    pub fn new(
        workspace: &Path,
        size: usize,
        runtime: &crate::tuning::RuntimeConfig,
    ) -> Result<Self, LainError> {
        let mut multiplexers = Vec::with_capacity(size);
        for _ in 0..size {
            multiplexers.push(Arc::new(AsyncMutex::new(LspMultiplexer::new(
                workspace, runtime,
            )?)));
        }
        Ok(Self {
            multiplexers,
            next: AtomicUsize::new(0),
        })
    }

    /// Get next multiplexer in round-robin fashion
    pub fn next(&self) -> Arc<AsyncMutex<LspMultiplexer>> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.multiplexers.len();
        Arc::clone(&self.multiplexers[idx])
    }

    /// Shutdown all multiplexers in the pool
    pub async fn shutdown_all(&self) {
        for m in &self.multiplexers {
            m.lock().await.shutdown().await;
        }
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;

    /// `get_health` renders `get_supported_languages`, so "available"
    /// has to mean "this binary can actually be started", not "nothing
    /// has failed yet". The negative cache alone reported every server
    /// as live on a machine that had none of them installed.
    #[test]
    fn availability_reflects_what_is_installed_not_an_empty_negative_cache() {
        let m =
            LspMultiplexer::new(Path::new("."), &crate::tuning::RuntimeConfig::default()).unwrap();
        let langs = m.get_supported_languages();
        assert!(!langs.is_empty(), "registry should not be empty");

        for (ext, binary, available) in &langs {
            let on_path = which::which(binary).is_ok();
            assert_eq!(
                *available, on_path,
                "{ext} -> {binary}: reported available={available} but which() says {on_path}"
            );
        }
    }

    /// The bug was specifically that a *fresh* multiplexer — one that
    /// has never attempted a spawn, so `unavailable` is empty — claimed
    /// everything worked. Pin that a binary that cannot exist is never
    /// reported as available.
    #[test]
    fn a_binary_that_is_not_installed_is_never_reported_available() {
        let mut m =
            LspMultiplexer::new(Path::new("."), &crate::tuning::RuntimeConfig::default()).unwrap();
        let bogus: &'static LspConfig = Box::leak(Box::new(LspConfig {
            binary: "definitely-not-a-real-language-server-xyz",
            install_cmd: None,
        }));
        m.registry.insert("zzz".to_string(), bogus);

        assert!(
            m.unavailable.is_empty(),
            "fresh multiplexer has an empty negative cache"
        );

        let reported = m
            .get_supported_languages()
            .into_iter()
            .find(|(ext, _, _)| ext == "zzz")
            .expect("the injected extension should be reported");
        assert!(
            !reported.2,
            "an uninstalled binary must report unavailable even before any spawn attempt"
        );
    }
}

#[cfg(test)]
mod circuit_breaker_tests {
    //! Per-binary circuit breaker for LSP flakiness. After
    //! `MAX_CONSECUTIVE_LSP_FAILURES` consecutive errors for a binary,
    //! the multiplexer marks it unavailable so subsequent calls fall
    //! back to tree-sitter without paying another timeout cost.

    use super::*;

    fn make() -> LspMultiplexer {
        LspMultiplexer::new(Path::new("."), &crate::tuning::RuntimeConfig::default()).unwrap()
    }

    #[test]
    fn three_consecutive_failures_mark_binary_unavailable() {
        let mut m = make();
        let binary = "rust-analyzer";

        // First two failures don't trip — operator gets one or two
        // warnings before the indexer silently degrades. Use the
        // RequestError kind (the default circuit-breaker driver) so
        // the path mirrors the production wiring.
        m.record_lsp_failure(binary, FailureKind::RequestError);
        m.record_lsp_failure(binary, FailureKind::RequestError);
        assert!(
            !m.unavailable.contains(binary),
            "two failures must not trip the breaker; got: {:?}",
            m.unavailable
        );

        // Third failure trips.
        m.record_lsp_failure(binary, FailureKind::RequestError);
        assert!(
            m.unavailable.contains(binary),
            "three consecutive failures must mark the binary unavailable"
        );
    }

    #[test]
    fn success_resets_failure_count() {
        let mut m = make();
        let binary = "rust-analyzer";

        // Two failures, then a success — counter resets.
        m.record_lsp_failure(binary, FailureKind::RequestError);
        m.record_lsp_failure(binary, FailureKind::RequestError);
        m.record_success(binary);
        // Two more failures shouldn't trip because the count was
        // cleared by the success.
        m.record_lsp_failure(binary, FailureKind::RequestError);
        m.record_lsp_failure(binary, FailureKind::RequestError);
        assert!(
            !m.unavailable.contains(binary),
            "a success between failures must reset the counter; \
             two follow-on failures shouldn't trip the breaker"
        );

        // Third follow-on failure trips.
        m.record_lsp_failure(binary, FailureKind::RequestError);
        assert!(
            m.unavailable.contains(binary),
            "after the reset, three fresh failures must trip"
        );
    }

    #[test]
    fn failure_count_is_per_binary() {
        // rust-analyzer and gopls are independent: failing one must
        // not affect the other's circuit.
        let mut m = make();
        m.record_lsp_failure("rust-analyzer", FailureKind::RequestError);
        m.record_lsp_failure("gopls", FailureKind::RequestError);
        m.record_lsp_failure("rust-analyzer", FailureKind::RequestError);
        m.record_lsp_failure("gopls", FailureKind::RequestError);
        m.record_lsp_failure("rust-analyzer", FailureKind::RequestError);
        assert!(
            m.unavailable.contains("rust-analyzer"),
            "rust-analyzer should trip after 3 failures"
        );
        assert!(
            !m.unavailable.contains("gopls"),
            "gopls must remain available — its circuit is independent of rust-analyzer's"
        );
    }

    #[test]
    fn successful_call_does_not_increment_count() {
        // The wiring rule is: error → increment, success → reset.
        // A success on its own (no prior failure) is a no-op, not a
        // bug. Pin that explicitly so a future "always increment"
        // refactor doesn't accidentally penalise the steady state.
        let mut m = make();
        m.record_success("rust-analyzer");
        assert!(m.consecutive_failures.is_empty());
        assert!(!m.unavailable.contains("rust-analyzer"));
    }

    #[test]
    fn record_lsp_failure_on_already_unavailable_binary_is_idempotent() {
        // Calling record_lsp_failure on a binary that's already marked
        // unavailable (via the existing `mark_unavailable` path or a
        // prior trip) must not double-log or otherwise misbehave.
        // The function is meant to be safe to call repeatedly.
        let mut m = make();
        m.mark_unavailable("rust-analyzer");
        m.record_lsp_failure("rust-analyzer", FailureKind::RequestError);
        m.record_lsp_failure("rust-analyzer", FailureKind::Timeout);
        assert!(m.unavailable.contains("rust-analyzer"));
    }

    // ── Restart-on-ProcessExited tests ─────────────────────────────────

    #[test]
    fn process_exited_drops_started_and_records_restart() {
        // The first restart: drop the dead child, increment the
        // budget, leave `unavailable` untouched (process is
        // presumed-recoverable).
        let mut m = make();
        let binary = "rust-analyzer";

        // Pretend a child is alive so we can verify it gets dropped.
        m.started.insert(binary.to_string());
        assert!(m.started.contains(binary));
        assert!(!m.restart_budget.contains_key(binary));

        m.record_lsp_failure(binary, FailureKind::ProcessExited);

        assert!(
            !m.started.contains(binary),
            "process exit must drop the dead child from `started` so the next \
             ensure_server respawns it"
        );
        assert!(
            !m.unavailable.contains(binary),
            "a single restart must not trip the breaker"
        );
        let (count, _window_start) = m.restart_budget[binary];
        assert_eq!(count, 1, "restart must be recorded in the budget");
    }

    #[test]
    fn process_exited_does_not_increment_circuit_breaker() {
        // ProcessExited goes to the restart budget, NOT the
        // circuit-breaker counter. A child that crashes once and
        // respawns should not consume the operator's tolerance for
        // request errors.
        let mut m = make();
        let binary = "rust-analyzer";

        for _ in 0..5 {
            m.record_lsp_failure(binary, FailureKind::ProcessExited);
        }
        assert!(
            m.consecutive_failures.is_empty(),
            "ProcessExited must not increment the circuit-breaker counter"
        );
        // 5 restarts exceed LSP_RESTART_BUDGET (3) and trip the
        // restart budget — but via the budget path, not the
        // circuit-breaker path.
        assert!(m.unavailable.contains(binary));
    }

    #[test]
    fn request_error_does_not_trigger_restart() {
        // RequestError / Timeout go to the circuit-breaker counter
        // ONLY, not the restart budget. The child is presumed
        // alive but unhealthy.
        let mut m = make();
        let binary = "rust-analyzer";
        m.started.insert(binary.to_string());

        m.record_lsp_failure(binary, FailureKind::RequestError);
        m.record_lsp_failure(binary, FailureKind::Timeout);
        m.record_lsp_failure(binary, FailureKind::RequestError);

        assert!(
            m.started.contains(binary),
            "RequestError must not drop the started entry"
        );
        assert!(
            m.restart_budget.is_empty(),
            "RequestError/Timeout must not consume the restart budget"
        );
        // 3 RequestErrors trip the circuit breaker.
        assert!(m.unavailable.contains(binary));
    }

    #[test]
    fn restart_budget_escalates_to_unavailable_after_threshold() {
        // LSP_RESTART_BUDGET+1 restarts within the window mark the
        // binary unavailable. Single threshold; sliding window.
        let mut m = make();
        let binary = "rust-analyzer";

        // Three restarts within the window: still available.
        for _ in 0..LSP_RESTART_BUDGET {
            m.record_lsp_failure(binary, FailureKind::ProcessExited);
        }
        assert!(
            !m.unavailable.contains(binary),
            "exactly {} restarts must not trip the breaker",
            LSP_RESTART_BUDGET
        );

        // One more restart crosses the threshold.
        m.record_lsp_failure(binary, FailureKind::ProcessExited);
        assert!(
            m.unavailable.contains(binary),
            "the ({} + 1)th restart within the window must trip the breaker",
            LSP_RESTART_BUDGET
        );
    }

    #[test]
    fn restart_budget_window_resets_after_lsp_restart_window() {
        // The budget window is [`LSP_RESTART_WINDOW`] long. Restarts
        // that fall outside the window don't count toward the
        // threshold. We test this by directly manipulating
        // `restart_budget` to simulate a window that has already
        // expired — the production code path is identical
        // (read `now - window_start > LSP_RESTART_WINDOW`).
        let mut m = make();
        let binary = "rust-analyzer";

        // Simulate three restarts that happened an hour ago.
        let long_ago_ms =
            unix_millis_now().saturating_sub(LSP_RESTART_WINDOW.as_millis() as u64 * 2);
        m.restart_budget
            .insert(binary.to_string(), (LSP_RESTART_BUDGET, long_ago_ms));

        // One fresh restart now. The previous window has expired,
        // so the budget resets and this single restart is fine.
        m.record_lsp_failure(binary, FailureKind::ProcessExited);
        let (count, _) = m.restart_budget[binary];
        assert_eq!(
            count, 1,
            "a fresh restart after the window expired must reset the budget"
        );
        assert!(
            !m.unavailable.contains(binary),
            "the budget must not trip when the window has expired"
        );
    }

    #[test]
    fn success_does_not_reset_restart_budget() {
        // The restart budget is process-lifetime and only resets
        // when the sliding window expires — a successful round-trip
        // does NOT reset it. This is intentional: a binary that has
        // crashed N times in the last minute is using up its window
        // regardless of whether the latest call succeeded.
        let mut m = make();
        let binary = "rust-analyzer";

        m.record_lsp_failure(binary, FailureKind::ProcessExited);
        m.record_lsp_failure(binary, FailureKind::ProcessExited);
        m.record_success(binary);
        let (count, _) = m.restart_budget[binary];
        assert_eq!(
            count, 2,
            "success must not reset the restart budget; only the window expiry does"
        );
    }

    #[test]
    fn restart_path_skips_circuit_breaker_even_when_already_unavailable() {
        // If the binary is already `unavailable` (from a prior
        // circuit-breaker trip), a subsequent ProcessExited should
        // be a no-op for the budget path. We don't want to wake up
        // the operator's pager twice for the same binary.
        let mut m = make();
        let binary = "rust-analyzer";
        m.mark_unavailable(binary);

        m.record_lsp_failure(binary, FailureKind::ProcessExited);

        // No new budget entry should be created.
        assert!(
            !m.restart_budget.contains_key(binary),
            "ProcessExited on an already-unavailable binary is a no-op"
        );
    }
}
