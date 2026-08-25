//! Query executor for graph operations

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::query::spec::{
    ConnectOp, Direction, EdgeSelector, FilterOp, GraphNodeRef, GraphPath, GraphEdgeRef,
    GroupBy, GroupOp, LimitOp, QueryGroup, QueryMeta, QueryMode,
    QueryResult, QuerySpec, SemanticFilterOp, SortDirection, SortField, SortOp,
    FindOp,
};
use crate::tools::utils::{build_enriched_text, cosine_similarity};
use petgraph::Direction as PetDirection;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Instant;

/// Executor for running queries against the graph
pub struct Executor<'a> {
    graph: &'a GraphDatabase,
    embedder: &'a NlpEmbedder,
    embedding_cache: &'a Arc<Mutex<HashMap<String, Vec<f32>>>>,
    /// Workspace root. Node paths are workspace-relative graph keys, so
    /// on-demand embedding needs this to read a symbol's body off disk.
    workspace: &'a std::path::Path,
    nodes_visited: usize,
    /// Cap applied when a query specifies no `limit` of its own.
    /// 0 disables the cap.
    default_limit: usize,
}

impl<'a> Executor<'a> {
    pub fn new(
        graph: &'a GraphDatabase,
        embedder: &'a NlpEmbedder,
        embedding_cache: &'a Arc<Mutex<HashMap<String, Vec<f32>>>>,
        workspace: &'a std::path::Path,
    ) -> Self {
        Self::with_default_limit(
            graph,
            embedder,
            embedding_cache,
            workspace,
            crate::tuning::IngestionConfig::default().default_query_limit,
        )
    }

    /// Like [`Self::new`] but with an explicit default result cap, so
    /// callers holding a `TuningConfig` can honour
    /// `ingestion.default_query_limit`.
    pub fn with_default_limit(
        graph: &'a GraphDatabase,
        embedder: &'a NlpEmbedder,
        embedding_cache: &'a Arc<Mutex<HashMap<String, Vec<f32>>>>,
        workspace: &'a std::path::Path,
        default_limit: usize,
    ) -> Self {
        Self {
            graph,
            embedder,
            embedding_cache,
            workspace,
            nodes_visited: 0,
            default_limit,
        }
    }

