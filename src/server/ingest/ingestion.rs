use super::blocking::offthread;
use super::scan::{scan_file_batch, PatternRef, StaticFileRef};
use super::LainServer;
use crate::error::LainError;
use crate::git::AnyGitSensor;
use crate::graph::{graph_path, GraphDatabase};
use crate::lsp::LspPool;
use crate::schema::{GraphEdge, GraphNode};
use crate::server::overlay::{OverlayDiff, VolatileOverlay};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

impl LainServer {
    /// The "Sane" Ingestion Pipeline: Map -> Reduce -> Resolve -> Enrich.
    /// `&self` (not `&mut self`) because every field this method writes
    /// to is `Arc`-shared (graph, presence, occupancy, broadcast) or
    /// `Arc<Mutex<…>>` (git, lsp) — so calling it on a clone of the
    /// `LainServer` is safe and the writes are visible to the original
    /// `Arc<LainServer>` the MCP layer holds.
    /// [`Self::build_core_memory`], repeated while each pass is partial
    /// but leaves fewer files than the one before — a repository over
    /// `max_files_per_scan` takes several passes. Stops at the first pass
    /// that makes no progress (a scan timeout on the same files), which
    /// is reported as that pass's error.
    pub async fn build_core_memory_until_complete(&self) -> Result<(), LainError> {
        let mut left_before = usize::MAX;
        loop {
            match self.build_core_memory().await {
                Err(e) => match partial_files_left(&e) {
                    Some(left) if left < left_before => left_before = left,
                    _ => return Err(e),
                },
                ok => return ok,
            }
        }
    }

