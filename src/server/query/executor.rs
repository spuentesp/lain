//! Query executor for graph operations

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::query::spec::{
    ConnectOp, Direction, EdgeSelector, FilterOp, FindOp, GraphEdgeRef, GraphNodeRef, GraphPath,
    GroupBy, GroupOp, LimitOp, QueryGroup, QueryMeta, QueryMode, QueryResult, QuerySpec,
    SemanticFilterOp, SortDirection, SortField, SortOp,
};
use crate::schema::GraphEdge;
use crate::tools::utils::{build_enriched_text, cosine_similarity};
use parking_lot::Mutex;
use petgraph::Direction as PetDirection;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Instant;

pub type EmbeddingCache = Arc<Mutex<lru::LruCache<String, Vec<f32>>>>;

/// A node reported by one pure walk, paired with that walk's selected
/// shortest path. Several walks can report the same node; `execute_connect`
/// merges the candidates under the documented tie-break.
struct TraversalCandidate {
    node: GraphNodeRef,
    /// Hop count of the walk's shortest matching-edge path to the node.
    hops: u32,
    /// The seed this walk started from — the path's first node.
    seed_id: String,
    direction: PetDirection,
    path: GraphPath,
}

impl TraversalCandidate {
    /// The documented selection order: fewest hops wins; on equal hops the
    /// outgoing walk beats the incoming one; on a further tie the lower seed
    /// id wins. `true` when `self` should replace `prev`.
    fn beats(&self, prev: &Self) -> bool {
        (
            self.hops,
            direction_rank(self.direction),
            self.seed_id.as_str(),
        ) < (
            prev.hops,
            direction_rank(prev.direction),
            prev.seed_id.as_str(),
        )
    }
}

fn direction_rank(direction: PetDirection) -> u8 {
    match direction {
        PetDirection::Outgoing => 0,
        PetDirection::Incoming => 1,
    }
}