    /// Execute a query spec and return results
    ///
    /// `spec.mode` is honoured here. It used to be deserialized from the
    /// caller's JSON and then never read by anything: an agent could send
    /// `{"mode":"tool"}` — documented in `docs/query-language.md` as
    /// "delegate to legacy named tool handlers" — and silently get
    /// auto-mode behaviour instead, with nothing to indicate the option
    /// had no effect. An advertised knob that does nothing is the same
    /// failure as an advertised edge type nothing produces.
    pub fn execute(&mut self, spec: &QuerySpec) -> Result<QueryResult, LainError> {
        let start = Instant::now();

        match spec.mode {
            // Named handler only. Asking for it without naming one is a
            // mistake worth reporting rather than quietly running the ops.
            QueryMode::Tool => {
                let Some(name) = &spec.named else {
                    return Err(LainError::Mcp(
                        "mode \"tool\" runs a named query, but no `named` was given; \
                         set `named`, or use mode \"query\" to run the ops array"
                            .to_string(),
                    ));
                };
                return self.execute_named(name);
            }
            // Ops only. `named` alongside it is contradictory input.
            QueryMode::Query => {
                if spec.named.is_some() {
                    return Err(LainError::Mcp(
                        "mode \"query\" runs the ops array, but `named` was also given; \
                         drop one of them, or use mode \"auto\""
                            .to_string(),
                    ));
                }
            }
            // Documented as "try ops first, fallback to named"; in
            // practice a spec carries one or the other, and a named
            // query wins when both are somehow present.
            QueryMode::Auto => {
                if let Some(name) = &spec.named {
                    return self.execute_named(name);
                }
            }
        }

        let mut current_nodes: Vec<GraphNodeRef> = Vec::new();
        let mut current_edges: Vec<GraphEdgeRef> = Vec::new();
        let mut current_paths: Vec<GraphPath> = Vec::new();
        let mut groups: Option<Vec<QueryGroup>> = None;

        for op in &spec.ops {
            match op {
                crate::query::spec::GraphOp::Find(find) => {
                    current_nodes = self.execute_find(find)?;
                    current_edges.clear();
                    current_paths.clear();
                }
                crate::query::spec::GraphOp::Connect(connect) => {
                    let (nodes, edges, paths) = self.execute_connect(&current_nodes, connect)?;
                    current_nodes = nodes;
                    current_edges = edges;
                    current_paths = paths;
                }
                crate::query::spec::GraphOp::Filter(filter) => {
                    self.apply_filter(&mut current_nodes, filter);
                }
                crate::query::spec::GraphOp::SemanticFilter(sem) => {
                    self.apply_semantic_filter(&mut current_nodes, sem)?;
                }
                crate::query::spec::GraphOp::Group(group) => {
                    groups = Some(self.apply_group(&current_nodes, group));
                }
                crate::query::spec::GraphOp::Sort(sort) => {
                    self.apply_sort(&mut current_nodes, sort);
                }
                crate::query::spec::GraphOp::Limit(limit) => {
                    self.apply_limit(&mut current_nodes, limit);
                    self.apply_limit_edges(&mut current_edges, limit);
                }
            }
        }

        // Apply the configured default cap when the query did not set its
        // own `limit`. `ingestion.default_query_limit` is documented as
        // "Default query result limit when not specified" and was never
        // read, so `{"op":"find","type":"Function"}` returned every match
        // — on this repo alone that is well over a thousand nodes into the
        // caller's context. The trim is reported in `meta` rather than
        // applied silently.
        let asked_for_a_limit = spec
            .ops
            .iter()
            .any(|op| matches!(op, crate::query::spec::GraphOp::Limit(_)));
        let matched = current_nodes.len();
        let mut truncated = false;
        if !asked_for_a_limit && self.default_limit > 0 && matched > self.default_limit {
            current_nodes.truncate(self.default_limit);
            truncated = true;
        }

        let exec_us = start.elapsed().as_micros() as u64;
        let count = current_nodes.len();

        Ok(QueryResult {
            nodes: current_nodes,
            edges: current_edges,
            paths: current_paths,
            count,
            legacy: false,
            meta: Some(QueryMeta {
                exec_us,
                nodes_visited: self.nodes_visited,
                plan: None,
                truncated,
                matched_before_limit: truncated.then_some(matched),
            }),
            groups,
        })
    }

    fn execute_named(&mut self, name: &str) -> Result<QueryResult, LainError> {
        let spec = QuerySpec::named(name)
            .ok_or_else(|| LainError::NotFound(format!("Unknown named query: {}", name)))?;
        let mut result = self.execute(&spec)?;
        result.legacy = true;
        Ok(result)
    }

    fn execute_find(&mut self, find: &FindOp) -> Result<Vec<GraphNodeRef>, LainError> {
        let nodes = self.graph.query_nodes(
            find.type_selector.as_ref(),
            find.name.as_ref(),
            find.label_selector.as_ref(),
            find.path.as_deref(),
        );

        self.nodes_visited += nodes.len();

        let results = nodes
            .into_iter()
            .map(|n| GraphNodeRef {
                id: n.id.clone(),
                node_type: n.node_type.to_string(),
                name: n.name.clone(),
                label: if n.is_deprecated { Some("deprecated".into()) } else { None },
            })
            .collect();

        Ok(results)
    }

    fn execute_connect(
        &mut self,
        start_nodes: &[GraphNodeRef],
        connect: &ConnectOp,
    ) -> Result<(Vec<GraphNodeRef>, Vec<GraphEdgeRef>, Vec<GraphPath>), LainError> {
        if start_nodes.is_empty() {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }

        let depth_range = connect.depth.to_range();

        // Handle Direction::Both by traversing both directions and merging
        let directions: Vec<PetDirection> = match connect.direction {
            Direction::Both => vec![PetDirection::Outgoing, PetDirection::Incoming],
            _ => vec![connect.direction.into()],
        };

        let mut found_nodes = Vec::new();
        let mut found_edges = Vec::new();
        let mut found_paths = Vec::new();

        for direction in directions {
            for start_node in start_nodes {
                let (nodes, edges, paths) = self.bfs_traverse(
                    &start_node.id,
                    &connect.edge,
                    depth_range.clone(),
                    direction,
                )?;

                found_nodes.extend(nodes);
                found_edges.extend(edges);
                found_paths.extend(paths);
            }
        }

        let mut unique_ids = HashMap::new();
        found_nodes.retain(|n| unique_ids.insert(n.id.clone(), true).is_none());

        self.nodes_visited += found_nodes.len();

        Ok((found_nodes, found_edges, found_paths))
    }