    pub async fn build_core_memory(&self) -> Result<(), LainError> {
        // Defensive gate: sidecar processes should never call build_core_memory,
        // but if a future refactor routes them here, bail out cleanly instead
        // of corrupting the shared on-disk graph.
        if self.ingest().graph().is_read_only() {
            return Ok(());
        }
        // AGENT_UX_ROADMAP.md M4 follow-up (FOLLOWUPS.md): every long-
        // running phase observes the server-owned cancellation token.
        // A `Drop` on `LainServer` cancels it (via `LifecycleInfo`'s
        // `Drop` impl), so a shutdown during a cold-boot re-index
        // returns control promptly instead of running to completion.
        let cancel = self.lifecycle_handle().cancel_token();
        if cancel.is_cancelled() {
            info!("build_core_memory: cancelled before discovering commit");
            return Err(LainError::Cancelled);
        }
        let scan_start = std::time::Instant::now();
        // AGENT_UX_ROADMAP.md M4 follow-up: the server-owned
        // cancellation token observes every phase boundary, including
        // the LSP subprocess awaits inside `scan_file_structure`.
        let cancel = self.lifecycle_handle().cancel_token();
        self.readiness().update(|snapshot| {
            snapshot.phase = crate::server::readiness::IndexPhase::Discovering;
        });
        // AGENT_UX_ROADMAP.md M4 follow-up: route the libgit2 commit
        // lookup through `offthread`. Pre-fix this ran on the Tokio
        // worker; a slow git operation (e.g. a packed-refs refresh
        // on a huge monorepo) would block the worker holding the
        // `AsyncMutex<GitSensor>` for as long as it took. The
        // `Arc<AnyGitSensor>` is cloned and the call is dispatched
        // *inside* the closure so no lock or IPC crosses the await boundary.
        // In InProcess mode, try_lock fails fast if another thread is holding
        // the lock (Bug #2 mitigation). In Sidecar mode, IPC dispatches directly.
        let git_sensor = Arc::clone(self.ingest().git());
        let (latest_commit, latest_time) = offthread(
            cancel.clone(),
            move || -> Result<(String, i64), LainError> { git_sensor.try_get_latest_commit_info() },
        )
        .await?;
        let mut last_commit = self.ingest().graph().get_last_commit()?;
        // A graph persisted by a Lain that minted ids under a per-process
        // random namespace matches nothing this process derives; the
        // "already up to date" shortcut below would keep it forever.
        if last_commit.is_some() && self.ingest().graph().minted_in_other_namespace() {
            info!("Persisted graph was built under a different id namespace; rebuilding it");
            self.ingest().graph().reset()?;
            last_commit = None;
        }
        self.readiness().update(|snapshot| {
            snapshot.target_commit = Some(latest_commit.clone());
        });
        if cancel.is_cancelled() {
            info!("build_core_memory: cancelled after discovering commit");
            return Err(LainError::Cancelled);
        }

        if let Some(ref last) = last_commit {
            if last == &latest_commit {
                info!("Core memory is already up to date with commit {}", last);
                return Ok(());
            }
        }

        info!("Building core topology for commit {}", latest_commit);
        // A real re-index (not the no-op "already up to date" path just
        // above, which never reaches here): if a prior pass already
        // published `ready`, this must move the gate back to
        // `warming_up` for the duration of this pass, or graph-required
        // tools keep dispatching against a graph this pass is actively
        // mutating.
        self.readiness().resume_warming_up();

        // 1. Parallel Map Phase: Scan files for structure and external references
        let files = if let Some(ref last) = last_commit {
            info!("Incremental update since {}", last);
            let last = last.clone();
            let git_sensor = Arc::clone(self.ingest().git());
            offthread(
                cancel.clone(),
                move || -> Result<Vec<std::path::PathBuf>, LainError> {
                    git_sensor.try_get_changed_files_since(&last)
                },
            )
            .await?
        } else {
            info!("Full repository scan");
            let git_sensor = Arc::clone(self.ingest().git());
            offthread(
                cancel.clone(),
                move || -> Result<Vec<std::path::PathBuf>, LainError> {
                    git_sensor.try_get_all_tracked_files()
                },
            )
            .await?
        };

        if files.is_empty() {
            // Same trap as the federation pipeline: a deletion-only
            // commit produces an empty scan list, and returning here
            // without sweeping strands the deleted file's nodes while
            // advancing the marker past the commit that removed them.
            info!("No files to scan; sweeping orphans.");
            sweep_orphans(
                &self.ingest().config().workspace,
                self.ingest().graph(),
                self.ingest().git(),
            );
            if cancel.is_cancelled() {
                info!("build_core_memory: cancelled after orphan sweep");
                return Err(LainError::Cancelled);
            }
            self.ingest().graph().set_last_commit(latest_commit)?;
            // Persisting is for the next start; this process serves from memory.
            // An unwritable `.lain` failed the whole pass, so a complete
            // in-memory index was reported unavailable and redone every tick.
            if let Err(e) = self.ingest().graph().save_to_disk().await {
                warn!("could not persist the graph ({e}); serving it from memory only");
            }
            return Ok(());
        }

        let lsp_sync_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Batch files into chunks to reduce task spawning overhead
        let files_per_batch = self.ingest().tuning().ingestion.files_per_batch;
        let max_files = self.ingest().tuning().ingestion.max_files_per_scan;
        // A capped pass used to take the same first `max_files` of the list
        // every time, so a repository over the cap never converged: every
        // pass was partial, the marker never advanced, and the index was
        // never ready. Now:
        // - files a previous pass already scanned for this same target
        //   commit (their File node carries it) are skipped, so successive
        //   passes cover the rest and the last one completes;
        // - only parseable source counts toward the cap — a `.json` costs a
        //   File node, and 5,000 of them kept a one-source-file repo from
        //   ever finishing.
        let workspace_root = self.ingest().config().workspace.clone();
        let done_for_target = |f: &PathBuf| {
            self.ingest()
                .graph()
                .get_file_node(&crate::graph::graph_path(&workspace_root, f))
                .is_some_and(|n| n.commit_hash.as_deref() == Some(latest_commit.as_str()))
        };
        let remaining: Vec<PathBuf> = files
            .iter()
            .filter(|f| !done_for_target(f))
            .cloned()
            .collect();
        let resumed = remaining.len() < files.len();
        let is_source = |f: &PathBuf| {
            f.extension()
                .and_then(|e| e.to_str())
                .is_some_and(crate::treesitter::is_indexed_extension)
        };
        let mut source_taken = 0usize;
        let files_to_scan: Vec<_> = remaining
            .iter()
            .filter(|f| {
                if !is_source(f) {
                    return true;
                }
                source_taken += 1;
                source_taken <= max_files
            })
            .cloned()
            .collect();
        let file_chunks: Vec<Vec<PathBuf>> = files_to_scan
            .chunks(files_per_batch)
            .map(|chunk| chunk.to_vec())
            .collect();

        // LSP cold-boot prewarm: fire one `documentSymbol` request per
        // language actually present in the scan set before we hand
        // off to the scan batch. A blocked, in-process cold cache
        // (rust-analyzer loading crates.io indices, clangd parsing
        // templated headers) routinely takes 2–5 s on the first call
        // — without this, the very first scan chunk would either
        // time out the first real `documentSymbol` against the 1 s
        // production boundary (PR #112) or quietly fall back to
        // tree-sitter and miss macro-resolved symbols.
        //
        // Honors:
        //   * `lsp_prewarm_opt_out` — operator-supplied escape hatch.
        //   * the server cancel token — shutdown during prewarm
        //     resets the readiness phase before returning, so the
        //     agent-visible `get_capabilities.phase` doesn't get
        //     stuck on `prewarming_lsp`.
        //   * `lsp_prewarm_max_files` — sentinel scan is bounded.
        //
        // Parallelism: one task per language via `JoinSet` so a 20-
        // language monorepo doesn't spend 20 × LSP_PREWARM_TIMEOUT
        // serially. Multiplexer contention falls out naturally — two
        // languages that route to the same `LspMultiplexer`
        // serialise on its inner `AsyncMutex`, which is fine: the
        // fast path (multiplexer already running) completes in
        // milliseconds and the slow path is gated by
        // `lsp_prewarm_timeout_secs` per task.
        //
        // `prewarm_server` uses `LSP_PREWARM_TIMEOUT` (30 s default)
        // and never touches the runtime circuit breaker, so even a
        // stuck cold LSP cannot promote itself to `unavailable` from
        // this path.
        let lsp_prewarm_opt_out = self.ingest().tuning().ingestion.lsp_prewarm_opt_out;
        if !lsp_prewarm_opt_out && !files_to_scan.is_empty() {
            self.readiness().update(|snapshot| {
                snapshot.phase = crate::server::readiness::IndexPhase::PrewarmingLsp;
                snapshot.files_total = Some(files_to_scan.len() as u64);
            });
            let skip_extensions: std::collections::HashSet<String> = self
                .ingest()
                .tuning()
                .ingestion
                .lsp_prewarm_skip_extensions
                .iter()
                .cloned()
                .collect();
            let unique_exts: Vec<String> = files_to_scan
                .iter()
                .filter_map(|p| p.extension().and_then(|e| e.to_str()))
                .map(|e| e.to_string())
                // Per-language opt-out: an extension listed in
                // `lsp_prewarm_skip_extensions` is filtered out of
                // the prewarm pass. The skip is logged so operators
                // can audit the configuration at startup; an
                // unconfigured `tuning.toml` filters nothing.
                .filter(|e| {
                    if skip_extensions.contains(e) {
                        debug!(
                            "build_core_memory: skipping LSP prewarm for extension {:?} \
                             (lsp_prewarm_skip_extensions)",
                            e
                        );
                        false
                    } else {
                        true
                    }
                })
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            let prewarm_max = self.ingest().tuning().ingestion.lsp_prewarm_max_files;
            let prewarm_timeout_secs = self.ingest().tuning().ingestion.lsp_prewarm_timeout_secs;
            info!(
                "build_core_memory: LSP prewarm starting for {} files ({} languages after skip-list, timeout {}s each, parallel)",
                files_to_scan.len(),
                unique_exts.len(),
                prewarm_timeout_secs
            );

            let mut prewarm_set = tokio::task::JoinSet::new();
            // Take a pool snapshot once at the boundary; each task
            // reaches for `lsp_pool().next()` on its own and routes
            // the result through `Arc<AsyncMutex<LspMultiplexer>>` —
            // because the multiplexer is keyed by binary rather
            // than by language, two tasks whose languages share a
            // binary will serialise on the same mutex. That's the
            // happy path for "rust + rust in two crates" and not
            // worth special-casing for now.
            let pool = Arc::clone(self.ingest().lsp_pool());
            let unique_exts_for_summary = unique_exts.clone();
            for ext in unique_exts.into_iter() {
                if cancel.is_cancelled() {
                    debug!("build_core_memory: cancel observed before prewarm task for {ext}");
                    break;
                }
                let sentinel =
                    crate::server::lsp::pick_prewarm_sentinel(&ext, &files_to_scan, prewarm_max);
                let cancel_for_task = cancel.clone();
                let pool_for_task = Arc::clone(&pool);
                prewarm_set.spawn(async move {
                    let mplex = pool_for_task.next();
                    let mut lsp = mplex.lock().await;
                    let timeout = std::time::Duration::from_secs(prewarm_timeout_secs);
                    lsp.prewarm_server(&ext, sentinel.as_deref(), Some(timeout))
                        .await;
                    if cancel_for_task.is_cancelled() {
                        tracing::debug!("LSP prewarm task for {ext} observed cancel");
                    }
                });
            }
            // Drain.
            while let Some(res) = prewarm_set.join_next().await {
                if cancel.is_cancelled() {
                    // Re-check after each task. If we observe cancel
                    // mid-drain, abort the rest so a 20-language repo
                    // doesn't hold us for 30s × remaining.
                    prewarm_set.abort_all();
                    // Reset readiness to the pre-prewarm phase
                    // (Discovering) before returning. Without this
                    // the readiness snapshot stays pinned at
                    // PrewarmingLsp and the operator-visible
                    // `get_capabilities.phase` looks stuck.
                    self.readiness().update(|snapshot| {
                        snapshot.phase = crate::server::readiness::IndexPhase::Discovering;
                    });
                    debug!("build_core_memory: cancelled during LSP prewarm drain");
                    return Err(LainError::Cancelled);
                }
                // `res` is `Result<(), JoinError>`. A JoinError means
                // the task panicked or was aborted. Aborting is a
                // cooperative signal; panicking would indicate a bug
                // somewhere in `prewarm_server`.
                if let Err(e) = res {
                    if e.is_panic() {
                        tracing::warn!("LSP prewarm task panicked: {e}");
                    }
                }
            }
            if cancel.is_cancelled() {
                self.readiness().update(|snapshot| {
                    snapshot.phase = crate::server::readiness::IndexPhase::Discovering;
                });
                return Err(LainError::Cancelled);
            }
            // Operator-facing summary. With `unique_exts.len() <=
            // pool.multiplexers.len()` every language runs in
            // parallel; larger language sets round-robin through
            // the pool, so languages routed to the same multiplexer
            // serialise on its inner `AsyncMutex`. The `max_per_mux`
            // value is what an operator would need to bump
            // `lsp_pool_size` past in `.lain/tuning.toml` to get
            // full parallelism.
            let pool_size = pool.size();
            let n_exts = unique_exts_for_summary.len();
            let max_per_mux = if pool_size == 0 {
                0
            } else {
                n_exts.div_ceil(pool_size)
            };
            if max_per_mux > 1 {
                info!(
                    "build_core_memory: LSP prewarm done — {} languages across {} multiplexers \
                     (~{} tasks per mux serialised on the inner mutex; bumping lsp_pool_size \
                     past {} in .lain/tuning.toml would give every language its own parallel slot)",
                    n_exts, pool_size, max_per_mux, n_exts,
                );
            } else {
                info!(
                    "build_core_memory: LSP prewarm done — {} languages across {} multiplexers (full parallelism)",
                    n_exts,
                    pool_size,
                );
            }
        }

        // `files_total` is fixed for this attempt the moment the scan is
        // planned; it does not shrink or grow even if the scan later times
        // out or aborts early — that partial-ness shows up as
        // `files_completed` staying below it, not as the total moving.
        self.readiness().update(|snapshot| {
            snapshot.phase = crate::server::readiness::IndexPhase::Scanning;
            snapshot.files_total = Some(files_to_scan.len() as u64);
            snapshot.files_completed = 0;
            snapshot.files_failed = 0;
        });

        let mut set = tokio::task::JoinSet::new();
        for chunk in file_chunks {
            // AGENT_UX_ROADMAP.md M4 follow-up: cancel between batches.
            // Spawning new work after a cancel would keep the runtime
            // busy past the shutdown budget.
            if cancel.is_cancelled() {
                info!("build_core_memory: cancelled before scan batch");
                return Err(LainError::Cancelled);
            }
            let lsp = self.ingest().lsp_pool().next();
            let workspace = self.ingest().config().workspace.clone();
            let commit_hash = latest_commit.clone();
            let git_time = latest_time;
            // `RepoNamespace` is `Copy`; capturing by value gives the
            // spawned task an owned `RepoNamespace` to borrow from, instead
            // of borrowing `&self.ingest().id_namespace()` which would dangle past
            // `self`'s lifetime.
            let namespace = *self.ingest().id_namespace();
            let cancel_for_spawn = cancel.clone();

            set.spawn(async move {
                scan_file_batch(
                    chunk,
                    workspace,
                    lsp,
                    lsp_sync_time,
                    git_time,
                    commit_hash,
                    &namespace,
                    cancel_for_spawn,
                )
                .await
            });
        }

        // 2. Reduce Phase: Incremental flush — write partial results as tasks complete
        let mut batch_nodes = Vec::new();
        let mut batch_edges = Vec::new();
        let mut all_external_refs = Vec::new();
        let mut all_static_refs: Vec<StaticFileRef> = Vec::new();
        let mut all_pattern_refs: Vec<PatternRef> = Vec::new();
        let batch_size = self.ingest().tuning().ingestion.ingest_batch_size;

        let mut scanned = 0usize;
        let mut failed = 0usize;
        // True when this pass did NOT cover every changed file: either the
        // scan-phase timeout aborted the remaining tasks, or `max_files_per_scan`
        // capped the input. A partial pass must persist whatever it produced but
        // must NOT advance `set_last_commit` — otherwise the graph claims to be
        // current at HEAD while missing files, which is worse than being visibly
        // behind. See the guarded `set_last_commit` at the end of this function.
        let mut partial = remaining.len() > files_to_scan.len();
        if partial {
            warn!(
                "Scan capped at max_files_per_scan={} of {} changed files;                  this pass is partial and will not advance the indexed-commit marker",
                files_to_scan.len(),
                files.len()
            );
        }
        let scan_timeout =
            std::time::Duration::from_secs(self.ingest().tuning().ingestion.scan_timeout_secs);

        while let Some(res) = set.join_next().await {
            // AGENT_UX_ROADMAP.md M4 follow-up: cancel observed at every
            // completed batch boundary. If we observe it after a batch
            // finishes, abort the rest and bail out before any further
            // persistence work runs.
            if cancel.is_cancelled() {
                info!("build_core_memory: cancelled mid-scan");
                set.abort_all();
                return Err(LainError::Cancelled);
            }
            // Check timeout - abort remaining tasks and break
            if scan_start.elapsed() >= scan_timeout {
                warn!(
                    "Scan phase timed out after {:?}, aborting {} remaining tasks",
                    scan_timeout,
                    set.len()
                );
                partial = true;
                set.abort_all();
                break;
            }
            match res {
                Ok(batch_results) => {
                    // Process each file result in this batch
                    for file_result in batch_results {
                        match file_result {
                            Ok(scan_result) => {
                                scanned += 1;
                                batch_nodes.extend(scan_result.nodes);
                                batch_edges.extend(scan_result.edges);
                                all_external_refs.extend(scan_result.external_references);
                                all_static_refs.extend(scan_result.static_refs);
                                all_pattern_refs.extend(scan_result.pattern_refs);
                            }
                            Err(e) => {
                                failed += 1;
                                warn!("File scan error: {}", e);
                            }
                        }
                    }
                    debug!(
                        "Batch completed: {} files scanned, {} failed in batch",
                        scanned, failed
                    );

                    // Incremental flush every batch_size files
                    if batch_nodes.len() >= batch_size {
                        info!(
                            "Flush phase 1: writing {} nodes ({} files scanned)",
                            batch_nodes.len(),
                            scanned
                        );
                        // Replace rather than insert, so a re-scan drops the
                        // symbols a file no longer defines instead of layering
                        // new nodes on top of stale ones. Scan results arrive
                        // whole-file and this flush runs between chunks, so
                        // every path here has all of its nodes in this batch.
                        // One call per flush, not per file: per-file would take
                        // a write lock hundreds of times while readers query.
                        let paths: Vec<String> = batch_nodes
                            .iter()
                            .map(|n| n.path.clone())
                            .collect::<HashSet<_>>()
                            .into_iter()
                            .collect();
                        if let Err(e) = self
                            .ingest()
                            .graph()
                            .replace_nodes_for_paths(&paths, &batch_nodes)
                        {
                            warn!("Batch node write error: {}", e);
                        }
                        insert_edges_best_effort(self.ingest().graph(), &batch_edges, "batch");
                        // Durably persist the flush. The in-memory inserts above
                        // are lost if the outer re-index timeout drops this task,
                        // which is why a 90s budget could never converge: every
                        // batch of parsing was discarded. Writing here means a
                        // killed run still leaves progress on disk and successive
                        // runs converge instead of restarting from the same graph.
                        if let Err(e) = self.ingest().graph().save_to_disk().await {
                            warn!("Batch persist error: {}", e);
                        }
                        batch_nodes.clear();
                        batch_edges.clear();
                    }
                }
                Err(e) => {
                    failed += 1;
                    warn!("Task join error: {}", e);
                }
            }
            // Published once per completed batch. `files_completed` counts
            // every attempt (success and failure), matching the wire
            // contract's "never decreases, always >= files_failed".
            self.readiness().update(|snapshot| {
                snapshot.files_completed = (scanned + failed) as u64;
                snapshot.files_failed = failed as u64;
            });
        }

        // Final partial flush
        if !batch_nodes.is_empty() {
            info!("Flush phase 1 (final): writing {} nodes", batch_nodes.len());
            let paths: Vec<String> = batch_nodes
                .iter()
                .map(|n| n.path.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            if let Err(e) = self
                .ingest()
                .graph()
                .replace_nodes_for_paths(&paths, &batch_nodes)
            {
                warn!("Final batch node write error: {}", e);
            }
            insert_edges_best_effort(self.ingest().graph(), &batch_edges, "final batch");
        }

        info!("Scanned {} files, {} failed, collected {} external refs, {} static refs, {} pattern refs",
              scanned, failed, all_external_refs.len(), all_static_refs.len(), all_pattern_refs.len());

        // 3. Resolve Phase: Link external references to internal nodes (CALLS/USES)
        self.readiness().update(|snapshot| {
            snapshot.phase = crate::server::readiness::IndexPhase::Resolving;
        });
        if cancel.is_cancelled() {
            info!("build_core_memory: cancelled before resolve");
            return Err(LainError::Cancelled);
        }
        info!(
            "Resolving topology: Linking {} external references...",
            all_external_refs.len()
        );
        let call_edges = super::resolve::resolve_call_edges(
            self.ingest().graph(),
            &self.ingest().config().workspace,
            &all_external_refs,
            None,
            None,
        );
        info!("Ingesting {} call edges", call_edges.len());
        insert_edges_reporting(self.ingest().graph(), &call_edges, "call")?;

        // 3b. Static Resolve Phase: tree-sitter derived Calls/Uses edges
        if cancel.is_cancelled() {
            info!("build_core_memory: cancelled after call resolve");
            return Err(LainError::Cancelled);
        }
        info!(
            "Resolving {} tree-sitter static references...",
            all_static_refs.len()
        );
        // A pass resuming an earlier capped one is incremental too: refs
        // from files scanned before into the files scanned now must resolve.
        if last_commit.is_some() || resumed {
            let git_sensor = Arc::clone(self.ingest().git());
            let tracked_result = offthread(cancel.clone(), move || {
                git_sensor.try_get_all_tracked_files()
            })
            .await;
            // Drop deleted and moved-away paths *before* resolving. After a
            // rename the old path's definition still sat beside the new
            // one, so every call to it resolved to the stale node (or was
            // dropped as ambiguous) and the later sweep deleted it with its
            // edges: `git mv a.py lib.py` left lib.py's functions with no
            // callers until a full re-index. Only with a complete listing —
            // an empty one would read as "everything was deleted".
            if let Ok(tracked_paths) = &tracked_result {
                let tracked_keys: HashSet<String> = tracked_paths
                    .iter()
                    .map(|p| crate::graph::graph_path(&self.ingest().config().workspace, p))
                    .collect();
                if let Err(e) = self.ingest().graph().prune_orphans(&tracked_keys) {
                    warn!("Orphan sweep before resolve failed: {e}");
                }
            }
            let tracked = tracked_result.unwrap_or_default();
            let extra = refs_into_rescanned(
                &self.ingest().config().workspace,
                self.ingest().graph(),
                &files_to_scan,
                &tracked,
            );
            info!(
                "Re-resolving {} references into the rescanned files",
                extra.len()
            );
            all_static_refs.extend(extra);
        }
        let static_edges = super::resolve::resolve_static_edges(
            self.ingest().graph(),
            &all_static_refs,
            None,
            None,
        );
        info!("Ingesting {} static tree-sitter edges", static_edges.len());
        insert_edges_reporting(self.ingest().graph(), &static_edges, "static")?;

        // 3c. Pattern Resolve Phase: Cross-boundary semantic edges from string literals
        if cancel.is_cancelled() {
            info!("build_core_memory: cancelled after static resolve");
            return Err(LainError::Cancelled);
        }
        info!(
            "Resolving {} pattern references for cross-boundary detection...",
            all_pattern_refs.len()
        );
        let pattern_edges = super::resolve::resolve_pattern_edges(
            self.ingest().graph(),
            &all_pattern_refs,
            super::resolve::PatternLimits::from_tuning(
                self.ingest().tuning(),
                super::resolve::PatternLimits::DEFAULT,
            ),
        );
        info!(
            "Ingesting {} cross-boundary pattern edges",
            pattern_edges.len()
        );
        insert_edges_reporting(self.ingest().graph(), &pattern_edges, "pattern")?;

        // 3d. Protocol sensors: HTTP routes, OpenAPI, proto, GraphQL,
        // WebSocket. Runs after the symbol nodes exist, because
        // `routes_to_graph` links a route to its handler by name and
        // needs that handler already in the graph.
        //
        // Nothing called `server::sensors` before this, so the node and
        // edge types it produces — `HttpRoute`, `CallsHttp`, `Implements`
        // — could never appear in a graph, while `describe_schema`
        // advertised them and `get_cross_runtime_callers` read them.
        let sensor_counts = crate::server::sensors::run_all(
            self.ingest().graph(),
            &self.ingest().config().workspace,
            self.ingest().id_namespace(),
        );
        if sensor_counts.total() > 0 {
            info!("Protocol sensors contributed {:?}", sensor_counts);
        }

        // 4. Temporal Analysis Phase: Co-changes
        if cancel.is_cancelled() {
            info!("build_core_memory: cancelled after pattern resolve");
            return Err(LainError::Cancelled);
        }
        let co_change_pairs: Vec<crate::git::CoChangePair> = {
            let window = self.ingest().tuning().ingestion.cochange_commit_window;
            let min_pair = self.ingest().tuning().ingestion.cochange_min_pair_count;
            let max_files = self.ingest().tuning().ingestion.cochange_max_commit_files;
            let git_sensor = Arc::clone(self.ingest().git());
            match offthread(
                cancel.clone(),
                move || -> Result<Vec<crate::git::CoChangePair>, LainError> {
                    git_sensor.try_analyze_co_changes(window, min_pair, max_files)
                },
            )
            .await
            {
                Ok(v) => v,
                Err(LainError::Cancelled) => return Err(LainError::Cancelled),
                Err(_) => Vec::new(),
            }
        };
        let co_change_tuples: Vec<_> = co_change_pairs
            .into_iter()
            .map(|p| (p.file1, p.file2, p.co_change_count))
            .collect();
        self.ingest()
            .graph()
            .insert_co_change_edges(&co_change_tuples)?;

        // 5. Enrichment Phase: Topological Algorithms (synchronous, fast)
        self.readiness().update(|snapshot| {
            snapshot.phase = crate::server::readiness::IndexPhase::Enriching;
        });
        if cancel.is_cancelled() {
            info!("build_core_memory: cancelled before enrichment");
            return Err(LainError::Cancelled);
        }
        info!("Enriching topology: Calculating anchors and depths...");
        self.ingest().graph().calculate_anchor_scores()?;
        self.ingest().graph().calculate_depths()?;

        // 6. NLP Phase: Spawn lazy background enrichment (non-blocking)
        // Pre-warm top anchor nodes first so first semantic queries return quickly
        // Then queue the rest for background processing
        let graph_clone = self.ingest().graph().clone();
        let embedder_clone = self.ingest().embedder().clone();
        let nlp_prewarm_count = self.ingest().tuning().ingestion.nlp_prewarm_count;
        let nlp_batch_size = self.ingest().tuning().ingestion.nlp_batch_size;
        let nlp_budget_per_pass = self.ingest().tuning().ingestion.nlp_budget_per_pass;
        // The NLP pass runs detached, so it needs its own copy of the
        // workspace root to resolve workspace-relative node paths.
        let ws_for_nlp = self.ingest().config().workspace.clone();
        // AGENT_UX_ROADMAP.md M4 follow-up: the detached NLP prewarm
        // task gets a *child* token, so cancelling the server-owned
        // token (via Drop / shutdown) is observed here too. A child
        // token never cancels its parent; cancelling the parent
        // cancels every child.
        let nlp_cancel: CancellationToken = cancel.child_token();
        let nlp_readiness = self.readiness().clone();
        tokio::spawn(async move {
            // Without a model there is nothing to compute: the stub returns
            // zero vectors, and storing one per symbol cost memory and disk
            // and marked every symbol "embedded" for when a model arrives.
            if nlp_cancel.is_cancelled() || embedder_clone.is_stub() {
                return;
            }
            let all_nodes = graph_clone.get_all_nodes();
            // Top anchors get embedded first (pre-warm)
            let mut anchors: Vec<_> = all_nodes
                .iter()
                .filter_map(|n| n.anchor_score.map(|s| (s, n.clone())))
                .collect();
            anchors.sort_by(|a, b| b.0.total_cmp(&a.0));

            let prewarm_count = anchors.len().min(nlp_prewarm_count);
            let (prewarm_nodes, rest_nodes) = anchors.split_at(prewarm_count);
            let prewarm: Vec<_> = prewarm_nodes.iter().map(|(_, n)| n.clone()).collect();
            let rest: Vec<_> = rest_nodes.iter().map(|(_, n)| n.clone()).collect();

            let already = anchors
                .iter()
                .filter(|(_, n)| !crate::server::nlp::needs_embedding(n.embedding.as_deref()))
                .count();
            // `total` drops for symbols that vanish before the pass reaches
            // them (replaced by a concurrent update): they cannot be
            // embedded, and counting them left coverage looking short.
            let progress = |embedded: usize, total: usize, running: bool| {
                nlp_readiness.update(|s| {
                    s.embeddings = Some(crate::server::readiness::EmbeddingProgress {
                        embedded: embedded as u64,
                        total: total as u64,
                        running,
                    });
                });
            };
            let mut embedded = already;
            let mut total = anchors.len();
            progress(embedded, total, true);

            info!("NLP pre-warming {} anchor nodes...", prewarm.len());
            let mut count = 0;
            for node in &prewarm {
                if nlp_cancel.is_cancelled() {
                    return;
                }
                if let Ok(Some(gn)) = graph_clone.get_node(&node.id) {
                    if crate::server::nlp::needs_embedding(gn.embedding.as_deref()) {
                        let text = crate::tools::utils::build_enriched_text(&gn, &ws_for_nlp);
                        // AGENT_UX_ROADMAP.md M4 follow-up: ONNX
                        // inference is sync CPU work — route it
                        // through `offthread` so a slow forward
                        // pass doesn't pin a Tokio worker. The
                        // per-call cancel boundary also lets a
                        // shutdown abort an in-flight embed.
                        let embedder_for_call = embedder_clone.clone();
                        let text_for_call = text.clone();
                        let emb_result = offthread(nlp_cancel.clone(), move || {
                            embedder_for_call.embed(&text_for_call)
                        })
                        .await;
                        let emb = match emb_result {
                            Ok(e) => e,
                            Err(LainError::Cancelled) => return,
                            Err(e) => {
                                warn!(
                                    "embedding inference failed for {}: {e}; leaving it \
                                     unembedded so a later pass retries",
                                    gn.name
                                );
                                continue;
                            }
                        };
                        // Never store a default on serialize failure.
                        // `unwrap_or_default()` wrote `Some("")`, which
                        // marks the node as embedded — `is_none()` is
                        // false, so it is never retried — while
                        // `executor.rs` fails to parse the empty string
                        // and skips it. The symbol disappears from
                        // `semantic_search` permanently and silently.
                        match serde_json::to_string(&emb) {
                            Ok(json) => {
                                if graph_clone.set_embedding(&gn.id, json).unwrap_or(false) {
                                    count += 1;
                                    embedded += 1;
                                    progress(embedded, total, true);
                                }
                            }
                            Err(e) => warn!(
                                "embedding not serialised for {}: {e}; leaving it \
                                 unembedded so a later pass retries",
                                gn.name
                            ),
                        }
                    }
                } else {
                    total = total.saturating_sub(1);
                    progress(embedded, total, true);
                }
            }
            info!(
                "NLP pre-warm complete ({} embedded). Queuing {} remaining nodes.",
                count,
                rest.len()
            );

            // Background lazy enrichment with backpressure
            let mut budget = nlp_budget_per_pass;
            for chunk in rest.chunks(nlp_batch_size) {
                if nlp_cancel.is_cancelled() {
                    return;
                }
                if budget == 0 {
                    break;
                }
                let to_embed: Vec<_> = chunk.iter().take(budget).cloned().collect();
                let batch_len = to_embed.len();
                for node in &to_embed {
                    if nlp_cancel.is_cancelled() {
                        return;
                    }
                    if let Ok(Some(gn)) = graph_clone.get_node(&node.id) {
                        if crate::server::nlp::needs_embedding(gn.embedding.as_deref()) {
                            let text = crate::tools::utils::build_enriched_text(&gn, &ws_for_nlp);
                            // Same offthread routing as the prewarm pass:
                            // keep ONNX off the async runtime.
                            let embedder_for_call = embedder_clone.clone();
                            let text_for_call = text.clone();
                            let emb_result = offthread(nlp_cancel.clone(), move || {
                                embedder_for_call.embed(&text_for_call)
                            })
                            .await;
                            let emb = match emb_result {
                                Ok(e) => e,
                                Err(LainError::Cancelled) => return,
                                Err(e) => {
                                    warn!(
                                        "embedding inference failed for {}: {e}; \
                                         leaving it unembedded so a later pass retries",
                                        gn.name
                                    );
                                    continue;
                                }
                            };
                            // Same reasoning as the prewarm pass above:
                            // a default here poisons the node with an
                            // unparseable embedding it will never retry.
                            match serde_json::to_string(&emb) {
                                Ok(json) => {
                                    // Only onto a node that still exists; a
                                    // re-index may have removed it meanwhile.
                                    if !graph_clone.set_embedding(&gn.id, json).unwrap_or(false) {
                                        total = total.saturating_sub(1);
                                    } else {
                                        embedded += 1;
                                        progress(embedded, total, true);
                                    }
                                }
                                Err(e) => {
                                    warn!("embedding not serialised for {}: {e}", gn.name)
                                }
                            }
                        }
                    } else {
                        total = total.saturating_sub(1);
                        progress(embedded, total, true);
                    }
                }
                budget = budget.saturating_sub(batch_len);
            }
            progress(embedded, total, false);
            info!("NLP lazy enrichment pass complete.");
            // The graph was saved before this pass ran; without a save here
            // every restart recomputed every embedding (minutes on a large
            // repository).
            if embedded > already {
                if let Err(e) = graph_clone.save_to_disk().await {
                    warn!("embeddings computed but not saved: {e}");
                }
            }
        });

        // Orphan sweep. Reclaims nodes whose file is no longer tracked: files
        // deleted or renamed outside this pass's view, and any backlog left by
        // builds that never deleted anything. Gated on a complete pass — after
        // a partial one, "not scanned this round" is indistinguishable from
        // "gone", and sweeping would delete live nodes.
        if !partial {
            if cancel.is_cancelled() {
                info!("build_core_memory: cancelled before orphan sweep");
                return Err(LainError::Cancelled);
            }
            let git_sensor = Arc::clone(self.ingest().git());
            let tracked_paths_result = offthread(
                cancel.clone(),
                move || -> Result<Vec<std::path::PathBuf>, LainError> {
                    git_sensor.try_get_all_tracked_files()
                },
            )
            .await;
            match tracked_paths_result {
                Ok(tracked_paths) => {
                    // Reduced with the same helper the scanner mints node paths
                    // with. Comparing git's absolute paths against relative node
                    // keys would mark every node an orphan, and that reads as a
                    // full sweep rather than as an error.
                    let tracked: HashSet<String> = tracked_paths
                        .iter()
                        .map(|p| crate::graph::graph_path(&self.ingest().config().workspace, p))
                        .collect();
                    match self.ingest().graph().prune_orphans(&tracked) {
                        Ok(0) => info!("Orphan sweep: nothing to prune"),
                        Ok(n) => info!("Orphan sweep: pruned {n} nodes for untracked files"),
                        Err(e) => warn!("Orphan sweep failed: {e}"),
                    }
                }
                Err(e) => warn!("Skipping orphan sweep: cannot list tracked files: {e}"),
            }
        }

        // Only claim "fully indexed through <commit>" when this pass actually
        // covered every changed file. A partial pass still persists its nodes and
        // edges below, so the work is kept and the next run resumes from it — but
        // the marker stays behind so `get_health` keeps reporting the true
        // commits-behind count instead of silently claiming to be current.
        self.readiness().update(|snapshot| {
            snapshot.phase = crate::server::readiness::IndexPhase::Persisting;
        });
        if cancel.is_cancelled() {
            info!("build_core_memory: cancelled before persist");
            return Err(LainError::Cancelled);
        }
        if partial {
            warn!(
                "Partial index pass ({} files scanned, {} failed);                  leaving indexed-commit marker unchanged",
                scanned, failed
            );
        } else {
            self.ingest().graph().set_last_commit(latest_commit)?;
        }
        // Persisting is for the next start; this process serves from memory.
        // An unwritable `.lain` failed the whole pass, so a complete
        // in-memory index was reported unavailable and redone every tick.
        if let Err(e) = self.ingest().graph().save_to_disk().await {
            warn!("could not persist the graph ({e}); serving it from memory only");
        }

        // Bump the overlay freshness so the indexer doesn't read as
        // "stale" the moment the server comes up. The index path
        // doesn't insert through the overlay (it writes the static
        // graph), so without this touch every freshly-indexed server
        // would start with `Overlay freshness: stale`.
        self.overlay().touch();

        let duration = scan_start.elapsed();

        // A partial pass (scan-phase timeout, or capped by
        // `max_files_per_scan`) persists whatever it produced above so the
        // next attempt resumes from it, but it must not be reported as a
        // successful attempt: `indexed_commit` was deliberately left behind
        // `target_commit`, and `files_completed < files_total`. Returning
        // `Ok(())` here let every caller (`await_startup_reindex`,
        // `run_background_sync`) publish `ready` on a graph known to be
        // incomplete. Matches the M4 design's "Refresh failed/timed out"
        // row: `unavailable_error`, valid persisted progress kept for a
        // future retry — not `ready`.
        if partial {
            warn!(
                "Partial index pass ({} files scanned, {} failed) persisted in {:?}; \
                 reporting as a failed attempt so the graph is not served as ready",
                scanned, failed, duration
            );
            return Err(LainError::Other(format!(
                "{PARTIAL_PASS}: {} of {} changed files scanned; {} left",
                scanned + failed,
                files.len(),
                remaining.len().saturating_sub(scanned + failed),
            )));
        }

        info!("Lain fully restored and ready in {:?}", duration);

        Ok(())
    }

    pub async fn sync_volatile_overlay(&self) -> Result<(), LainError> {
        if self.ingest().graph().is_read_only() {
            return Ok(());
        }
        // AGENT_UX_ROADMAP.md M4 follow-up: observe the server-owned
        // cancel token at the top of the reconciliation pass. Shutdown
        // during a slow LSP round-trip on a large diff used to keep
        // running until the loop drained; now it returns promptly.
        let cancel = self.lifecycle_handle().cancel_token();
        if cancel.is_cancelled() {
            return Err(LainError::Cancelled);
        }
        // The snapshot, removals, and replacements form one reconciliation.
        // Direct process_change calls use the same lock.
        let _guard = self.ingest().process_change_lock().lock().await;
        let changes = self.ingest().git().get_uncommitted_changes()?;
        let root = &self.ingest().config().workspace;
        let current_paths: HashSet<String> = changes
            .iter()
            .map(|change| graph_path(root, &change.path))
            .collect();
        let head = self
            .ingest()
            .git()
            .get_latest_commit_info()
            .ok()
            .map(|(h, _)| h);
        let graph_caught_up = match head {
            Some(h) => self.ingest().graph().get_last_commit()?.as_deref() == Some(h.as_str()),
            None => false,
        };
        let stale: Vec<String> = self
            .ingest()
            .overlay_paths()
            .lock()
            .keys()
            .filter(|path| !current_paths.contains(*path))
            .filter(|path| graph_caught_up || !root.join(path).is_file())
            .cloned()
            .collect();
        for path in stale {
            self.remove_owned_overlay_path(&path);
        }
        for change in changes {
            if cancel.is_cancelled() {
                return Err(LainError::Cancelled);
            }
            if let Err(e) = self.process_change_locked(&change.path).await {
                warn!("Failed to process change {:?}: {}", change.path, e);
            }
        }
        Ok(())
    }

    // Caller holds process_change_lock. Publish removal before any replacement
    // insert so a subscriber never deletes the newly inserted copy of an ID.
    fn remove_owned_overlay_path(&self, key: &str) {
        let ids = self
            .ingest()
            .overlay_paths()
            .lock()
            .remove(key)
            .unwrap_or_default();
        if ids.is_empty() {
            return;
        }
        for id in &ids {
            self.overlay().remove_node(id);
        }
        crate::server::overlay::broadcast_overlay_diff(OverlayDiff {
            revision: self.next_revision(),
            added: vec![],
            removed: ids,
            updated: vec![],
        });
    }

    pub async fn process_change(&self, path: &Path) -> Result<(), LainError> {
        if self.ingest().graph().is_read_only() {
            return Ok(());
        }
        // Mirror sync_volatile_overlay: observe the cancel token
        // before queueing behind the reconciliation lock. The lock
        // itself can be held for a long time on a busy repo, and
        // shutdown should not have to wait for it.
        if self.lifecycle_handle().is_cancelled() {
            return Err(LainError::Cancelled);
        }
        let _guard = self.ingest().process_change_lock().lock().await;
        self.process_change_locked(path).await
    }

    async fn process_change_locked(&self, path: &Path) -> Result<(), LainError> {
        let cancel = self.lifecycle_handle().cancel_token();
        if cancel.is_cancelled() {
            return Err(LainError::Cancelled);
        }
        let key = graph_path(&self.ingest().config().workspace, path);
        if !path.is_file() {
            self.remove_owned_overlay_path(&key);
            return Ok(());
        }
        // Try the LSP path first. With rust-analyzer unavailable (CI's
        // default for this test env), the LSP request errors out —
        // pre-fix, `process_change` returned `Ok(())` with no overlay
        // nodes, leaving the single-server overlay empty after every
        // edit. The federation equivalent has a tree-sitter fallback
        // (`RepoIndex::process_overlay_change`); mirror it here so the
        // single-server overlay actually receives symbols when LSP is
        // unavailable. URGENT FIXES #3 follow-up.
        use crate::server::lsp::HierarchicalSymbol;
        let symbols: Option<Vec<HierarchicalSymbol>> = {
            let lsp = self.ingest().lsp_pool().next();
            let mut lsp = lsp.lock().await;
            match lsp
                .get_document_symbols_hierarchical(
                    path,
                    &self.ingest().config().workspace,
                    self.ingest().id_namespace(),
                )
                .await
            {
                Ok(s) if !s.is_empty() => Some(s),
                Ok(_) => None, // cold LSP / empty — fall through to tree-sitter
                Err(e) => {
                    debug!(
                        "No LSP symbols for {:?}: {}; falling back to tree-sitter",
                        path, e
                    );
                    None
                }
            }
        };
        // Tree-sitter fallback when LSP fails or returns empty.
        // Mirrors `RepoIndex::process_overlay_change`: read the file,
        // mint one `GraphNode` per `SymbolDef`, namespace-namespaced
        // by `self.ingest().id_namespace()` so the overlay id matches the
        // static-graph id minted by `scan_file_structure`.
        let symbols: Vec<HierarchicalSymbol> = match symbols {
            Some(s) => s,
            None => {
                let Ok(content) = std::fs::read_to_string(path) else {
                    return Ok(());
                };
                let graph_key = graph_path(&self.ingest().config().workspace, path);
                crate::treesitter::extract_definitions(path, &content)
                    .into_iter()
                    .map(|d| HierarchicalSymbol {
                        node: crate::schema::GraphNode::new_in(
                            d.kind,
                            d.name.clone(),
                            graph_key.clone(),
                            self.ingest().id_namespace(),
                        )
                        .with_location_in(
                            d.line_start,
                            d.line_end,
                            self.ingest().id_namespace(),
                        ),
                        children: vec![],
                    })
                    .collect()
            }
        };
        // A successfully scanned empty file must also retract its old symbols.
        self.remove_owned_overlay_path(&key);
        // Track the workspace-relative path + the ids we inserted
        // there so the next `sync_volatile_overlay` cycle can purge
        // by id if this path drops out of the changes list.
        let workspace_root = self.ingest().config().workspace.clone();
        let key = graph_path(&workspace_root, path);
        let mut new_ids: Vec<String> = Vec::with_capacity(symbols.len());
        for symbol in symbols {
            self.overlay().insert_node(symbol.node.clone());
            // Broadcast the new node to any subscribed sidecar. The
            // read-only gate above (`is_read_only`) ensures this only
            // runs for owners.
            self.broadcast_overlay_insert(symbol.node.clone());
            new_ids.push(symbol.node.id);
        }
        self.ingest().overlay_paths().lock().insert(key, new_ids);
        Ok(())
    }
}

/// Per-repo ingestion pipeline used by the federation writer. Runs the same
/// algorithmic stages as `LainServer::build_core_memory` (latest-commit
/// short-circuit → file batch scan → resolve → co-change → enrich → save)
/// but takes the four components it needs directly so it can be called on
/// any `RepoSource` without instantiating a full `LainServer`.
///
/// Federation ingestion intentionally skips the per-server NLP pre-warm
/// phase (`tokio::spawn` block in `build_core_memory`); the global
/// `FederatedIndex` runs its own embedding/index work and we don't want to
/// block the per-repo write on it. The signature takes `&GitSensor` (not
/// `Arc<Mutex<GitSensor>>`) so the caller decides the locking strategy;
/// `RepoIndex::index` wraps the lock in a single `let _g = ...` scope.
/// Prune nodes whose file is no longer tracked by git.
///
/// Split out so both exits from `index_one_repo` run it. The
/// `files.is_empty()` early return did not, and that was a permanent
/// leak: `get_changed_files_since` deliberately skips paths that are no
/// longer on disk, so a commit that *only* deletes files yields an
/// empty scan list. The early return then advanced the last-commit
/// marker past that deletion, so no later pass ever revisited it and
/// the deleted file's symbols stayed in the graph forever. That is the
/// most likely reason a long-lived index carried more nodes than a
/// fresh index of the same commit (observed: 3769 vs 3340).
/// Insert edges and report any the graph refused.
///
/// `insert_edges_batch` skips edges whose endpoints are missing from the
/// index — necessary, since an edge to a node that isn't there can't be
/// added, but it must never be silent: that silence is what hid 37 of
/// 335 files whose symbols had no `Contains` edge from their own file
/// node, in a graph that reported itself healthy.
///
/// `label` names the phase ("call", "static", "batch") so a warning
/// says which part of the pipeline lost them.
fn insert_edges_reporting(
    db: &GraphDatabase,
    edges: &[GraphEdge],
    label: &str,
) -> Result<(), LainError> {
    match db.insert_edges_batch(edges) {
        Ok(0) => Ok(()),
        Ok(dropped) => {
            warn!(
                "{dropped} of {} {label} edges dropped (endpoint not in index)",
                edges.len()
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Same as [`insert_edges_reporting`] but for the flush path, where a
/// write error is logged and the pass continues rather than aborting.
fn insert_edges_best_effort(db: &GraphDatabase, edges: &[GraphEdge], label: &str) {
    if let Err(e) = insert_edges_reporting(db, edges, label) {
        warn!("{label} edge write error: {e}");
    }
}

fn sweep_orphans(path: &Path, db: &GraphDatabase, git: &AnyGitSensor) {
    match git.get_all_tracked_files() {
        Ok(tracked_paths) => {
            // Reduced with the same helper the scanner mints node paths with:
            // git returns absolute paths, and comparing those to relative node
            // keys marks every node an orphan rather than raising an error.
            let tracked: HashSet<String> = tracked_paths
                .iter()
                .map(|p| crate::graph::graph_path(path, p))
                .collect();
            match db.prune_orphans(&tracked) {
                Ok(0) => info!("[federation] {:?}: orphan sweep found nothing", path),
                Ok(n) => info!("[federation] {:?}: orphan sweep pruned {n} nodes", path),
                Err(e) => warn!("[federation] {:?}: orphan sweep failed: {e}", path),
            }
        }
        Err(e) => warn!("[federation] Skipping orphan sweep for {:?}: {e}", path),
    }
}

/// Link a federation repo's calls into the other repos, once every repo's
/// symbols are known.
///
/// Repos index one after another, and name-only cross-repo resolution can
/// only find a symbol whose repo is already indexed: a repo indexed before
/// the ones it calls into got no cross-repo edges at all (cobra, indexed
/// before pflag, had 0 edges into it). Cross-repo edges are also not
/// persisted per repo, so a warm restart lost them too. Running this after
/// every repo is indexed covers both. Only edges whose target lies in
/// another repo are added; local resolution already happened.
pub async fn relink_cross_repo(
    path: &Path,
    graph: &GraphDatabase,
    git: &Arc<AnyGitSensor>,
    resolver: &dyn crate::federation::cross_repo::CrossRepoResolver,
    source_repo: &crate::federation::repo_id::RepoId,
    cancel: &CancellationToken,
) -> Result<usize, LainError> {
    let git_sensor = Arc::clone(git);
    let files = offthread(cancel.clone(), move || {
        git_sensor.try_get_all_tracked_files()
    })
    .await?;
    let root = path.to_path_buf();
    let refs = offthread(
        cancel.clone(),
        move || -> Result<Vec<StaticFileRef>, LainError> {
            let mut refs = Vec::new();
            for file in files {
                let abs = if file.is_absolute() {
                    file.clone()
                } else {
                    root.join(&file)
                };
                let indexed = abs
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(crate::treesitter::is_indexed_extension);
                if !indexed {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&abs) else {
                    continue;
                };
                let rel = graph_path(&root, &abs);
                refs.extend(
                    crate::treesitter::extract_refs(&abs, &content)
                        .into_iter()
                        .map(|r| StaticFileRef {
                            file_path: rel.clone(),
                            source_line: r.source_line,
                            target_name: r.target_name,
                            edge_type: r.edge_type,
                            foreign_receiver: r.foreign_receiver,
                            self_receiver: r.self_receiver,
                            qualifier: r.qualifier.clone(),
                        }),
                );
            }
            Ok(refs)
        },
    )
    .await?;
    let external: Vec<GraphEdge> =
        super::resolve::resolve_static_edges(graph, &refs, Some(resolver), Some(source_repo))
            .into_iter()
            .filter(|e| graph.get_node(&e.target_id).ok().flatten().is_none())
            .collect();
    let n = external.len();
    graph.insert_edges_batch(&external)?;
    Ok(n)
}

/// Inputs for one repository indexing pass. The references deliberately tie
/// the graph, overlay, sensors, and namespace to the same call lifetime.
///
/// Field names match the per-subsystem config structs in this crate
/// (`ToolContextDeps`, `ToolExecutorConfig`): `graph`, `lsp_pool`,
/// `git`, `overlay`, `namespace`. The shorter `db`/`lsp` would have
/// been a footgun for anyone matching the two-struct pattern.
pub struct IndexRequest<'a> {
    pub path: &'a Path,
    pub graph: &'a GraphDatabase,
    pub lsp_pool: &'a LspPool,
    pub git: &'a AnyGitSensor,
    pub overlay: &'a VolatileOverlay,
    pub resolver: Option<&'a dyn crate::federation::cross_repo::CrossRepoResolver>,
    pub source_repo: Option<&'a crate::federation::repo_id::RepoId>,
    pub namespace: &'a crate::schema::RepoNamespace,
    pub force: bool,
    /// AGENT_UX_ROADMAP.md M4 follow-up: cooperative cancellation
    /// token observed by the LSP subprocess calls in
    /// `scan_file_structure`. Federation callers pass the
    /// server-owned token so a shutdown that lands mid-scan aborts
    /// the LSP round-trip promptly instead of waiting for the
    /// child to answer.
    pub cancel: &'a tokio_util::sync::CancellationToken,
}

pub async fn index_one_repo(request: IndexRequest<'_>) -> Result<(), LainError> {
    let IndexRequest {
        path,
        graph,
        lsp_pool,
        git,
        overlay,
        resolver,
        source_repo,
        namespace,
        force,
        cancel,
    } = request;
    if cancel.is_cancelled() {
        return Err(LainError::Cancelled);
    }
    let scan_start = std::time::Instant::now();
    let (latest_commit, latest_time) = git.get_latest_commit_info()?;
    let last_commit = graph.get_last_commit()?;

    // The commit-hash short-circuit exists to skip an expensive full
    // re-scan when nothing has changed on disk. The file-watcher path
    // fires on every `notify` event though, including edits the user
    // made *without* committing yet — and skipping the re-scan in that
    // case was the bug behind wishlist #17 (the new symbol was on disk
    // but the per-repo DB stayed at the previous commit). `force=true`
    // tells the indexer the caller has independent evidence a change
    // happened (a kernel inotify event, an explicit reindex request);
    // `false` keeps the optimization for the CLI boot loop and any
    // other caller that runs on a known-good commit cadence.
    if !force {
        if let Some(ref last) = last_commit {
            if last == &latest_commit {
                info!("[federation] {:?} already up to date at {}", path, last);
                return Ok(());
            }
        }
    }

    info!(
        "[federation] Building core topology for {:?} at commit {}",
        path, latest_commit
    );

    // `force=true` means the caller has independent evidence the
    // worktree changed (a kernel `notify` event, an explicit reindex
    // request) but the commit hash hasn't advanced yet — the user
    // hasn't committed. A `get_changed_files_since(last)` against
    // the unchanged commit tree returns the empty diff, and the
    // uncommitted edit silently goes missing. Walk every tracked
    // file in that case so the worktree state is the source of
    // truth.
    let files = if force {
        info!("[federation] Forced full re-scan of worktree {:?}", path);
        git.get_all_tracked_files()?
    } else if let Some(ref last) = last_commit {
        info!(
            "[federation] Incremental update since {} for {:?}",
            last, path
        );
        git.get_changed_files_since(last)?
    } else {
        info!("[federation] Full repository scan for {:?}", path);
        git.get_all_tracked_files()?
    };

    if files.is_empty() {
        // "No files to scan" is not "nothing changed": a deletion-only
        // commit lands here, because `get_changed_files_since` skips
        // paths that are gone from disk. Sweep before advancing the
        // marker, or those nodes are stranded permanently.
        info!(
            "[federation] No files to scan for {:?}; sweeping orphans.",
            path
        );
        if cancel.is_cancelled() {
            return Err(LainError::Cancelled);
        }
        sweep_orphans(path, graph, git);
        graph.set_last_commit(latest_commit)?;
        if let Err(e) = graph.save_to_disk_sync() {
            warn!("could not persist the graph ({e}); serving it from memory only");
        }
        return Ok(());
    }

    let lsp_sync_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Tighter batches than the default tuning for federation workloads —
    // repos are loaded concurrently and we want to keep each batch's wall
    // time bounded. The full pipeline doesn't need micro-batches here.
    const FILES_PER_BATCH: usize = 8;
    const INGEST_BATCH_SIZE: usize = 256;
    const COCHANGE_COMMIT_WINDOW: usize = 100;
    const COCHANGE_MIN_PAIR_COUNT: usize = 2;
    const COCHANGE_MAX_COMMIT_FILES: usize = 50;

    let files_to_scan: Vec<_> = files.into_iter().collect();
    let file_chunks: Vec<Vec<PathBuf>> = files_to_scan
        .chunks(FILES_PER_BATCH)
        .map(|chunk| chunk.to_vec())
        .collect();

    let mut set = tokio::task::JoinSet::new();
    for chunk in file_chunks {
        if cancel.is_cancelled() {
            return Err(LainError::Cancelled);
        }
        let lsp_mux = lsp_pool.next();
        let workspace = path.to_path_buf();
        let commit_hash = latest_commit.clone();
        let git_time = latest_time;
        // `RepoNamespace` is `Copy`; `index_one_repo` takes it by
        // reference, so the spawned task borrows from the captured
        // value (which lives for the closure's lifetime).
        let namespace = *namespace;
        let cancel_for_spawn = cancel.clone();

        set.spawn(async move {
            scan_file_batch(
                chunk,
                workspace,
                lsp_mux,
                lsp_sync_time,
                git_time,
                commit_hash,
                &namespace,
                cancel_for_spawn,
            )
            .await
        });
    }

    // Reduce phase
    let mut batch_nodes: Vec<GraphNode> = Vec::new();
    let mut batch_edges: Vec<GraphEdge> = Vec::new();
    let mut all_external_refs: Vec<(String, crate::lsp::ReferenceLocation)> = Vec::new();
    let mut all_static_refs: Vec<StaticFileRef> = Vec::new();
    let mut all_pattern_refs: Vec<PatternRef> = Vec::new();

    let mut scanned = 0usize;
    let mut failed = 0usize;

    while let Some(res) = set.join_next().await {
        if cancel.is_cancelled() {
            set.abort_all();
            return Err(LainError::Cancelled);
        }
        match res {
            Ok(batch_results) => {
                for file_result in batch_results {
                    match file_result {
                        Ok(scan_result) => {
                            scanned += 1;
                            batch_nodes.extend(scan_result.nodes);
                            batch_edges.extend(scan_result.edges);
                            all_external_refs.extend(scan_result.external_references);
                            all_static_refs.extend(scan_result.static_refs);
                            all_pattern_refs.extend(scan_result.pattern_refs);
                        }
                        Err(e) => {
                            failed += 1;
                            warn!("[federation] File scan error: {}", e);
                        }
                    }
                }

                if batch_nodes.len() >= INGEST_BATCH_SIZE {
                    // Replace, so a re-scan drops what a file no longer
                    // defines. Mirrors the single-workspace pipeline; without
                    // it a federated repo accumulates a node for every symbol
                    // ever deleted or moved. Scan results arrive whole-file and
                    // this runs between chunks, so each path here has all of
                    // its nodes in this batch.
                    let paths: Vec<String> = batch_nodes
                        .iter()
                        .map(|n| n.path.clone())
                        .collect::<HashSet<_>>()
                        .into_iter()
                        .collect();
                    if let Err(e) = graph.replace_nodes_for_paths(&paths, &batch_nodes) {
                        warn!("[federation] Batch node write error: {}", e);
                    }
                    insert_edges_best_effort(graph, &batch_edges, "batch");
                    batch_nodes.clear();
                    batch_edges.clear();
                }
            }
            Err(e) => {
                failed += 1;
                warn!("[federation] Task join error: {}", e);
            }
        }
    }

    if !batch_nodes.is_empty() {
        let paths: Vec<String> = batch_nodes
            .iter()
            .map(|n| n.path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if let Err(e) = graph.replace_nodes_for_paths(&paths, &batch_nodes) {
            warn!("[federation] Final batch node write error: {}", e);
        }
        insert_edges_best_effort(graph, &batch_edges, "final batch");
    }

    info!(
        "[federation] {:?}: scanned {} files, {} failed, {} external refs, {} static refs, {} pattern refs",
        path,
        scanned,
        failed,
        all_external_refs.len(),
        all_static_refs.len(),
        all_pattern_refs.len(),
    );

    // Resolve phase: link external references to internal nodes (CALLS)
    if cancel.is_cancelled() {
        return Err(LainError::Cancelled);
    }
    let call_edges =
        super::resolve::resolve_call_edges(graph, path, &all_external_refs, resolver, source_repo);
    info!(
        "[federation] {:?}: ingesting {} call edges",
        path,
        call_edges.len()
    );
    insert_edges_reporting(graph, &call_edges, "call")?;

    // Static resolve: tree-sitter derived Calls/Uses edges
    if cancel.is_cancelled() {
        return Err(LainError::Cancelled);
    }
    if last_commit.is_some() && !force {
        // Deleted and renamed-away paths go before resolving (see the
        // single-workspace pipeline for the rename that lost its callers).
        sweep_orphans(path, graph, git);
        let tracked = git.get_all_tracked_files().unwrap_or_default();
        all_static_refs.extend(refs_into_rescanned(path, graph, &files_to_scan, &tracked));
    }
    let static_edges =
        super::resolve::resolve_static_edges(graph, &all_static_refs, resolver, source_repo);
    info!(
        "[federation] {:?}: ingesting {} static tree-sitter edges",
        path,
        static_edges.len()
    );
    insert_edges_reporting(graph, &static_edges, "static")?;

    // Pattern resolve: cross-boundary detection
    if cancel.is_cancelled() {
        return Err(LainError::Cancelled);
    }
    let pattern_edges = super::resolve::resolve_pattern_edges(
        graph,
        &all_pattern_refs,
        super::resolve::PatternLimits::FEDERATION,
    );
    info!(
        "[federation] {:?}: ingesting {} cross-boundary pattern edges",
        path,
        pattern_edges.len()
    );
    graph.insert_edges_batch(&pattern_edges)?;

    // Refresh the federation's symbol index so the just-populated
    // per-repo DB is visible to subsequent cross-repo lookups in
    // this same `index_one_repo` call.
    if let Some(resolver) = resolver {
        resolver.refresh();
    }

    // Protocol sensors — same rationale as the single-workspace pipeline;
    // runs after symbol nodes exist so route->handler links resolve.
    let sensor_counts = crate::server::sensors::run_all(graph, path, namespace);
    if sensor_counts.total() > 0 {
        info!(
            "[federation] {:?}: protocol sensors contributed {:?}",
            path, sensor_counts
        );
    }

    // Co-change analysis
    let co_change_pairs = git
        .analyze_co_changes(
            COCHANGE_COMMIT_WINDOW,
            COCHANGE_MIN_PAIR_COUNT,
            COCHANGE_MAX_COMMIT_FILES,
        )
        .unwrap_or_default();
    let co_change_tuples: Vec<_> = co_change_pairs
        .into_iter()
        .map(|p| (p.file1, p.file2, p.co_change_count))
        .collect();
    graph.insert_co_change_edges(&co_change_tuples)?;

    // Enrichment: anchor scores + depths
    graph.calculate_anchor_scores()?;
    graph.calculate_depths()?;

    // Orphan sweep. This function has no scan-phase timeout and no
    // max-files cap, and the reduce loop always drains the JoinSet, so
    // reaching this point means the pass covered every changed file —
    // there is no partial case to gate on here, unlike the
    // single-workspace pipeline.
    if cancel.is_cancelled() {
        return Err(LainError::Cancelled);
    }
    sweep_orphans(path, graph, git);

    if cancel.is_cancelled() {
        return Err(LainError::Cancelled);
    }
    graph.set_last_commit(latest_commit)?;
    if let Err(e) = graph.save_to_disk_sync() {
        warn!("could not persist the graph ({e}); serving it from memory only");
    }

    info!(
        "[federation] {:?}: fully indexed in {:?}",
        path,
        scan_start.elapsed()
    );
    overlay.touch();
    Ok(())
}

/// References from files an incremental pass did *not* rescan to symbols
/// the rescanned files define.
///
/// A node's id includes its line, so a commit that shifts a function down a
/// line — or moves it to another file, or adds a function others already
/// call — gives it a new id. Edges from unchanged files were only carried
/// over for ids that survived, and those files' references were never
/// resolved again: the callers were gone for good (`assess_change` then
/// reported 0 dependents, "low" risk) until a full re-index. Re-resolving
/// just the references into the rescanned files' names makes an
/// incremental pass agree with a fresh one.
pub(crate) fn refs_into_rescanned(
    root: &Path,
    graph: &GraphDatabase,
    rescanned: &[PathBuf],
    tracked: &[PathBuf],
) -> Vec<StaticFileRef> {
    use crate::schema::NodeType;
    let scanned: HashSet<String> = rescanned.iter().map(|p| graph_path(root, p)).collect();
    let names: HashSet<String> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| {
            scanned.contains(&n.path)
                && !matches!(
                    n.node_type,
                    NodeType::File | NodeType::Namespace | NodeType::Module | NodeType::Package
                )
        })
        .map(|n| n.name)
        .collect();
    if names.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for abs in tracked {
        let rel = graph_path(root, abs);
        if scanned.contains(&rel) {
            continue;
        }
        let indexed = abs
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(crate::treesitter::is_indexed_extension);
        if !indexed {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(abs) else {
            continue;
        };
        // Cheap pre-filter: skip files that never mention one of the names.
        let mentions = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|w| names.contains(w));
        if !mentions {
            continue;
        }
        out.extend(
            crate::treesitter::extract_refs(abs, &text)
                .into_iter()
                .filter(|r| names.contains(&r.target_name))
                .map(|r| StaticFileRef {
                    file_path: rel.clone(),
                    source_line: r.source_line,
                    target_name: r.target_name,
                    edge_type: r.edge_type,
                    foreign_receiver: r.foreign_receiver,
                    self_receiver: r.self_receiver,
                    qualifier: r.qualifier,
                }),
        );
    }
    out
}

#[cfg(test)]
mod readiness_progress_tests {
    use super::*;

    fn git_fixture_with_one_file() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root.path())
            .status()
            .unwrap()
            .success());
        for (k, v) in [
            ("user.email", "readiness-test@lain"),
            ("user.name", "readiness-test"),
        ] {
            std::process::Command::new("git")
                .args(["config", k, v])
                .current_dir(root.path())
                .status()
                .unwrap();
        }
        std::fs::write(root.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-q", "-m", "fixture"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success());
        root
    }

    /// When the server's cancel token fires during the LSP
    /// prewarm phase, `build_core_memory` returns
    /// `LainError::Cancelled` AND the readiness snapshot's `phase`
    /// is reset to `Discovering` rather than left dangling on
    /// `PrewarmingLsp`. Without this fix the operator-visible
    /// `get_capabilities.phase` looked frozen on a successful
    /// shutdown — the snapshot never recovered until the next
    /// indexing pass moved it forward again.
    ///
    /// Pin down both halves of the contract: the return is
    /// `Cancelled`, AND the readiness snapshot is reset.
    #[tokio::test]
    async fn build_core_memory_resets_readiness_on_prewarm_cancel() {
        let root = git_fixture_with_one_file();
        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();
        disable_real_lsp(&server, root.path()).await;

        // Cancel before `build_core_memory` runs. The prewarm path
        // observes the token at the top of its loop, returns
        // `Cancelled`, and resets the readiness phase. Driving
        // the cancel from the outside (rather than racing inside
        // the test) makes the test deterministic on slow CI
        // runners where prewarm would otherwise complete before
        // the test thread could cancel.
        server.lifecycle_handle().cancel();

        let result = server.build_core_memory().await;
        match result {
            Err(LainError::Cancelled) => {}
            Err(other) => {
                panic!("expected LainError::Cancelled from a cancelled prewarm, got {other:?}")
            }
            Ok(()) => {
                panic!("build_core_memory returned Ok after the cancel token fired during prewarm")
            }
        }

        // Readiness must be Discovering, not stuck on PrewarmingLsp.
        // The whole point of the fix in this PR is that an agent
        // polling get_capabilities after the cancel does not see a
        // frozen phase.
        let snapshot = server.readiness().snapshot();
        assert_eq!(
            snapshot.phase,
            crate::server::readiness::IndexPhase::Discovering,
            "readiness.phase must reset to Discovering on prewarm cancel, got {:?}",
            snapshot.phase
        );
        assert_ne!(
            snapshot.phase,
            crate::server::readiness::IndexPhase::PrewarmingLsp,
            "readiness.phase must not stay on PrewarmingLsp after cancel"
        );
    }

    /// `lsp_prewarm_skip_extensions` must actually filter languages out
    /// of the prewarm pass. The knob-reachability test pins that the
    /// field is *referenced* by production code; this test pins that
    /// the filter *works*. Without it a refactor that silently breaks
    /// the skip logic (e.g. accidentally re-including the filtered
    /// extension after a `.collect()` dedup change) would still pass
    /// every other test in this module.
    ///
    /// Fixture: a git repo with both `lib.rs` and `test.py`. Tuning
    /// is set to `lsp_prewarm_skip_extensions = ["rs"]` via the
    /// workspace's `.lain/tuning.toml` (which `LainServer::new`
    /// loads via `tuning::load_tuning_config`). Both LSP binaries are
    /// pre-marked unavailable so the post-prewarm `prewarm_state`
    /// records a deterministic outcome per binary — `pylsp` reaches
    /// `prewarm_server` and lands as `SkippedUnavailable`; `rust-analyzer`
    /// never reaches `prewarm_server` because it's filtered out
    /// before the JoinSet is built.
    #[tokio::test]
    async fn lsp_prewarm_skip_extensions_actually_filters_languages() {
        use crate::server::lsp::PrewarmOutcome;

        let root = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root.path())
            .status()
            .unwrap()
            .success());
        for (k, v) in [
            ("user.email", "readiness-test@lain"),
            ("user.name", "readiness-test"),
        ] {
            std::process::Command::new("git")
                .args(["config", k, v])
                .current_dir(root.path())
                .status()
                .unwrap();
        }
        std::fs::create_dir_all(root.path().join(".lain")).unwrap();
        std::fs::write(
            root.path().join(".lain").join("tuning.toml"),
            "[ingestion]\nlsp_prewarm_skip_extensions = [\"rs\"]\n",
        )
        .unwrap();
        std::fs::write(root.path().join("lib.rs"), "pub fn hello() {}\n").unwrap();
        std::fs::write(root.path().join("test.py"), "def hello(): pass\n").unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-q", "-m", "fixture"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success());

        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();

        // Mark every multiplexer in the pool as unavailable for both
        // rust-analyzer AND pylsp. The existing helper only marks
        // rust-analyzer; for this test we need both so the post-
        // prewarm `prewarm_state` has a known outcome per binary
        // (SkippedUnavailable). Marking only rust-analyzer would
        // leave pylsp eligible to actually try to spawn, which can
        // hang on `LspProcess::Drop` per the `disable_real_lsp`
        // helper's doc comment.
        let pool_size = crate::tuning::load_tuning_config(root.path())
            .ingestion
            .lsp_pool_size;
        for _ in 0..pool_size {
            let mplex = server.ingest().lsp_pool().next();
            let mut guard = mplex.lock().await;
            guard.mark_unavailable("rust-analyzer");
            guard.mark_unavailable("pylsp");
        }

        // Run the full indexing pipeline. The prewarm phase runs
        // first; the scan phase that follows is what most of the
        // test runtime pays for. We bound it with a generous
        // timeout so CI flakiness on the scan side doesn't show up
        // as a flake in this test.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            server.build_core_memory(),
        )
        .await
        .expect("build_core_memory must complete within 60s with skip-list applied");