/// Executor for running queries against the graph
pub struct Executor<'a> {
    graph: &'a GraphDatabase,
    embedder: &'a NlpEmbedder,
    embedding_cache: &'a EmbeddingCache,
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
        embedding_cache: &'a EmbeddingCache,
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
        embedding_cache: &'a EmbeddingCache,
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
                    return Err(LainError::InvalidArgument(
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
                    return Err(LainError::InvalidArgument(
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
        // Selected shortest path per working node, from the most recent
        // `connect`. The wire-level `edges`/`paths` are derived from this
        // after the whole pipeline, so they always describe exactly the
        // reported nodes — filters, `limit`, and the default cap included.
        let mut current_paths: HashMap<String, GraphPath> = HashMap::new();
        let mut groups: Option<Vec<QueryGroup>> = None;

        for op in &spec.ops {
            match op {
                crate::query::spec::GraphOp::Find(find) => {
                    current_nodes = self.execute_find(find)?;
                    // A find replaces the working set, so no reported node
                    // came from a walk and there is nothing left to report.
                    current_paths.clear();
                }
                crate::query::spec::GraphOp::Connect(connect) => {
                    let (nodes, paths) = self.execute_connect(&current_nodes, connect)?;
                    current_nodes = nodes;
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

        // `edges`/`paths` describe exactly the *reported* nodes: one path
        // per node (parallel to `nodes`), and the deduplicated union of the
        // edges on those paths in (from, to, type) order. A find/filter-only
        // pipeline walked no edges, so both stay empty and the skip-if-empty
        // attributes keep them off the wire.
        let mut paths: Vec<GraphPath> = Vec::new();
        let mut edge_set: BTreeMap<(String, String, String), GraphEdgeRef> = BTreeMap::new();
        for node in &current_nodes {
            // `current_paths` is written only by `connect` (which covers
            // every node it reports) and cleared by `find` (which replaces
            // the working set), so each remaining node has its path.
            let Some(path) = current_paths.get(&node.id) else {
                continue;
            };
            for edge in &path.edges {
                edge_set
                    .entry((edge.from.clone(), edge.to.clone(), edge.edge_type.clone()))
                    .or_insert_with(|| edge.clone());
            }
            paths.push(path.clone());
        }

        let exec_us = start.elapsed().as_micros() as u64;
        let count = current_nodes.len();

        Ok(QueryResult {
            nodes: current_nodes,
            edges: edge_set.into_values().collect(),
            paths,
            count,
            legacy: false,
            meta: Some(QueryMeta {
                exec_us,
                nodes_visited: self.nodes_visited,
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
        check_node_types(find)?;
        let mut nodes = self.graph.query_nodes(
            find.type_selector.as_ref(),
            find.name.as_ref(),
            find.label_selector.as_ref(),
            find.path.as_deref(),
        );
        // `id` selects exactly one node — at top level too. It used to be
        // honoured only inside a `connect` `target`, so a top-level
        // `{"op":"find","id":"..."}` parsed the id and then ignored it,
        // answering with every match of the other (often absent) filters:
        // the whole graph, for a query that asked for one node.
        if let Some(want) = &find.id {
            nodes.retain(|n| &n.id == want);
        }

        self.nodes_visited += nodes.len();

        let results = nodes
            .into_iter()
            .map(|n| GraphNodeRef {
                id: n.id.clone(),
                node_type: n.node_type.to_string(),
                name: n.name.clone(),
                // Surface the node's query label, not just `deprecated`:
                // producers attach `test` and attribute-derived labels too,
                // and a `filter` on one of those needs to see them. The old
                // mapping collapsed everything else to `None`.
                label: n.query_label().map(str::to_string),
            })
            .collect();

        Ok(results)
    }

    fn execute_connect(
        &mut self,
        start_nodes: &[GraphNodeRef],
        connect: &ConnectOp,
    ) -> Result<(Vec<GraphNodeRef>, HashMap<String, GraphPath>), LainError> {
        if start_nodes.is_empty() {
            return Ok((Vec::new(), HashMap::new()));
        }

        let depth_range = connect.depth.to_range();

        // Handle Direction::Both by traversing both directions and merging:
        // each direction is its own pure walk, so no path ever mixes them.
        let directions: Vec<PetDirection> = match connect.direction {
            Direction::Both => vec![PetDirection::Outgoing, PetDirection::Incoming],
            _ => vec![connect.direction.into()],
        };

        // One selected candidate per reported node — the best walk's path
        // under the documented tie-break (`TraversalCandidate::beats`).
        // `order` keeps first-report order across the walk loop.
        let mut best: HashMap<String, TraversalCandidate> = HashMap::new();
        let mut order: Vec<String> = Vec::new();

        for direction in directions {
            for start_node in start_nodes {
                for cand in self.bfs_traverse(
                    &start_node.id,
                    &connect.edge,
                    depth_range.clone(),
                    direction,
                )? {
                    let replace = match best.get(&cand.node.id) {
                        None => {
                            order.push(cand.node.id.clone());
                            true
                        }
                        Some(prev) => cand.beats(prev),
                    };
                    if replace {
                        best.insert(cand.node.id.clone(), cand);
                    }
                }
            }
        }

        // `target`: keep only the reached nodes it selects.
        if let Some(target) = connect.target.as_deref() {
            check_node_types(target)?;
            let allowed: std::collections::HashSet<String> = self
                .graph
                .query_nodes(
                    target.type_selector.as_ref(),
                    target.name.as_ref(),
                    target.label_selector.as_ref(),
                    target.path.as_deref(),
                )
                .into_iter()
                .map(|n| n.id.clone())
                .filter(|id| target.id.as_ref().is_none_or(|want| want == id))
                .collect();
            order.retain(|id| allowed.contains(id));
        }

        self.nodes_visited += order.len();

        let mut nodes = Vec::with_capacity(order.len());
        let mut paths = HashMap::with_capacity(order.len());
        for id in order {
            if let Some(cand) = best.remove(&id) {
                nodes.push(cand.node);
                paths.insert(id, cand.path);
            }
        }
        Ok((nodes, paths))
    }

    /// One pure walk from `start_id` (never mixed-direction): plain BFS
    /// recording every node's shortest matching-edge hop count, then one
    /// [`TraversalCandidate`] per node whose hops fall inside the reported
    /// range. Hops obey the same `depth` filter as node reporting; the walk
    /// itself runs to `max`, and the start (hop 0) is never reported.
    fn bfs_traverse(
        &mut self,
        start_id: &str,
        edge_selector: &EdgeSelector,
        depth_range: RangeInclusive<u32>,
        direction: PetDirection,
    ) -> Result<Vec<TraversalCandidate>, LainError> {
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
            return Err(LainError::InvalidArgument(format!(
                "unknown edge type(s) {}: valid edge types are {}",
                unknown.join(", "),
                valid.join(", ")
            )));
        }

        // Plain BFS, with the start seeded at hop 0. This used to walk the
        // queue as a LIFO stack, so a node reachable by paths of different
        // lengths was reported at its *first-discovery* depth rather than
        // its shortest hop count — `depth: 2` could miss a node two hops
        // away that the walk happened to reach via a three-hop detour first,
        // and "hops exactly 3" could report a node two hops away. The start
        // was also missing from `visited`, so a cycle or self-loop
        // re-reported the start node at its loop depth even though start
        // nodes are never reported (`docs/query-language.md`).
        let mut hops: HashMap<String, u32> = HashMap::new();
        let mut discovery: Vec<String> = Vec::new();
        hops.insert(start_id.to_string(), 0);
        let mut queue: VecDeque<String> = VecDeque::from([start_id.to_string()]);

        while let Some(current_id) = queue.pop_front() {
            let depth = hops[&current_id];

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
            if depth.saturating_add(1) > *depth_range.end() {
                continue;
            }

            for (neighbor, edge) in self.graph.get_neighbors(&current_id, direction) {
                if !edge_selector.matches(&edge.edge_type.to_string()) {
                    continue;
                }
                if hops.contains_key(&neighbor.id) {
                    continue;
                }
                hops.insert(neighbor.id.clone(), depth + 1);
                discovery.push(neighbor.id.clone());
                queue.push_back(neighbor.id.clone());
            }
        }

        let mut found = Vec::new();
        for id in discovery {
            let depth = hops[&id];
            if !depth_range.contains(&depth) {
                continue;
            }
            let path = self.shortest_walk_path(start_id, &id, &hops, edge_selector, direction)?;
            let Some(node) = self.graph.get_node(&id)? else {
                continue;
            };
            found.push(TraversalCandidate {
                node: GraphNodeRef {
                    id: node.id.clone(),
                    node_type: node.node_type.to_string(),
                    name: node.name.clone(),
                    label: node.query_label().map(str::to_string),
                },
                hops: depth,
                seed_id: start_id.to_string(),
                direction,
                path,
            });
        }
        Ok(found)
    }

    /// The walk's selected shortest path from `start_id` to `target_id`,
    /// built backwards one hop at a time. Each hop goes to the **lowest-id**
    /// predecessor sitting at the next-lower hop count — the documented
    /// tie-break — and among parallel matching edges on that pair the
    /// lexicographically smallest edge type name. Hop counts strictly
    /// decrease and only the seed sits at hop 0, so the chain terminates at
    /// `start_id`.
    fn shortest_walk_path(
        &self,
        start_id: &str,
        target_id: &str,
        hops: &HashMap<String, u32>,
        edge_selector: &EdgeSelector,
        direction: PetDirection,
    ) -> Result<GraphPath, LainError> {
        // The walk moves `current -> neighbour` along `direction`; walking
        // it backwards flips the direction.
        let backwards = match direction {
            PetDirection::Outgoing => PetDirection::Incoming,
            PetDirection::Incoming => PetDirection::Outgoing,
        };

        let mut node_ids = vec![target_id.to_string()];
        let mut edge_refs = Vec::new();
        let mut current = target_id.to_string();
        while current != start_id {
            let depth = *hops.get(&current).ok_or_else(|| {
                LainError::Graph(format!("node {current} was not reached by this walk"))
            })?;
            let mut best: Option<(String, GraphEdge)> = None;
            for (pred, edge) in self.graph.get_neighbors(&current, backwards) {
                if !edge_selector.matches(&edge.edge_type.to_string()) {
                    continue;
                }
                if hops.get(&pred.id).copied() != Some(depth - 1) {
                    continue;
                }
                let better = match &best {
                    None => true,
                    Some((best_id, best_edge)) => {
                        (pred.id.as_str(), edge.edge_type.to_string())
                            < (best_id.as_str(), best_edge.edge_type.to_string())
                    }
                };
                if better {
                    best = Some((pred.id.clone(), edge));
                }
            }
            let Some((pred_id, edge)) = best else {
                return Err(LainError::Graph(format!(
                    "no matching predecessor at hop {} on the walk to {target_id}",
                    depth - 1
                )));
            };
            edge_refs.push(edge_ref(&edge));
            node_ids.push(pred_id.clone());
            current = pred_id;
        }
        node_ids.reverse();
        edge_refs.reverse();

        let mut nodes = Vec::with_capacity(node_ids.len());
        for id in &node_ids {
            let Some(node) = self.graph.get_node(id)? else {
                return Err(LainError::Graph(format!("path node {id} vanished")));
            };
            nodes.push(GraphNodeRef {
                id: node.id.clone(),
                node_type: node.node_type.to_string(),
                name: node.name.clone(),
                label: node.query_label().map(str::to_string),
            });
        }
        Ok(GraphPath {
            nodes,
            edges: edge_refs,
        })
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
        let query_emb = self
            .embedder
            .embed_query(&sem.like)
            .map_err(|e| LainError::Nlp(format!("Failed to embed query: {}", e)))?;

        // Get full graph nodes to access their embeddings
        let all_graph_nodes: HashMap<String, crate::schema::GraphNode> = self
            .graph
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
                self.embedding_cache
                    .lock()
                    .put(node.id.clone(), emb.clone());
                return Some(emb);
            }
        }

        // On-demand embed
        let text = build_enriched_text(node, self.workspace);
        self.embedder.embed(&text).ok().inspect(|emb| {
            self.embedding_cache
                .lock()
                .put(node.id.clone(), emb.clone());
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

    fn apply_sort(&self, nodes: &mut [GraphNodeRef], sort: &SortOp) {
        let cmp = match (sort.by, sort.direction) {
            (SortField::Name, SortDirection::Asc) => {
                |a: &GraphNodeRef, b: &GraphNodeRef| a.name.cmp(&b.name)
            }
            (SortField::Name, SortDirection::Desc) => {
                |a: &GraphNodeRef, b: &GraphNodeRef| b.name.cmp(&a.name)
            }
            (SortField::Type, SortDirection::Asc) => {
                |a: &GraphNodeRef, b: &GraphNodeRef| a.node_type.cmp(&b.node_type)
            }
            (SortField::Type, SortDirection::Desc) => {
                |a: &GraphNodeRef, b: &GraphNodeRef| b.node_type.cmp(&a.node_type)
            }
            (SortField::Label, SortDirection::Asc) => {
                |a: &GraphNodeRef, b: &GraphNodeRef| a.label.cmp(&b.label)
            }
            (SortField::Label, SortDirection::Desc) => {
                |a: &GraphNodeRef, b: &GraphNodeRef| b.label.cmp(&a.label)
            }
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

/// Wire ref for an edge: `id` is the `(from, to, type)` key, so the same
/// physical edge always carries the same id wherever it appears.
fn edge_ref(edge: &GraphEdge) -> GraphEdgeRef {
    GraphEdgeRef {
        id: format!("{}->{}:{}", edge.source_id, edge.target_id, edge.edge_type),
        edge_type: edge.edge_type.to_string(),
        from: edge.source_id.clone(),
        to: edge.target_id.clone(),
    }
}

/// Reject node type names that do not exist instead of matching nothing.
fn check_node_types(find: &FindOp) -> Result<(), LainError> {
    let Some(sel) = find.type_selector.as_ref() else {
        return Ok(());
    };
    let unknown = sel.unknown_types();
    if unknown.is_empty() {
        return Ok(());
    }
    let valid: Vec<String> = crate::server::schema::NodeType::all()
        .iter()
        .map(|t| t.to_string())
        .collect();
    Err(LainError::InvalidArgument(format!(
        "unknown node type(s) {unknown:?}; valid types: {}",
        valid.join(", ")
    )))
}
