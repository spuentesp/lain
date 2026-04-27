use crate::schema::{GraphEdge, NodeType, EdgeType};
use crate::server::LainServer;
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

impl LainServer {
    /// Run periodic sync every interval_seconds
    pub async fn run_background_sync(&self, interval_secs: u64) {
        let interval = tokio::time::Duration::from_secs(interval_secs);
        loop {
            tokio::time::sleep(interval).await;
            info!("Background sync: checking for updates...");
            let commit = self.git.lock().get_latest_commit_info()
                .map(|(commit, _)| commit)
                .inspect_err(|e| warn!("Background sync: failed to get commit info: {}", e))
                .ok();
            if let Some(commit) = commit {
                if let Ok(Some(last)) = self.graph.get_last_commit() {
                    if last != commit {
                        info!("Background sync: new commits detected, triggering sync");
                        let mut s = self.clone();
                        if let Err(e) = s.build_core_memory().await {
                            warn!("Background sync failed: {}", e);
                        }
                    } else {
                        debug!("Background sync: already up to date");
                    }
                }
            }
        }
    }

    /// Sliding window background task: dirty-first traversal with backpressure.
    /// Priority: overlay (dirty files) → LSP symbols → tree-sitter edges → queue NLP.
    /// Non-blocking at every phase — never stalls the hot path.
    pub async fn run_sliding_window(&self, interval_secs: u64) {
        let interval = tokio::time::Duration::from_secs(interval_secs);
        info!("Sliding window background task started (interval: {}s)", interval_secs);
        loop {
            tokio::time::sleep(interval).await;
            let changes = match self.git.lock().get_uncommitted_changes() {
                Ok(c) => c,
                Err(e) => {
                    warn!("Sliding window: failed to get uncommitted changes: {}", e);
                    continue;
                }
            };

            if changes.is_empty() {
                debug!("Sliding window: no dirty files");
                continue;
            }

            // Dirty-first: prioritize changed files, cap at 20 per pass (backpressure)
            const MAX_DIRTY_PER_PASS: usize = 20;
            const MAX_EDGES_PER_PASS: usize = 100;
            const MAX_EMBED_PER_PASS: usize = 30;

            let dirty_paths: Vec<_> = changes.iter()
                .map(|c| c.path.clone())
                .take(MAX_DIRTY_PER_PASS)
                .collect();

            debug!("Sliding window: {} dirty files (max {}), edge budget {}, embed budget {}",
                dirty_paths.len(), MAX_DIRTY_PER_PASS, MAX_EDGES_PER_PASS, MAX_EMBED_PER_PASS);

            // ── Phase 1: LSP symbols (dirty-first, early exit on budget) ─────
            let mut refreshed_ids: Vec<String> = Vec::new();
            let mut new_edges: Vec<GraphEdge> = Vec::new();
            let mut seen: HashSet<(String, String)> = HashSet::new();
            let mut edge_count = 0usize;

            for path in &dirty_paths {
                if edge_count >= MAX_EDGES_PER_PASS { break; }
                let lsp = self.lsp_pool.next();
                let mut lsp = lsp.lock().await;

                match lsp.get_document_symbols_hierarchical(path).await {
                    Ok(symbols) => {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;
                        for symbol in symbols {
                            let mut node = symbol.node;
                            node.last_lsp_sync = Some(now);
                            refreshed_ids.push(node.id.clone());
                            self.overlay.insert_node(node);
                        }
                        // LSP references for Call edges
                        let path_str = path.to_string_lossy();
                        if let Ok(file_refs) = lsp.get_references(path, 0, 0).await {
                            for ref_loc in file_refs {
                                if edge_count >= MAX_EDGES_PER_PASS { break; }
                                let ref_path_str = ref_loc.path.to_string_lossy().to_string();
                                let source = self.graph.get_node_at_location(&path_str, ref_loc.line);
                                let target = self.graph.get_node_at_location(&ref_path_str, ref_loc.line);
                            if let (Some(s), Some(t)) = (source, target) {
                                if s.id != t.id {
                                    let key = (s.id.clone(), t.id.clone());
                                    if seen.insert(key) {
                                        new_edges.push(GraphEdge::new(EdgeType::Calls, s.id.clone(), t.id));
                                        edge_count += 1;
                                    }
                                }
                            }
                            }
                        }
                    }
                    Err(_) => { /* skip, tree-sitter fallback below */ }
                }
            }

            // ── Phase 2: Tree-sitter (offline, no LSP needed) ─────────────────
            let all_graph_nodes = self.graph.get_all_nodes();
            let name_index: HashMap<String, Vec<(String, NodeType)>> = all_graph_nodes
                .iter()
                .fold(HashMap::new(), |mut acc, n| {
                    acc.entry(n.name.clone())
                        .or_default()
                        .push((n.id.clone(), n.node_type.clone()));
                    acc
                });

            for path in &dirty_paths {
                if edge_count >= MAX_EDGES_PER_PASS { break; }
                if let Ok(content) = tokio::fs::read_to_string(path).await {
                    let file_path_str = path.to_string_lossy().to_string();
                    if let Some(source) = all_graph_nodes.iter().find(|n| n.path == file_path_str) {
                        for r in crate::treesitter::extract_refs(path, &content) {
                            if edge_count >= MAX_EDGES_PER_PASS { break; }
                            if let Some(candidates) = name_index.get(&r.target_name) {
                                for (target_id, target_type) in candidates {
                                    if *target_id == source.id { continue; }
                                    if matches!(r.edge_type, EdgeType::Uses)
                                        && !matches!(target_type,
                                            NodeType::Struct | NodeType::Enum | NodeType::Trait
                                            | NodeType::Class | NodeType::Interface)
                                    {
                                        continue;
                                    }
                                    let key = (source.id.clone(), target_id.clone());
                                    if seen.insert(key) {
                                        new_edges.push(GraphEdge::new(r.edge_type.clone(), source.id.clone(), target_id.clone()));
                                        edge_count += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Persist edges immediately (edge insert is cheap, non-blocking)
            if !new_edges.is_empty() {
                if let Err(e) = self.graph.insert_edges_batch(&new_edges) {
                    warn!("Sliding window: edge insert failed: {}", e);
                } else {
                    debug!("Sliding window: {} new edges from {} files", new_edges.len(), dirty_paths.len());
                }
            }

            // ── Phase 3: Lazy NLP embed (background, not on hot path) ─────────
            // Queue embeddings to background task instead of computing inline
            if !refreshed_ids.is_empty() {
                let graph_clone = self.graph.clone();
                let embedder_clone = self.embedder.clone();
                let ids = refreshed_ids.clone();
                tokio::spawn(async move {
                    let mut count = 0;
                    for id in ids.iter().take(MAX_EMBED_PER_PASS) {
                        if let Ok(Some(mut gn)) = graph_clone.get_node(id) {
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
                    if count > 0 {
                        debug!("Sliding window: lazily embedded {} nodes", count);
                    }
                });
            }

            // Incremental persist (cheap)
            self.graph.save_to_disk().await.ok();
        }
    }
}