        // Aggregate the per-multiplexer prewarm state. Every
        // multiplexer recorded the same outcomes for the same
        // binary, so the union is the merged answer.
        let outcomes = server
            .ingest()
            .lsp_pool()
            .aggregate_prewarm_outcomes()
            .await;

        // The whole point: rust-analyzer never reached
        // prewarm_server because the skip-list filtered it out
        // before the JoinSet was built. The prewarm_state hash
        // must NOT contain a rust-analyzer key.
        assert!(
            !outcomes.contains_key("rust-analyzer"),
            "rust-analyzer must be excluded from prewarm_state when \
             lsp_prewarm_skip_extensions = [\"rs\"]; got {:?}",
            outcomes
        );

        // The non-skipped language (pylsp) DID reach prewarm_server
        // and landed as SkippedUnavailable (we marked pylsp
        // unavailable above). The test would also pass if pylsp's
        // outcome were TimedOut or Failed — the contract we're
        // pinning is "skipped extensions are excluded, not-skipped
        // extensions still go through prewarm_server".
        match outcomes.get("pylsp") {
            Some(PrewarmOutcome::SkippedUnavailable) => {}
            Some(other) => panic!(
                "pylsp entry must be SkippedUnavailable after the skip-list \
                 filter; got {other:?}. outcomes={outcomes:?}"
            ),
            None => panic!(
                "pylsp must appear in prewarm_state — the skip list only \
                 excluded rust-analyzer, not pylsp; outcomes={outcomes:?}"
            ),
        }

