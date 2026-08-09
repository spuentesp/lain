use crate::error::LainError;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::schema::{GraphEdge, GraphNode, NodeType, EdgeType};
use crate::server::LainServer;
use crate::server::scan::{scan_file_batch, StaticFileRef, PatternRef};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

impl LainServer {
    /// The "Sane" Ingestion Pipeline: Map -> Reduce -> Resolve -> Enrich
    pub async fn build_core_memory(&mut self) -> Result<(), LainError> {
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
        let scan_timeout = std::time::Duration::from_secs(self.tuning.ingestion.scan_timeout_secs);

        while let Some(res) = set.join_next().await {
            // Check timeout - abort remaining tasks and break
            if scan_start.elapsed() >= scan_timeout {
                warn!("Scan phase timed out after {:?}, aborting {} remaining tasks",
                      scan_timeout, set.len());
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
                        if let Err(e) = self.graph.insert_nodes_batch(&batch_nodes) {
                            warn!("Batch node write error: {}", e);
                        }
                        if let Err(e) = self.graph.insert_edges_batch(&batch_edges) {
                            warn!("Batch edge write error: {}", e);
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
            if let Err(e) = self.graph.insert_nodes_batch(&batch_nodes) {
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
        let mut call_edges = Vec::new();
        for (source_id, ref_loc) in all_external_refs {
            let path_str = ref_loc.path.to_string_lossy().to_string();
            if let Some(target_node) = self.graph.get_node_at_location(&path_str, ref_loc.line) {
                if target_node.id != source_id {
                    call_edges.push(GraphEdge::new(EdgeType::Calls, source_id, target_node.id));
                }
            }
        }
        info!("Ingesting {} call edges", call_edges.len());
        self.graph.insert_edges_batch(&call_edges)?;

        // 3b. Static Resolve Phase: tree-sitter derived Calls/Uses edges
        info!("Resolving {} tree-sitter static references...", all_static_refs.len());
        {
            // Build name → node IDs index for O(1) target resolution
            let mut name_index: HashMap<String, Vec<(String, NodeType)>> = HashMap::new();
            for node in self.graph.get_all_nodes() {
                name_index
                    .entry(node.name.clone())
                    .or_default()
                    .push((node.id.clone(), node.node_type.clone()));
            }

            let mut static_edges: Vec<GraphEdge> = Vec::new();
            let mut seen: HashSet<(String, String)> = HashSet::new();

            for sr in all_static_refs {
                let Some(source_node) =
                    self.graph.get_node_at_location(&sr.file_path, sr.source_line)
                else {
                    continue;
                };

                let Some(candidates) = name_index.get(&sr.target_name) else {
                    continue;
                };

                for (target_id, target_type) in candidates {
                    if *target_id == source_node.id {
                        continue; // no self-edges
                    }
                    // Uses edges only towards type-level nodes
                    if matches!(sr.edge_type, EdgeType::Uses)
                        && !matches!(
                            target_type,
                            NodeType::Struct
                                | NodeType::Enum
                                | NodeType::Trait
                                | NodeType::Class
                                | NodeType::Interface
                        )
                    {
                        continue;
                    }
                    let key = (source_node.id.clone(), target_id.clone());
                    if seen.insert(key) {
                        static_edges.push(GraphEdge::new(
                            sr.edge_type.clone(),
                            source_node.id.clone(),
                            target_id.clone(),
                        ));
                    }
                }
            }

            info!("Ingesting {} static tree-sitter edges", static_edges.len());
            self.graph.insert_edges_batch(&static_edges)?;
        }

        // 3c. Pattern Resolve Phase: Cross-boundary semantic edges from string literals
        info!("Resolving {} pattern references for cross-boundary detection...", all_pattern_refs.len());
        {
            use std::collections::HashMap as Map;
            // Pre-compute file path → node ID for O(1) lookups
            let file_nodes: Map<String, GraphNode> = self.graph.get_all_nodes()
                .into_iter()
                .filter(|n| matches!(n.node_type, NodeType::File))
                .map(|n| (n.path.clone(), n))
                .collect();

            // Group by pattern value: pattern_value -> list of file paths that reference it
            let mut value_to_files: Map<String, Vec<String>> = Map::new();
            for pr in &all_pattern_refs {
                // Deduplicate by file path - multiple refs from same file count as one
                let entry = value_to_files.entry(pr.value.clone()).or_default();
                if !entry.contains(&pr.file_path) {
                    entry.push(pr.file_path.clone());
                }
            }

            // Score patterns: count cross-directory pairs. Higher = more interesting.
            // Only keep patterns with 2-20 references (too few = noise, too many = common libs)
            let mut scored: Vec<(usize, String, Vec<String>)> = Vec::new();
            for (value, files) in value_to_files {
                if files.len() < 2 || files.len() > 20 {
                    continue;
                }
                // Count unique directories
                let mut dirs: HashSet<String> = HashSet::new();
                for f in &files {
                    if let Some(parent) = std::path::Path::new(f).parent() {
                        dirs.insert(parent.to_string_lossy().to_string());
                    }
                }
                if dirs.len() < 2 {
                    continue;
                }
                // Score = number of directory pairs (combinatorial size of coupling)
                let pairs = dirs.len() * (dirs.len() - 1) / 2;
                scored.push((pairs, value, files));
            }

            // Sort by score descending; take top patterns until edge budget exhausted
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            // Proportional cap: 10 edges per pattern, max 200. Prevents combinatorial blowup while scaling with pattern count.
            let max_pattern_edges = (scored.len() * 10).min(200);
            let mut pattern_edges: Vec<GraphEdge> = Vec::new();
            let mut seen: HashSet<(String, String)> = HashSet::new();

            for (_score, _value, files) in scored {
                if pattern_edges.len() >= max_pattern_edges {
                    break;
                }
                // Group by directory, pick one representative file per dir
                let mut dirs: Map<String, String> = Map::new(); // dir -> representative file
                for f in &files {
                    if let Some(parent) = std::path::Path::new(f).parent() {
                        let parent_str = parent.to_string_lossy().to_string();
                        dirs.entry(parent_str).or_insert_with(|| f.clone());
                    }
                }
                let all_dirs: Vec<_> = dirs.into_iter().collect();
                // Connect representative files across directory pairs (1 edge per pair)
                for i in 0..all_dirs.len() {
                    if pattern_edges.len() >= max_pattern_edges {
                        break;
                    }
                    for j in (i + 1)..all_dirs.len() {
                        let (_dir_a, file_a) = &all_dirs[i];
                        let (_dir_b, file_b) = &all_dirs[j];
                        let key = (file_a.clone(), file_b.clone());
                        if seen.insert(key) {
                            if let (Some(node_a), Some(node_b)) = (
                                file_nodes.get(file_a),
                                file_nodes.get(file_b),
                            ) {
                                pattern_edges.push(GraphEdge::new(EdgeType::Pattern, node_a.id.clone(), node_b.id.clone()));
                            }
                        }
                        if pattern_edges.len() >= max_pattern_edges {
                            break;
                        }
                    }
                }
            }

            info!("Ingesting {} cross-boundary pattern edges", pattern_edges.len());
            self.graph.insert_edges_batch(&pattern_edges)?;
        }

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
                        let text = crate::tools::utils::build_enriched_text(&gn);
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
                            let text = crate::tools::utils::build_enriched_text(&gn);
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

        self.graph.set_last_commit(latest_commit)?;
        self.graph.save_to_disk().await?;

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
            match lsp.get_document_symbols_hierarchical(path).await {
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
    const PATTERN_MAX_REF_COUNT: usize = 20;
    const PATTERN_MIN_DIRS: usize = 2;
    const PATTERN_MAX_EDGES: usize = 200;
    const PATTERN_EDGES_PER_VALUE: usize = 10;

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
                    if let Err(e) = db.insert_nodes_batch(&batch_nodes) {
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
        if let Err(e) = db.insert_nodes_batch(&batch_nodes) {
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
    let mut call_edges: Vec<GraphEdge> = Vec::new();
    for (source_id, ref_loc) in all_external_refs {
        let path_str = ref_loc.path.to_string_lossy().to_string();
        if let Some(target_node) = db.get_node_at_location(&path_str, ref_loc.line) {
            if target_node.id != source_id {
                call_edges.push(GraphEdge::new(EdgeType::Calls, source_id, target_node.id));
            }
        }
    }
    info!("[federation] {:?}: ingesting {} call edges", path, call_edges.len());
    db.insert_edges_batch(&call_edges)?;

    // Static resolve: tree-sitter derived Calls/Uses edges
    {
        let mut name_index: HashMap<String, Vec<(String, NodeType)>> = HashMap::new();
        for node in db.get_all_nodes() {
            name_index
                .entry(node.name.clone())
                .or_default()
                .push((node.id.clone(), node.node_type.clone()));
        }

        let mut static_edges: Vec<GraphEdge> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();

        for sr in all_static_refs {
            let Some(source_node) = db.get_node_at_location(&sr.file_path, sr.source_line) else {
                continue;
            };
            let Some(candidates) = name_index.get(&sr.target_name) else {
                continue;
            };
            for (target_id, target_type) in candidates {
                if *target_id == source_node.id {
                    continue;
                }
                if matches!(sr.edge_type, EdgeType::Uses)
                    && !matches!(
                        target_type,
                        NodeType::Struct
                            | NodeType::Enum
                            | NodeType::Trait
                            | NodeType::Class
                            | NodeType::Interface
                    )
                {
                    continue;
                }
                let key = (source_node.id.clone(), target_id.clone());
                if seen.insert(key) {
                    static_edges.push(GraphEdge::new(
                        sr.edge_type.clone(),
                        source_node.id.clone(),
                        target_id.clone(),
                    ));
                }
            }
        }
        info!("[federation] {:?}: ingesting {} static tree-sitter edges", path, static_edges.len());
        db.insert_edges_batch(&static_edges)?;
    }

    // Pattern resolve: cross-boundary detection
    {
        let file_nodes: HashMap<String, GraphNode> = db.get_all_nodes()
            .into_iter()
            .filter(|n| matches!(n.node_type, NodeType::File))
            .map(|n| (n.path.clone(), n))
            .collect();

        let mut value_to_files: HashMap<String, Vec<String>> = HashMap::new();
        for pr in &all_pattern_refs {
            let entry = value_to_files.entry(pr.value.clone()).or_default();
            if !entry.contains(&pr.file_path) {
                entry.push(pr.file_path.clone());
            }
        }

        let mut scored: Vec<(usize, String, Vec<String>)> = Vec::new();
        for (value, files) in value_to_files {
            if files.len() < 2 || files.len() > PATTERN_MAX_REF_COUNT {
                continue;
            }
            let mut dirs: HashSet<String> = HashSet::new();
            for f in &files {
                if let Some(parent) = std::path::Path::new(f).parent() {
                    dirs.insert(parent.to_string_lossy().to_string());
                }
            }
            if dirs.len() < PATTERN_MIN_DIRS {
                continue;
            }
            let pairs = dirs.len() * (dirs.len() - 1) / 2;
            scored.push((pairs, value, files));
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0));

        let max_pattern_edges = (scored.len() * PATTERN_EDGES_PER_VALUE).min(PATTERN_MAX_EDGES);
        let mut pattern_edges: Vec<GraphEdge> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();

        for (_score, _value, files) in scored {
            if pattern_edges.len() >= max_pattern_edges {
                break;
            }
            let mut dirs: HashMap<String, String> = HashMap::new();
            for f in &files {
                if let Some(parent) = std::path::Path::new(f).parent() {
                    let parent_str = parent.to_string_lossy().to_string();
                    dirs.entry(parent_str).or_insert_with(|| f.clone());
                }
            }
            let all_dirs: Vec<_> = dirs.into_iter().collect();
            for i in 0..all_dirs.len() {
                if pattern_edges.len() >= max_pattern_edges {
                    break;
                }
                for j in (i + 1)..all_dirs.len() {
                    let (_dir_a, file_a) = &all_dirs[i];
                    let (_dir_b, file_b) = &all_dirs[j];
                    let key = (file_a.clone(), file_b.clone());
                    if seen.insert(key) {
                        if let (Some(node_a), Some(node_b)) = (
                            file_nodes.get(file_a),
                            file_nodes.get(file_b),
                        ) {
                            pattern_edges.push(GraphEdge::new(
                                EdgeType::Pattern,
                                node_a.id.clone(),
                                node_b.id.clone(),
                            ));
                        }
                    }
                    if pattern_edges.len() >= max_pattern_edges {
                        break;
                    }
                }
            }
        }

        info!("[federation] {:?}: ingesting {} cross-boundary pattern edges", path, pattern_edges.len());
        db.insert_edges_batch(&pattern_edges)?;
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
    db.insert_co_change_edges(&co_change_tuples)?;

    // Enrichment: anchor scores + depths
    db.calculate_anchor_scores()?;
    db.calculate_depths()?;

    db.set_last_commit(latest_commit)?;
    db.save_to_disk_sync()?;

    info!(
        "[federation] {:?}: fully indexed in {:?}",
        path,
        scan_start.elapsed()
    );
    Ok(())
}