    fn bfs_traverse(
        &mut self,
        start_id: &str,
        edge_selector: &EdgeSelector,
        depth_range: RangeInclusive<u32>,
        direction: PetDirection,
    ) -> Result<(Vec<GraphNodeRef>, Vec<GraphEdgeRef>, Vec<GraphPath>), LainError> {
        // Reject an edge name that is not a real EdgeType rather than
        // traversing and returning nothing. A silent empty answer reads
        // as "no such relationship in this codebase" when it actually
        // means "you named an edge that does not exist".
        let unknown = edge_selector.unknown_types();
        if !unknown.is_empty() {
            let mut valid: Vec<String> = crate::server::schema::EdgeType::all()
                .iter()
                .map(|e| e.to_string())
                .collect();
            valid.sort();
            return Err(LainError::Mcp(format!(
                "unknown edge type(s) {}: valid edge types are {}",
                unknown.join(", "),
                valid.join(", ")
            )));
        }

        let mut found_nodes = Vec::new();

        let mut visited = HashMap::new();
        let mut queue: Vec<(String, Vec<String>, Vec<(String, String)>)> = vec![(start_id.into(), vec![start_id.into()], vec![])];

        while let Some((current_id, path_ids, path_edges)) = queue.pop() {
            let current_depth = path_ids.len() - 1;

            if depth_range.contains(&(current_depth as u32)) && path_ids.len() > 1 {
                if let Some(node) = self.graph.get_node(&current_id)? {
                    found_nodes.push(GraphNodeRef {
                        id: node.id.clone(),
                        node_type: node.node_type.to_string(),
                        name: node.name.clone(),
                        label: if node.is_deprecated { Some("deprecated".into()) } else { None },
                    });
                }
            }

            // Keep walking while the next hop is within `max`. This used
            // to require the next depth to be *inside* the whole range,
            // which conflates "which depths do I report" with "how far do
            // I walk". Reaching depth `min` means passing through depths
            // 1..min — exactly the ones a `min > 1` range excludes — so
            // the walk stopped at the start node and any query with
            // `{"depth":{"min":2,...}}` returned nothing at all. That form
            // is documented in `docs/query-language.md`, and it answered
            // empty rather than erroring, so it read as "no such
            // relationship" instead of "this never worked".
            if (current_depth as u32).saturating_add(1) > *depth_range.end() {
                continue;
            }

            let neighbors = self.graph.get_neighbors(&current_id, direction.into());

            for (neighbor, edge) in neighbors {
                if !edge_selector.matches(&edge.edge_type.to_string()) {
                    continue;
                }

                if visited.contains_key(&neighbor.id) {
                    continue;
                }
                visited.insert(neighbor.id.clone(), true);

                let edge_key = (edge.source_id.clone(), edge.target_id.clone());
                let mut new_path_ids = path_ids.clone();
                new_path_ids.push(neighbor.id.clone());
                let mut new_path_edges = path_edges.clone();
                new_path_edges.push(edge_key);

                queue.push((neighbor.id, new_path_ids, new_path_edges));
            }
        }

        Ok((found_nodes, Vec::new(), Vec::new()))
    }

    fn apply_filter(&self, nodes: &mut Vec<GraphNodeRef>, filter: &FilterOp) {
        nodes.retain(|n| {
            if let Some(ref type_sel) = filter.type_filter {
                if !type_sel.matches(&n.node_type) {
                    return false;
                }
            }

            if let Some(ref label_sel) = filter.label_filter {
                if !label_sel.matches(n.label.as_deref()) {
                    return false;
                }
            }

            if let Some(ref name_sel) = filter.name {
                if !name_sel.matches(&n.name) {
                    return false;
                }
            }

            true
        });
    }