        // The prewarm phase must complete normally (not stuck on
        // PrewarmingLsp after build_core_memory returns). The
        // cancel-reset test pins the cancel path; this test pins
        // the normal-completion path.
        let snapshot = server.readiness().snapshot();
        assert_ne!(
            snapshot.phase,
            crate::server::readiness::IndexPhase::PrewarmingLsp,
            "readiness.phase must advance past PrewarmingLsp when \
             prewarm completes normally; got {:?}",
            snapshot.phase
        );
    }

    /// Mark every multiplexer in the server's LSP pool unavailable so
    /// `build_core_memory` takes the tree-sitter fallback path instead of
    /// spawning a real `rust-analyzer`. Mirrors the same guard in
    /// `scan.rs`'s tests: the `lsp-bridge` crate's `LspProcess::Drop` can
    /// hang cleaning up a real, in-process LSP child, which turns any test
    /// that actually drives the LSP path into an intermittent multi-minute
    /// hang instead of a failure.
    async fn disable_real_lsp(server: &LainServer, workspace: &Path) {
        // `next()` round-robins over a fixed, private Vec with no length
        // accessor; call it exactly as many times as the pool has entries
        // (the same tuning value `LainServer::new` used to size it) so
        // every multiplexer gets marked, not just however many a smaller
        // guess would have covered.
        let pool_size = crate::tuning::load_tuning_config(workspace)
            .ingestion
            .lsp_pool_size;
        for _ in 0..pool_size {
            server
                .ingest()
                .lsp_pool()
                .next()
                .lock()
                .await
                .mark_unavailable("rust-analyzer");
        }
    }

    /// `build_core_memory` is the one place that reports indexing progress;
    /// `readiness()` is the one shared handle `get_capabilities` and the
    /// central MCP gate both read. This pins that the coordinator actually
    /// publishes phase and file-count progress into it — before this,
    /// `IndexLifecycleSnapshot` only ever moved on `ready()`/`failed()` at
    /// the very end, so a client polling `get_capabilities` mid-index saw
    /// nothing but the static `warming_up/discovering` default the whole
    /// time.
    #[tokio::test]
    async fn build_core_memory_reports_file_progress_and_target_commit() {
        let root = git_fixture_with_one_file();
        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();
        disable_real_lsp(&server, root.path()).await;
        assert_eq!(
            server.readiness().snapshot().state,
            crate::server::readiness::IndexState::WarmingUp
        );

        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            server.build_core_memory(),
        )
        .await
        .expect("build_core_memory must not hang past the test's own budget")
        .unwrap();

        let snapshot = server.readiness().snapshot();
        assert_eq!(snapshot.files_total, Some(1));
        assert_eq!(snapshot.files_completed, 1);
        assert_eq!(snapshot.files_failed, 0);
        assert_eq!(
            snapshot.phase,
            crate::server::readiness::IndexPhase::Persisting
        );
        assert_eq!(
            snapshot.target_commit,
            server.ingest().graph().get_last_commit().unwrap()
        );
        // `build_core_memory` only reports progress; only the startup
        // coordinator (`await_startup_reindex`) decides `ready`/`failed`,
        // so the state itself must stay `warming_up` here.
        assert_eq!(
            snapshot.state,
            crate::server::readiness::IndexState::WarmingUp
        );
    }

    /// `git mv a.py lib.py` (committed) keeps the moved function's
    /// callers on an incremental pass.
    #[tokio::test]
    async fn a_renamed_file_keeps_its_callers() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .status()
                .unwrap()
                .success())
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(root.path().join("a.py"), "def helper_hh():\n    return 1\n").unwrap();
        std::fs::write(
            root.path().join("b.py"),
            "from a import helper_hh\n\ndef user_uu():\n    return helper_hh()\n",
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "one"]);
        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();
        disable_real_lsp(&server, root.path()).await;
        server.build_core_memory().await.unwrap();
        let callers = |server: &LainServer| {
            let g = server.ingest().graph();
            let target = g
                .find_all_nodes_by_name("helper_hh")
                .into_iter()
                .find(|n| n.node_type == crate::schema::NodeType::Function)
                .expect("helper_hh indexed");
            (
                target.path.clone(),
                g.get_edges_to(&target.id)
                    .unwrap()
                    .iter()
                    .filter(|e| e.edge_type == crate::schema::EdgeType::Calls)
                    .count(),
            )
        };
        assert_eq!(callers(&server), ("a.py".to_string(), 1));
        git(&["mv", "a.py", "lib.py"]);
        git(&["commit", "-qm", "move"]);
        server.build_core_memory().await.unwrap();
        assert_eq!(callers(&server), ("lib.py".to_string(), 1));
    }

    /// Bug #2 from the 2026-09-18 Tauri postmortem: a libgit2 call that
    /// wedges (packed-refs read on a huge monorepo, hung filesystem)
    /// leaves the parking_lot `GitSensor` mutex held by the stuck
    /// `spawn_blocking` thread. Pre-fix, every subsequent
    /// `build_core_memory` (and watcher-triggered `index_forced`)
    /// would block forever on `git_sensor.lock()` waiting for that
    /// stuck thread. The post-fix `build_core_memory` uses `try_lock`
    /// inside the offthread closures and fails fast with
    /// `LainError::Other` if the mutex is held. This test holds the
    /// mutex externally and confirms `build_core_memory` returns the
    /// structured error within a few hundred ms instead of hanging
    /// on the test's outer 5 s budget.
    ///
    /// Would hang on pre-fix code: the spawned `offthread` closure
    /// would call `git_sensor.lock()` and never return, the outer
    /// `tokio::time::timeout` would fire, and the test would panic
    /// with "build_core_memory must not hang past 5s".
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn default_mode_is_sidecar() {
        // On Unix. The sidecar needs Unix sockets; elsewhere the default
        // is the in-process sensor.
        let expected = if cfg!(unix) {
            crate::git::GitSensorMode::Sidecar
        } else {
            crate::git::GitSensorMode::InProcess
        };
        assert_eq!(crate::git::GitSensorMode::default(), expected);
        let root = git_fixture_with_one_file();
        // Force the sidecar mode explicitly so the test is deterministic
        // regardless of whether the env-var resolution finds a usable sidecar
        // binary on the host.
        let server = LainServer::with_git_sensor_mode(
            root.path(),
            &root.path().join("state/graph.bin"),
            None,
            crate::git::GitSensorMode::Sidecar,
        )
        .unwrap();
        assert_eq!(server.ingest().git().mode(), expected);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn build_core_memory_fails_fast_when_git_sensor_mutex_held() {
        let root = git_fixture_with_one_file();
        let server = LainServer::with_git_sensor_mode(
            root.path(),
            &root.path().join("state/graph.bin"),
            None,
            crate::git::GitSensorMode::InProcess,
        )
        .unwrap();
        disable_real_lsp(&server, root.path()).await;

        // Externally grab the parking_lot `GitSensor` mutex to simulate
        // a previous `index()` call's stuck libgit2 thread still
        // holding it. parking_lot's `lock()` is infallible; the
        // offthread closure's `try_lock` will see `None`.
        let _held = server
            .ingest()
            .git()
            .as_in_process()
            .expect("test runs in InProcess mode")
            .lock();

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            server.build_core_memory(),
        )
        .await
        .expect(
            "build_core_memory must not hang past 5s when the git sensor mutex is held externally",
        );

        assert!(
            matches!(result, Err(LainError::Other(_))),
            "expected LainError::Other from try_lock failure; got {result:?}"
        );
        // try_lock is O(1) and the offthread future resolves
        // immediately on WouldBlock. Generous slack for slow CI.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "build_core_memory took too long to fail-fast: {:?}",
            started.elapsed()
        );
    }

    /// Companion to `build_core_memory_fails_fast_when_git_sensor_mutex_held`:
    /// the postmortem's actual user-visible failure was the watcher
    /// *pile-up* — rust-analyzer fires many diagnostics in quick
    /// succession after projection, each one triggering
    /// `index_forced`, and every one blocked forever on the parking_lot
    /// mutex held by the stuck thread. This test pins the concurrent
    /// case: N parallel `build_core_memory` calls, all expected to
    /// fail fast with `LainError::Other`. Pre-fix they would have
    /// queued on the parking_lot mutex; post-fix they all return
    /// within a few ms.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn build_core_memory_concurrent_calls_fail_fast_when_mutex_held() {
        use std::sync::Arc;
        let root = git_fixture_with_one_file();
        let server = Arc::new(
            LainServer::with_git_sensor_mode(
                root.path(),
                &root.path().join("state/graph.bin"),
                None,
                crate::git::GitSensorMode::InProcess,
            )
            .unwrap(),
        );
        disable_real_lsp(&server, root.path()).await;

        // Hold the parking_lot mutex externally — simulates a stuck
        // spawn_blocking thread holding the guard.
        let _held = server
            .ingest()
            .git()
            .as_in_process()
            .expect("test runs in InProcess mode")
            .lock();

        const N: usize = 8;
        let started = std::time::Instant::now();
        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let server = server.clone();
            handles.push(tokio::spawn(async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    server.build_core_memory(),
                )
                .await
            }));
        }

        let mut other_count = 0;
        for h in handles {
            match h.await.expect("task panicked") {
                Ok(Err(LainError::Other(_))) => other_count += 1,
                Ok(Ok(())) => {
                    panic!("expected LainError::Other from try_lock failure, got Ok")
                }
                Ok(Err(e)) => panic!("expected LainError::Other, got {e:?}"),
                Err(_) => panic!("a concurrent build_core_memory hung past the 5s budget"),
            }
        }
        assert_eq!(other_count, N, "every concurrent call must fail fast");

        // try_lock is O(1) and the offthread futures resolve
        // immediately on WouldBlock. Generous slack for slow CI.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "concurrent fail-fast took too long: {:?}",
            started.elapsed()
        );
    }

    /// A no-op re-index (graph already current at `HEAD`) must not corrupt
    /// progress fields left over from a previous attempt into looking like
    /// a fresh scan happened — it returns before touching `files_total`, so
    /// a stale value from an earlier attempt could otherwise leak through.
    /// This pins that a second call is at least self-consistent: `files_total`
    /// is always defined and never smaller than `files_completed`.
    #[tokio::test]
    async fn a_second_up_to_date_call_leaves_progress_internally_consistent() {
        let root = git_fixture_with_one_file();
        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();
        disable_real_lsp(&server, root.path()).await;
        let budget = std::time::Duration::from_secs(60);
        tokio::time::timeout(budget, server.build_core_memory())
            .await
            .expect("first build_core_memory must not hang past the test's own budget")
            .unwrap();
        tokio::time::timeout(budget, server.build_core_memory())
            .await
            .expect("second build_core_memory must not hang past the test's own budget")
            .unwrap();

        let snapshot = server.readiness().snapshot();
        let total = snapshot.files_total.expect("files_total must be set");
        assert!(snapshot.files_completed <= total);
        assert!(snapshot.files_failed <= snapshot.files_completed);
    }

    /// A second, real re-index after the coordinator already published
    /// `ready` from a first pass must move the gate through
    /// `warming_up` again, not leave `state` at `Ready` for a graph
    /// that's being actively mutated (Copilot review finding on PR #63:
    /// a commit-sync/background-sync re-index after the first `ready`
    /// changed only `phase`, never `state`, so the central gate kept
    /// dispatching `graph_required` tools throughout it). Pinned via
    /// `attempt_id`, which only `resume_warming_up`/the initial default
    /// ever advance — a deterministic signal that doesn't need to catch
    /// the pass mid-flight the way asserting on `state` alone would.
    #[tokio::test]
    async fn a_second_real_reindex_after_ready_resumes_warming_up() {
        let root = git_fixture_with_one_file();
        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();
        disable_real_lsp(&server, root.path()).await;
        let budget = std::time::Duration::from_secs(60);
        tokio::time::timeout(budget, server.build_core_memory())
            .await
            .expect("first build_core_memory must not hang past the test's own budget")
            .unwrap();

        // Simulate what `await_startup_reindex`/`run_background_sync` do
        // after a successful pass: publish `ready`.
        server
            .readiness()
            .ready(server.ingest().graph().get_last_commit().ok().flatten());
        let attempt_after_ready = server.readiness().snapshot().attempt_id;
        assert_eq!(
            server.readiness().snapshot().state,
            crate::server::readiness::IndexState::Ready
        );

        // A second real commit, so the next build_core_memory call takes
        // the real re-index path, not the no-op "already up to date" one.
        std::fs::write(root.path().join("lib2.rs"), "pub fn world() {}\n").unwrap();
        for args in [["add", "-A"].as_slice(), &["commit", "-q", "-m", "second"]] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .status()
                .unwrap()
                .success());
        }

        tokio::time::timeout(budget, server.build_core_memory())
            .await
            .expect("second build_core_memory must not hang past the test's own budget")
            .unwrap();

        let snapshot = server.readiness().snapshot();
        assert!(
            snapshot.attempt_id > attempt_after_ready,
            "a real second re-index must bump attempt_id via resume_warming_up \
             (before: {attempt_after_ready}, after: {})",
            snapshot.attempt_id
        );
        assert_eq!(
            snapshot.state,
            crate::server::readiness::IndexState::WarmingUp,
            "build_core_memory itself never publishes ready; only its caller does, \
             so it must still read warming_up right after the pass completes"
        );
    }

    /// A pass capped by `max_files_per_scan` before covering every changed
    /// file is "partial": it persists whatever it scanned (so the next
    /// attempt can resume) but must not report success — `indexed_commit`
    /// is deliberately left behind `HEAD`, and every caller
    /// (`await_startup_reindex`, `run_background_sync`) reads a bare `Ok`
    /// as "publish `ready`." Before this fix, a capped pass returned
    /// `Ok(())` and the graph was served as ready with files missing.
    #[tokio::test]
    async fn a_capped_partial_pass_reports_failure_not_success() {
        let root = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root.path())
            .status()
            .unwrap()
            .success());
        for (k, v) in [
            ("user.email", "readiness-test@lain"),
            ("user.name", "readiness-test"),
        ] {
            std::process::Command::new("git")
                .args(["config", k, v])
                .current_dir(root.path())
                .status()
                .unwrap();
        }
        // Two files, so max_files_per_scan=1 below caps this pass short of
        // covering every changed file.
        std::fs::write(root.path().join("a.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(root.path().join("b.rs"), "pub fn b() {}\n").unwrap();
        std::fs::create_dir_all(root.path().join(".lain")).unwrap();
        std::fs::write(
            root.path().join(".lain/tuning.toml"),
            "[ingestion]\nmax_files_per_scan = 1\n",
        )
        .unwrap();
        assert!(std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "-q", "-m", "fixture"])
            .current_dir(root.path())
            .status()
            .unwrap()
            .success());

        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();
        disable_real_lsp(&server, root.path()).await;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            server.build_core_memory(),
        )
        .await
        .expect("build_core_memory must not hang past the test's own budget");

        assert!(
            result.is_err(),
            "a capped partial pass must report failure, not Ok(()), so callers don't publish ready"
        );
        // The partial pass still persists what it scanned...
        // One source file (the cap) plus the committed `.lain/tuning.toml`,
        // which is not source and does not count toward it.
        let snapshot = server.readiness().snapshot();
        assert_eq!(snapshot.files_total, Some(2));
        // ...but must not have advanced the indexed-commit marker to HEAD.
        assert_eq!(server.ingest().graph().get_last_commit().unwrap(), None);

        // The next pass resumes with the file the first one skipped, and
        // completes: a capped repository converges instead of rescanning
        // the same first files forever.
        server
            .build_core_memory()
            .await
            .expect("second pass completes");
        assert!(server.ingest().graph().get_last_commit().unwrap().is_some());
        for f in ["a", "b"] {
            assert!(
                server.ingest().graph().find_node_by_name(f).is_some(),
                "{f} indexed"
            );
        }
    }

    #[test]
    fn partial_errors_carry_the_files_left() {
        let e = LainError::Other(format!(
            "{PARTIAL_PASS}: 5 of 12 changed files scanned; 7 left"
        ));
        assert_eq!(partial_files_left(&e), Some(7));
        assert_eq!(partial_files_left(&LainError::Other("boom".into())), None);
    }

    /// An unwritable state directory costs persistence, not the index.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unwritable_state_dir_still_serves_the_index() {
        use std::os::unix::fs::PermissionsExt;
        let root = git_fixture_with_one_file();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let server = LainServer::new(root.path(), &state.join("graph.bin"), None).unwrap();
        disable_real_lsp(&server, root.path()).await;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = server.build_core_memory().await;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        result.expect("indexing succeeds without persistence");
        assert!(server.ingest().graph().find_node_by_name("hello").is_some());
    }

    /// Non-source files do not count toward `max_files_per_scan`.
    #[tokio::test]
    async fn non_source_files_do_not_use_up_the_scan_cap() {
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(root.path())
                .status()
                .unwrap()
                .success())
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        for i in 0..5 {
            std::fs::write(root.path().join(format!("d{i}.json")), "{}").unwrap();
        }
        std::fs::write(root.path().join("only.py"), "def only_fn():\n    pass\n").unwrap();
        std::fs::create_dir_all(root.path().join(".lain")).unwrap();
        std::fs::write(
            root.path().join(".lain/tuning.toml"),
            "[ingestion]\nmax_files_per_scan = 1\n",
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "fixture"]);
        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();
        disable_real_lsp(&server, root.path()).await;
        server
            .build_core_memory()
            .await
            .expect("one source file fits the cap");
        assert!(server
            .ingest()
            .graph()
            .find_node_by_name("only_fn")
            .is_some());
    }
}

