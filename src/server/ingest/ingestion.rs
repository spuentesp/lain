use crate::error::LainError;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use crate::server::overlay::VolatileOverlay;
use super::LainServer;
use super::scan::{scan_file_batch, StaticFileRef, PatternRef};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

impl LainServer {
    /// The "Sane" Ingestion Pipeline: Map -> Reduce -> Resolve -> Enrich.
    /// `&self` (not `&mut self`) because every field this method writes
    /// to is `Arc`-shared (graph, presence, occupancy, broadcast) or
    /// `Arc<Mutex<…>>` (git, lsp) — so calling it on a clone of the
    /// `LainServer` is safe and the writes are visible to the original
    /// `Arc<LainServer>` the MCP layer holds.
    pub async fn build_core_memory(&self) -> Result<(), LainError> {
        // Defensive gate: sidecar processes should never call build_core_memory,
        // but if a future refactor routes them here, bail out cleanly instead
        // of corrupting the shared on-disk graph.
        if self.graph.is_read_only() {
            return Ok(());
        }
        let scan_start = std::time::Instant::now();
        let (latest_commit, latest_time) = self.git.lock().get_latest_commit_info()?;
        let last_commit = self.graph.get_last_commit()?;

        if let Some(ref last) = last_commit {
            if last == &latest_commit {
                info!("Core memory is already up to date with commit {}", last);
                return Ok(());
            }
        }

        info!("Building core topology for commit {}", latest_commit);

        // 1. Parallel Map Phase: Scan files for structure and external references
        let files = if let Some(ref last) = last_commit {
            info!("Incremental update since {}", last);
            self.git.lock().get_changed_files_since(last)?
        } else {
            info!("Full repository scan");
            self.git.lock().get_all_tracked_files()?
        };

        if files.is_empty() {
            info!("No files to process.");
            self.graph.set_last_commit(latest_commit)?;
            return Ok(());
        }

        let lsp_sync_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Batch files into chunks to reduce task spawning overhead
        let files_per_batch = self.tuning.ingestion.files_per_batch;
        let max_files = self.tuning.ingestion.max_files_per_scan;
        let files_to_scan: Vec<_> = files.iter().take(max_files).cloned().collect();
        let file_chunks: Vec<Vec<PathBuf>> = files_to_scan
            .chunks(files_per_batch)
            .map(|chunk| chunk.to_vec())
            .collect();

        let mut set = tokio::task::JoinSet::new();
        for chunk in file_chunks {
            let lsp = self.lsp_pool.next();
            let workspace = self.config.workspace.clone();
            let commit_hash = latest_commit.clone();
            let git_time = latest_time;

            set.spawn(async move {
                scan_file_batch(chunk, workspace, lsp, lsp_sync_time, git_time, commit_hash).await
            });
        }

        // 2. Reduce Phase: Incremental flush — write partial results as tasks complete
        let mut batch_nodes = Vec::new();
        let mut batch_edges = Vec::new();
        let mut all_external_refs = Vec::new();
        let mut all_static_refs: Vec<StaticFileRef> = Vec::new();
        let mut all_pattern_refs: Vec<PatternRef> = Vec::new();
        let batch_size = self.tuning.ingestion.ingest_batch_size;

        let mut scanned = 0usize;
        let mut failed = 0usize;
        // True when this pass did NOT cover every changed file: either the
        // scan-phase timeout aborted the remaining tasks, or `max_files_per_scan`
        // capped the input. A partial pass must persist whatever it produced but
        // must NOT advance `set_last_commit` — otherwise the graph claims to be
        // current at HEAD while missing files, which is worse than being visibly
        // behind. See the guarded `set_last_commit` at the end of this function.
        let mut partial = files.len() > files_to_scan.len();
        if partial {
            warn!(
                "Scan capped at max_files_per_scan={} of {} changed files;                  this pass is partial and will not advance the indexed-commit marker",
                files_to_scan.len(),
                files.len()
            );
        }
        let scan_timeout = std::time::Duration::from_secs(self.tuning.ingestion.scan_timeout_secs);

        while let Some(res) = set.join_next().await {
            // Check timeout - abort remaining tasks and break
            if scan_start.elapsed() >= scan_timeout {
                warn!("Scan phase timed out after {:?}, aborting {} remaining tasks",
                      scan_timeout, set.len());
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
                    debug!("Batch completed: {} files scanned, {} failed in batch", scanned, failed);

                    // Incremental flush every batch_size files
                    if batch_nodes.len() >= batch_size {
                        info!("Flush phase 1: writing {} nodes ({} files scanned)", batch_nodes.len(), scanned);
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
                        if let Err(e) = self.graph.replace_nodes_for_paths(&paths, &batch_nodes) {
                            warn!("Batch node write error: {}", e);
                        }
                        if let Err(e) = self.graph.insert_edges_batch(&batch_edges) {
                            warn!("Batch edge write error: {}", e);
                        }
                        // Durably persist the flush. The in-memory inserts above
                        // are lost if the outer re-index timeout drops this task,
                        // which is why a 90s budget could never converge: every
                        // batch of parsing was discarded. Writing here means a
                        // killed run still leaves progress on disk and successive
                        // runs converge instead of restarting from the same graph.
                        if let Err(e) = self.graph.save_to_disk().await {
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
            if let Err(e) = self.graph.replace_nodes_for_paths(&paths, &batch_nodes) {
                warn!("Final batch node write error: {}", e);
            }
            if let Err(e) = self.graph.insert_edges_batch(&batch_edges) {
                warn!("Final batch edge write error: {}", e);
            }
        }

        info!("Scanned {} files, {} failed, collected {} external refs, {} static refs, {} pattern refs",
              scanned, failed, all_external_refs.len(), all_static_refs.len(), all_pattern_refs.len());

        // 3. Resolve Phase: Link external references to internal nodes (CALLS/USES)
        info!("Resolving topology: Linking {} external references...", all_external_refs.len());
        let call_edges =
            super::resolve::resolve_call_edges(&self.graph, &self.config.workspace, &all_external_refs);
        info!("Ingesting {} call edges", call_edges.len());
        self.graph.insert_edges_batch(&call_edges)?;

        // 3b. Static Resolve Phase: tree-sitter derived Calls/Uses edges
        info!("Resolving {} tree-sitter static references...", all_static_refs.len());
        let static_edges = super::resolve::resolve_static_edges(&self.graph, &all_static_refs);
        info!("Ingesting {} static tree-sitter edges", static_edges.len());
        self.graph.insert_edges_batch(&static_edges)?;

        // 3c. Pattern Resolve Phase: Cross-boundary semantic edges from string literals
        info!("Resolving {} pattern references for cross-boundary detection...", all_pattern_refs.len());
        let pattern_edges = super::resolve::resolve_pattern_edges(
            &self.graph,
            &all_pattern_refs,
            super::resolve::PatternLimits::DEFAULT,
        );
        info!("Ingesting {} cross-boundary pattern edges", pattern_edges.len());
        self.graph.insert_edges_batch(&pattern_edges)?;

        // 4. Temporal Analysis Phase: Co-changes
        let co_change_pairs = {
            let git = self.git.lock();
            git.analyze_co_changes(
                self.tuning.ingestion.cochange_commit_window,
                self.tuning.ingestion.cochange_min_pair_count,
                self.tuning.ingestion.cochange_max_commit_files,
            ).unwrap_or_default()
        };
        let co_change_tuples: Vec<_> = co_change_pairs.into_iter()
            .map(|p| (p.file1, p.file2, p.co_change_count))
            .collect();
        self.graph.insert_co_change_edges(&co_change_tuples)?;

        // 5. Enrichment Phase: Topological Algorithms (synchronous, fast)
        info!("Enriching topology: Calculating anchors and depths...");
        self.graph.calculate_anchor_scores()?;
        self.graph.calculate_depths()?;

        // 6. NLP Phase: Spawn lazy background enrichment (non-blocking)
        // Pre-warm top anchor nodes first so first semantic queries return quickly
        // Then queue the rest for background processing
        let graph_clone = self.graph.clone();
        let embedder_clone = self.embedder.clone();
        let nlp_prewarm_count = self.tuning.ingestion.nlp_prewarm_count;
        let nlp_batch_size = self.tuning.ingestion.nlp_batch_size;
        let nlp_budget_per_pass = self.tuning.ingestion.nlp_budget_per_pass;
        // The NLP pass runs detached, so it needs its own copy of the
        // workspace root to resolve workspace-relative node paths.
        let ws_for_nlp = self.config.workspace.clone();
        tokio::spawn(async move {
            let all_nodes = graph_clone.get_all_nodes();
            // Top anchors get embedded first (pre-warm)
            let mut anchors: Vec<_> = all_nodes.iter()
                .filter_map(|n| n.anchor_score.map(|s| (s, n.clone())))
                .collect();
            anchors.sort_by(|a, b| b.0.total_cmp(&a.0));

            let prewarm_count = anchors.len().min(nlp_prewarm_count);
            let (prewarm_nodes, rest_nodes) = anchors.split_at(prewarm_count);
            let prewarm: Vec<_> = prewarm_nodes.iter().map(|(_, n)| n.clone()).collect();
            let rest: Vec<_> = rest_nodes.iter().map(|(_, n)| n.clone()).collect();

            info!("NLP pre-warming {} anchor nodes...", prewarm.len());
            let mut count = 0;
            for node in &prewarm {
                if let Ok(Some(mut gn)) = graph_clone.get_node(&node.id) {
                    if gn.embedding.is_none() {
                        let text = crate::tools::utils::build_enriched_text(&gn, &ws_for_nlp);
                        if let Ok(emb) = embedder_clone.embed(&text) {
                            gn.embedding = Some(serde_json::to_string(&emb).unwrap_or_default());
                            if graph_clone.insert_node(&gn).is_ok() {
                                count += 1;
                            }
                        }
                    }
                }
            }
            info!("NLP pre-warm complete ({} embedded). Queuing {} remaining nodes.", count, rest.len());

            // Background lazy enrichment with backpressure
            let mut budget = nlp_budget_per_pass;
            for chunk in rest.chunks(nlp_batch_size) {
                if budget == 0 { break; }
                let to_embed: Vec<_> = chunk.iter().take(budget).cloned().collect();
                let batch_len = to_embed.len();
                for node in &to_embed {
                    if let Ok(Some(mut gn)) = graph_clone.get_node(&node.id) {
                        if gn.embedding.is_none() {
                            let text = crate::tools::utils::build_enriched_text(&gn, &ws_for_nlp);
                            if let Ok(emb) = embedder_clone.embed(&text) {
                                gn.embedding = Some(serde_json::to_string(&emb).unwrap_or_default());
                                let _ = graph_clone.insert_node(&gn);
                            }
                        }
                    }
                }
                budget = budget.saturating_sub(batch_len);
            }
            info!("NLP lazy enrichment pass complete.");
        });

        // Orphan sweep. Reclaims nodes whose file is no longer tracked: files
        // deleted or renamed outside this pass's view, and any backlog left by
        // builds that never deleted anything. Gated on a complete pass — after
        // a partial one, "not scanned this round" is indistinguishable from
        // "gone", and sweeping would delete live nodes.
        if !partial {
            match self.git.lock().get_all_tracked_files() {
                Ok(tracked_paths) => {
                    // Reduced with the same helper the scanner mints node paths
                    // with. Comparing git's absolute paths against relative node
                    // keys would mark every node an orphan, and that reads as a
                    // full sweep rather than as an error.
                    let tracked: HashSet<String> = tracked_paths
                        .iter()
                        .map(|p| crate::graph::graph_path(&self.config.workspace, p))
                        .collect();
                    if tracked.is_empty() {
                        warn!("Skipping orphan sweep: git reported no tracked files");
                    } else {
                        match self.graph.prune_orphans(&tracked) {
                            Ok(0) => info!("Orphan sweep: nothing to prune"),
                            Ok(n) => info!("Orphan sweep: pruned {n} nodes for untracked files"),
                            Err(e) => warn!("Orphan sweep failed: {e}"),
                        }
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
        if partial {
            warn!(
                "Partial index pass ({} files scanned, {} failed);                  leaving indexed-commit marker unchanged",
                scanned, failed
            );
        } else {
            self.graph.set_last_commit(latest_commit)?;
        }
        self.graph.save_to_disk().await?;

        // Bump the overlay freshness so the indexer doesn't read as
        // "stale" the moment the server comes up. The index path
        // doesn't insert through the overlay (it writes the static
        // graph), so without this touch every freshly-indexed server
        // would start with `Overlay freshness: stale`.
        self.overlay.touch();

        let duration = scan_start.elapsed();
        info!("Lain fully restored and ready in {:?}", duration);

        Ok(())
    }

    pub async fn sync_volatile_overlay(&mut self) -> Result<(), LainError> {
        // Sidecars populate their overlay from the owner's /overlay/subscribe
        // stream; they never re-scan the local working tree.
        if self.graph.is_read_only() {
            return Ok(());
        }
        self.overlay.clear();
        let changes = self.git.lock().get_uncommitted_changes()?;

        for change in &changes {
            if let Err(e) = self.process_change(&change.path).await {
                warn!("Failed to process change {:?}: {}", change.path, e);
            }
        }
        Ok(())
    }

    async fn process_change(&mut self, path: &Path) -> Result<(), LainError> {
        let symbols = {
            let lsp = self.lsp_pool.next();
            let mut lsp = lsp.lock().await;
            match lsp.get_document_symbols_hierarchical(path, &self.config.workspace).await {
                Ok(s) => s,
                Err(e) => {
                    debug!("No LSP symbols for changed file {:?}: {}", path, e);
                    return Ok(());
                }
            }
        };
        for symbol in symbols {
            self.overlay.insert_node(symbol.node.clone());
            // Broadcast the new node to any subscribed sidecar. The
            // read-only gate above (`is_read_only`) ensures this only
            // runs for owners.
            self.broadcast_overlay_insert(symbol.node);
        }
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
pub async fn index_one_repo(
    path: &Path,
    db: &GraphDatabase,
    lsp: &LspPool,
    git: &GitSensor,
    overlay: &VolatileOverlay,
) -> Result<(), LainError> {
    let scan_start = std::time::Instant::now();
    let (latest_commit, latest_time) = git.get_latest_commit_info()?;
    let last_commit = db.get_last_commit()?;

    if let Some(ref last) = last_commit {
        if last == &latest_commit {
            info!("[federation] {:?} already up to date at {}", path, last);
            return Ok(());
        }
    }

    info!("[federation] Building core topology for {:?} at commit {}", path, latest_commit);

    let files = if let Some(ref last) = last_commit {
        info!("[federation] Incremental update since {} for {:?}", last, path);
        git.get_changed_files_since(last)?
    } else {
        info!("[federation] Full repository scan for {:?}", path);
        git.get_all_tracked_files()?
    };

    if files.is_empty() {
        info!("[federation] No files to process for {:?}.", path);
        db.set_last_commit(latest_commit)?;
        db.save_to_disk_sync()?;
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
        let lsp_mux = lsp.next();
        let workspace = path.to_path_buf();
        let commit_hash = latest_commit.clone();
        let git_time = latest_time;

        set.spawn(async move {
            scan_file_batch(chunk, workspace, lsp_mux, lsp_sync_time, git_time, commit_hash).await
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
                    if let Err(e) = db.replace_nodes_for_paths(&paths, &batch_nodes) {
                        warn!("[federation] Batch node write error: {}", e);
                    }
                    if let Err(e) = db.insert_edges_batch(&batch_edges) {
                        warn!("[federation] Batch edge write error: {}", e);
                    }
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
        if let Err(e) = db.replace_nodes_for_paths(&paths, &batch_nodes) {
            warn!("[federation] Final batch node write error: {}", e);
        }
        if let Err(e) = db.insert_edges_batch(&batch_edges) {
            warn!("[federation] Final batch edge write error: {}", e);
        }
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
    let call_edges = super::resolve::resolve_call_edges(db, path, &all_external_refs);
    info!("[federation] {:?}: ingesting {} call edges", path, call_edges.len());
    db.insert_edges_batch(&call_edges)?;

    // Static resolve: tree-sitter derived Calls/Uses edges
    let static_edges = super::resolve::resolve_static_edges(db, &all_static_refs);
    info!("[federation] {:?}: ingesting {} static tree-sitter edges", path, static_edges.len());
    db.insert_edges_batch(&static_edges)?;

    // Pattern resolve: cross-boundary detection
    let pattern_edges = super::resolve::resolve_pattern_edges(
        db,
        &all_pattern_refs,
        super::resolve::PatternLimits::FEDERATION,
    );
    info!("[federation] {:?}: ingesting {} cross-boundary pattern edges", path, pattern_edges.len());
    db.insert_edges_batch(&pattern_edges)?;

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
    db.insert_co_change_edges(&co_change_tuples)?;

    // Enrichment: anchor scores + depths
    db.calculate_anchor_scores()?;
    db.calculate_depths()?;

    // Orphan sweep. This function has no scan-phase timeout and no
    // max-files cap, and the reduce loop always drains the JoinSet, so
    // reaching this point means the pass covered every changed file —
    // there is no partial case to gate on here, unlike the
    // single-workspace pipeline.
    match git.get_all_tracked_files() {
        Ok(tracked_paths) => {
            // Reduced with the same helper the scanner mints node paths with:
            // git returns absolute paths, and comparing those to relative node
            // keys marks every node an orphan rather than raising an error.
            let tracked: HashSet<String> = tracked_paths
                .iter()
                .map(|p| crate::graph::graph_path(path, p))
                .collect();
            if tracked.is_empty() {
                warn!("[federation] Skipping orphan sweep for {:?}: no tracked files", path);
            } else {
                match db.prune_orphans(&tracked) {
                    Ok(0) => info!("[federation] {:?}: orphan sweep found nothing", path),
                    Ok(n) => info!("[federation] {:?}: orphan sweep pruned {n} nodes", path),
                    Err(e) => warn!("[federation] {:?}: orphan sweep failed: {e}", path),
                }
            }
        }
        Err(e) => warn!("[federation] Skipping orphan sweep for {:?}: {e}", path),
    }

    db.set_last_commit(latest_commit)?;
    db.save_to_disk_sync()?;

    info!(
        "[federation] {:?}: fully indexed in {:?}",
        path,
        scan_start.elapsed()
    );
    overlay.touch();
    Ok(())
}