    fn apply_semantic_filter(
        &self,
        nodes: &mut Vec<GraphNodeRef>,
        sem: &SemanticFilterOp,
    ) -> Result<(), LainError> {
        if nodes.is_empty() {
            return Ok(());
        }

        // Embed the query once. `embed_query`, not `embed`: this is a
        // user query and must carry the configured prefix, which this
        // call site silently omitted.
        let query_emb = self.embedder.embed_query(&sem.like)
            .map_err(|e| LainError::Nlp(format!("Failed to embed query: {}", e)))?;

        // Get full graph nodes to access their embeddings
        let all_graph_nodes: HashMap<String, crate::schema::GraphNode> = self.graph
            .get_all_nodes()
            .into_iter()
            .map(|n| (n.id.clone(), n))
            .collect();

        nodes.retain(|node_ref| {
            let full_node = match all_graph_nodes.get(&node_ref.id) {
                Some(n) => n,
                None => return false,
            };

            let node_emb = self.get_node_embedding(full_node);
            if let Some(emb) = node_emb {
                cosine_similarity(&query_emb, &emb) > sem.threshold
            } else {
                false
            }
        });

        Ok(())
    }

    fn get_node_embedding(&self, node: &crate::schema::GraphNode) -> Option<Vec<f32>> {
        // Check cache first
        if let Some(emb) = self.embedding_cache.lock().get(&node.id).cloned() {
            return Some(emb);
        }

        // Check stored embedding
        if let Some(ref e_json) = node.embedding {
            if let Ok(emb) = serde_json::from_str::<Vec<f32>>(e_json) {
                self.embedding_cache.lock().insert(node.id.clone(), emb.clone());
                return Some(emb);
            }
        }

        // On-demand embed
        let text = build_enriched_text(node, self.workspace);
        self.embedder.embed(&text).ok().map(|emb| {
            self.embedding_cache.lock().insert(node.id.clone(), emb.clone());
            emb
        })
    }

    fn apply_group(&self, nodes: &[GraphNodeRef], group: &GroupOp) -> Vec<QueryGroup> {
        let mut groups: HashMap<String, Vec<GraphNodeRef>> = HashMap::new();

        for node in nodes {
            let key = match group.by {
                GroupBy::Type => node.node_type.clone(),
                GroupBy::Label => node.label.clone().unwrap_or_default(),
                GroupBy::Name => node.name.clone(),
            };
            groups.entry(key).or_default().push(node.clone());
        }

        groups
            .into_iter()
            .map(|(key, nodes)| QueryGroup {
                key,
                count: nodes.len(),
                nodes,
            })
            .collect()
    }

    fn apply_sort(&self, nodes: &mut Vec<GraphNodeRef>, sort: &SortOp) {
        let cmp = match (sort.by, sort.direction) {
            (SortField::Name, SortDirection::Asc) => |a: &GraphNodeRef, b: &GraphNodeRef| a.name.cmp(&b.name),
            (SortField::Name, SortDirection::Desc) => |a: &GraphNodeRef, b: &GraphNodeRef| b.name.cmp(&a.name),
            (SortField::Type, SortDirection::Asc) => |a: &GraphNodeRef, b: &GraphNodeRef| a.node_type.cmp(&b.node_type),
            (SortField::Type, SortDirection::Desc) => |a: &GraphNodeRef, b: &GraphNodeRef| b.node_type.cmp(&a.node_type),
            (SortField::Label, SortDirection::Asc) => |a: &GraphNodeRef, b: &GraphNodeRef| a.label.cmp(&b.label),
            (SortField::Label, SortDirection::Desc) => |a: &GraphNodeRef, b: &GraphNodeRef| b.label.cmp(&a.label),
        };
        nodes.sort_by(cmp);
    }

    fn apply_limit(&self, nodes: &mut Vec<GraphNodeRef>, limit: &LimitOp) {
        // Drain elements before offset
        if limit.offset > 0 {
            nodes.drain(0..limit.offset.min(nodes.len()));
        }
        // Then keep at most count elements
        if nodes.len() > limit.count {
            nodes.drain(limit.count..);
        }
    }

    fn apply_limit_edges(&self, edges: &mut Vec<GraphEdgeRef>, limit: &LimitOp) {
        let start = limit.offset.min(edges.len());
        let end = (limit.offset.saturating_add(limit.count)).min(edges.len());
        edges.drain(start..end);
    }
    // `explain` built a `QueryExplanation` describing a query plan. It had
    // no caller and no test, and nothing else ever constructed the type, so
    // the whole explain path was unreachable.

}

impl From<Direction> for PetDirection {
    fn from(dir: Direction) -> Self {
        match dir {
            Direction::Outgoing => PetDirection::Outgoing,
            Direction::Incoming => PetDirection::Incoming,
            Direction::Both => PetDirection::Outgoing,
        }
    }
}