#[cfg(test)]
mod reconciliation_lock_tests {
    use super::*;

    #[tokio::test]
    async fn both_overlay_entry_points_wait_for_reconciliation_lock() {
        let root = tempfile::tempdir().unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root.path())
            .status()
            .unwrap()
            .success());
        let server =
            LainServer::new(root.path(), &root.path().join("state/graph.bin"), None).unwrap();
        let guard = server.ingest().process_change_lock().lock().await;
        let budget = std::time::Duration::from_millis(30);
        assert!(tokio::time::timeout(budget, server.sync_volatile_overlay())
            .await
            .is_err());
        assert!(tokio::time::timeout(
            budget,
            server.process_change(&root.path().join("missing.rs"))
        )
        .await
        .is_err());
        drop(guard);
        server
            .process_change(&root.path().join("missing.rs"))
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod nlp_offthread_tests {
    //! Tests for the ONNX → offthread migration (PR B follow-up).
    //! The full NLP prewarm is exercised end-to-end in
    //! `tests/cancellation_token.rs`; here we just pin the
    //! call-site contract — `NlpEmbedder::embed` runs on the
    //! blocking pool when called via `offthread`, a pre-cancelled
    //! token short-circuits without invoking the embedder.
    use super::*;
    use crate::nlp::NlpEmbedder;
    use crate::server::ingest::blocking::offthread;
    use crate::tuning::TuningConfig;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    /// `NlpEmbedder::new_stub()` produces a stub that returns an
    /// empty Vec<f32> from `embed`. Wrapping in `offthread` with
    /// a pre-cancelled token must return `LainError::Cancelled`
    /// without ever calling the embedder.
    #[tokio::test(flavor = "current_thread")]
    async fn offthread_embed_short_circuits_on_pre_cancelled_token() {
        let embedder = NlpEmbedder::new_stub();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result: Result<Vec<f32>, LainError> =
            offthread(cancel, move || embedder.embed("hello")).await;
        assert!(matches!(result, Err(LainError::Cancelled)));
    }

    /// When the token is *not* cancelled, offthread returns the
    /// embedder's success value verbatim. The stub returns a
    /// `Vec<f32>` whose length matches the embedder's
    /// `embedding_dim()` (typically 384). We assert the round
    /// trip succeeds and the returned length is positive.
    #[tokio::test(flavor = "current_thread")]
    async fn offthread_embed_returns_stub_value_when_not_cancelled() {
        let embedder = NlpEmbedder::new_stub();
        let dim = embedder.embedding_dim();
        let cancel = CancellationToken::new();
        let result: Result<Vec<f32>, LainError> =
            offthread(cancel, move || embedder.embed("hello")).await;
        let v = result.expect("embed returns Ok in stub mode");
        assert_eq!(v.len(), dim, "stub returns dim-length vector");
    }

    /// Sanity: `TuningConfig::default()` exists (the production
    /// offthread helper takes a `&TuningConfig`).
    #[test]
    fn tuning_config_default_is_available() {
        let _t: TuningConfig = TuningConfig::default();
        let _arc: Arc<TuningConfig> = Arc::new(TuningConfig::default());
    }
}

/// How a partial pass's error begins; see [`partial_files_left`].
const PARTIAL_PASS: &str = "index pass was partial";

/// Files a partial pass left for the next one, read from its error.
fn partial_files_left(e: &LainError) -> Option<usize> {
    let LainError::Other(msg) = e else {
        return None;
    };
    msg.strip_prefix(PARTIAL_PASS)?
        .rsplit_once("; ")?
        .1
        .strip_suffix(" left")?
        .parse()
        .ok()
}
