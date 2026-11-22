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
    services: Vec<crate::federation::contracts::config::ServiceDecl>,
    repo_workspaces: HashMap<String, std::path::PathBuf>,
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
            services: Vec::new(),
            repo_workspaces: HashMap::new(),
        }
    }

    pub fn with_services(
        mut self,
        services: Vec<crate::federation::contracts::config::ServiceDecl>,
    ) -> Self {
        self.services = services;
        self
    }

    pub fn with_repo_workspaces(mut self, roots: HashMap<String, std::path::PathBuf>) -> Self {
        self.repo_workspaces = roots;
        self
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
        self.nodes_visited = 0;

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
        let mut group_op: Option<GroupOp> = None;
        let mut explicitly_sorted = false;
        let mut rows: Vec<GraphPath> = Vec::new();
        let mut explanation = Vec::new();
        let mut explicit_matched = None;

        for op in &spec.ops {
            match op {
                crate::query::spec::GraphOp::Find(find) => {
                    current_nodes = self.execute_find(find)?;
                    explicitly_sorted = false;
                    // A find replaces the working set, so no reported node
                    // came from a walk and there is nothing left to report.
                    current_paths.clear();
                    rows.clear();
                    groups = None;
                    group_op = None;
                }
                crate::query::spec::GraphOp::Connect(connect) => {
                    let (nodes, paths, next_rows) =
                        self.execute_connect(&current_nodes, &current_paths, &rows, connect)?;
                    rows = next_rows;
                    groups = None;
                    group_op = None;
                    current_nodes = nodes;
                    current_paths = paths;
                    explicitly_sorted = false;
                }
                crate::query::spec::GraphOp::Filter(filter) => {
                    if rows.is_empty() {
                        current_nodes.retain(|n| self.filter_matches(n, None, filter));
                    } else {
                        rows.retain(|p| {
                            p.nodes
                                .last()
                                .is_some_and(|n| self.filter_matches(n, Some(p), filter))
                        });
                        current_nodes.retain(|n| {
                            rows.iter()
                                .any(|p| p.nodes.last().is_some_and(|v| v.id == n.id))
                        });
                    }
                    current_paths.retain(|_, selected| {
                        rows.iter().any(|path| {
                            path.edges
                                .iter()
                                .map(|e| &e.id)
                                .eq(selected.edges.iter().map(|e| &e.id))
                        })
                    });
                    for path in &rows {
                        if let Some(node) = path.nodes.last() {
                            current_paths
                                .entry(node.id.clone())
                                .or_insert_with(|| path.clone());
                        }
                    }
                }
                crate::query::spec::GraphOp::SemanticFilter(sem) => {
                    self.apply_semantic_filter(&mut current_nodes, sem)?;
                }
                crate::query::spec::GraphOp::Group(group) => {
                    groups = Some(self.apply_group(&current_nodes, &rows, group)?);
                    group_op = Some(group.clone());
                }
                crate::query::spec::GraphOp::Sort(sort) => {
                    if let Some(groups) = groups.as_mut() {
                        groups.sort_by(|a, b| {
                            let order = if sort.by == SortField::Count {
                                a.count.cmp(&b.count)
                            } else {
                                a.key.cmp(&b.key)
                            };
                            (if sort.direction == SortDirection::Desc {
                                order.reverse()
                            } else {
                                order
                            })
                            .then_with(|| a.key.cmp(&b.key))
                        });
                    } else if sort.by == SortField::Count {
                        return Err(LainError::InvalidArgument(
                            "sort by count/call_sites requires group first".into(),
                        ));
                    }
                    self.apply_sort(&mut current_nodes, sort);
                    explicitly_sorted = true;
                }
                crate::query::spec::GraphOp::Limit(limit) => {
                    explicit_matched
                        .get_or_insert(groups.as_ref().map_or(current_nodes.len(), Vec::len));
                    if let Some(groups) = groups.as_mut() {
                        let start = limit.offset.min(groups.len());
                        let end = start.saturating_add(limit.count).min(groups.len());
                        *groups = groups[start..end].to_vec();
                        current_nodes.retain(|n| {
                            groups.iter().any(|g| g.nodes.iter().any(|v| v.id == n.id))
                        });
                    } else {
                        self.apply_limit(&mut current_nodes, limit);
                    }
                }
            }
            rows.retain(|p| {
                p.nodes
                    .last()
                    .is_some_and(|n| current_nodes.iter().any(|v| v.id == n.id))
            });
            if matches!(
                op,
                crate::query::spec::GraphOp::Filter(_)
                    | crate::query::spec::GraphOp::SemanticFilter(_)
                    | crate::query::spec::GraphOp::Limit(_)
            ) {
                self.refresh_groups(&mut groups, group_op.as_ref(), &current_nodes, &rows)?;
            }
            if spec.explain {
                let mut edges = std::collections::HashSet::new();
                for path in &rows {
                    for edge in &path.edges {
                        edges.insert(edge.id.clone());
                    }
                }
                explanation.push(crate::query::spec::QueryStep {
                    op: serde_json::to_value(op).map_err(LainError::Json)?["op"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                    nodes: current_nodes.len(),
                    edges: edges.len(),
                });
            }
        }

        let asked_for_a_limit = spec
            .ops
            .iter()
            .any(|op| matches!(op, crate::query::spec::GraphOp::Limit(_)));
        let matched = current_nodes.len();
        let mut next_cursor = None;
        let mut truncated = false;
        let paginated = spec.limit.is_some() || spec.cursor.is_some();
        let default_capped =
            !asked_for_a_limit && self.default_limit > 0 && matched > self.default_limit;
        if !explicitly_sorted && (paginated || default_capped) {
            current_nodes.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        }
        if paginated {
            let start = match &spec.cursor {
                Some(raw) => {
                    let after = decode_cursor(spec, raw)?;
                    current_nodes
                        .iter()
                        .position(|node| node.id == after)
                        .map(|index| index + 1)
                        .ok_or_else(|| {
                            LainError::InvalidArgument(
                                "cursor anchor is not present in this query's results".into(),
                            )
                        })?
                }
                None => 0,
            };
            let page_limit = spec.limit.unwrap_or(self.default_limit);
            let end = if page_limit == 0 {
                current_nodes.len()
            } else {
                start.saturating_add(page_limit).min(current_nodes.len())
            };
            if end < current_nodes.len() {
                if let Some(last) = current_nodes.get(end.saturating_sub(1)) {
                    next_cursor = Some(encode_cursor(spec, &last.id));
                }
                truncated = true;
            }
            current_nodes = current_nodes[start..end].to_vec();
        } else if default_capped {
            current_nodes.truncate(self.default_limit);
            truncated = true;
        }

        rows.retain(|path| {
            path.nodes
                .last()
                .is_some_and(|node| current_nodes.iter().any(|n| n.id == node.id))
        });
        self.refresh_groups(&mut groups, group_op.as_ref(), &current_nodes, &rows)?;

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
            explain: spec.explain.then_some(explanation),
            nodes: current_nodes,
            edges: edge_set.into_values().collect(),
            paths,
            count,
            legacy: false,
            meta: Some(QueryMeta {
                exec_us,
                nodes_visited: self.nodes_visited,
                truncated,
                matched_before_limit: explicit_matched
                    .or_else(|| (truncated || spec.limit.is_some()).then_some(matched)),
                next_cursor,
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

        let results = nodes.into_iter().map(|n| GraphNodeRef::from(&n)).collect();

        Ok(results)
    }

    fn execute_connect(
        &mut self,
        start_nodes: &[GraphNodeRef],
        start_paths: &HashMap<String, GraphPath>,
        start_rows: &[GraphPath],
        connect: &ConnectOp,
    ) -> Result<
        (
            Vec<GraphNodeRef>,
            HashMap<String, GraphPath>,
            Vec<GraphPath>,
        ),
        LainError,
    > {
        if start_nodes.is_empty() {
            return Ok((Vec::new(), HashMap::new(), Vec::new()));
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
        let mut rows = Vec::new();

        for direction in directions {
            for start_node in start_nodes {
                for cand in self.bfs_traverse(
                    &start_node.id,
                    &connect.edge,
                    depth_range.clone(),
                    direction,
                )? {
                    let prefixes: Vec<_> = start_rows
                        .iter()
                        .filter(|p| p.nodes.last().is_some_and(|n| n.id == start_node.id))
                        .collect();
                    if prefixes.is_empty() {
                        rows.push(
                            start_paths
                                .get(&start_node.id)
                                .map(|prefix| compose_paths(prefix, &cand.path))
                                .unwrap_or_else(|| cand.path.clone()),
                        );
                    } else {
                        rows.extend(
                            prefixes
                                .into_iter()
                                .map(|prefix| compose_paths(prefix, &cand.path)),
                        );
                    }
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
                let path = cand
                    .path
                    .nodes
                    .first()
                    .and_then(|seed| start_paths.get(&seed.id))
                    .map(|prefix| compose_paths(prefix, &cand.path))
                    .unwrap_or(cand.path);
                nodes.push(cand.node);
                paths.insert(id, path);
            }
        }
        rows.retain(|p| {
            p.nodes
                .last()
                .is_some_and(|n| nodes.iter().any(|v| v.id == n.id))
        });
        Ok((nodes, paths, rows))
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
                node: GraphNodeRef::from(&node),
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
            nodes.push(GraphNodeRef::from(&node));
        }
        Ok(GraphPath {
            nodes,
            edges: edge_refs,
        })
    }

    fn filter_matches(
        &self,
        n: &GraphNodeRef,
        path: Option<&GraphPath>,
        filter: &FilterOp,
    ) -> bool {
        filter
            .type_filter
            .as_ref()
            .is_none_or(|v| v.matches(&n.node_type))
            && filter
                .label_filter
                .as_ref()
                .is_none_or(|v| v.matches(n.label.as_deref()))
            && filter.name.as_ref().is_none_or(|v| v.matches(&n.name))
            && [
                ("repo", &filter.repo),
                ("path", &filter.path),
                ("resolution", &filter.resolution),
                ("provenance", &filter.provenance),
                ("usage", &filter.usage),
                ("confidence", &filter.confidence),
            ]
            .iter()
            .all(|(property, predicate)| {
                predicate
                    .as_ref()
                    .is_none_or(|p| p.matches(&self.property(n, path, property)))
            })
    }

    /// Resolve node, path-endpoint, and traversed-edge properties without
    /// flattening away the consumer that contributed a row.
    fn property(
        &self,
        node: &GraphNodeRef,
        path: Option<&GraphPath>,
        property: &str,
    ) -> serde_json::Value {
        use serde_json::Value;
        if let Some(property) = property.strip_prefix("consumer.") {
            return path
                .and_then(|p| p.nodes.first())
                .map(|n| self.property(n, None, property))
                .unwrap_or(Value::Null);
        }
        if let Some(property) = property.strip_prefix("provider.") {
            return self.property(node, None, property);
        }
        if let Some(property) = property
            .strip_prefix("neighbour.")
            .or_else(|| property.strip_prefix("neighbor."))
        {
            return path
                .and_then(|p| p.nodes.iter().rev().nth(1))
                .map(|n| self.property(n, None, property))
                .unwrap_or(Value::Null);
        }
        if let Some(property) = property.strip_prefix("provenance.") {
            return self.property(node, path, property);
        }
        // Resolve common node fields without graph lookups or edge discovery.
        match property {
            "id" => return Value::String(node.id.clone()),
            "type" | "node_type" => return Value::String(node.node_type.clone()),
            "name" => return Value::String(node.name.clone()),
            "label" => return Value::String(node.label.clone().unwrap_or_default()),
            "path" => return node.path.clone().map(Value::String).unwrap_or(Value::Null),
            "repo" | "repo_id" => {
                return node
                    .repo_id
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null)
            }
            "line_start" => return serde_json::json!(node.line_start),
            "line_end" => return serde_json::json!(node.line_end),
            _ => {}
        }
        let needs_edge = property.starts_with("edge.")
            || matches!(
                property,
                "resolution" | "provenance" | "kind" | "source" | "usage" | "confidence"
            );
        let edge = needs_edge
            .then(|| {
                path.and_then(|p| p.edges.last()).cloned().or_else(|| {
                    self.graph
                        .get_neighbors(&node.id, PetDirection::Outgoing)
                        .iter()
                        .find(|(_, e)| e.edge_type == crate::schema::EdgeType::Binds)
                        .map(|(_, e)| GraphEdgeRef::from(e))
                })
            })
            .flatten();
        if let Some(p) = property.strip_prefix("edge.") {
            if p == "provenance"
                || p == "kind"
                || p == "source"
                || p == "resolution"
                || p == "confidence"
            {
                return self.property(node, path, p);
            }
            if let Some(nested) = p.strip_prefix("provenance.") {
                return self.property(node, path, nested);
            }
            return edge
                .as_ref()
                .and_then(|e| serde_json::to_value(e).ok())
                .and_then(|v| v.get(p).cloned())
                .unwrap_or(Value::Null);
        }
        match property {
            "service" => {
                let service = self
                    .services
                    .iter()
                    .filter(|s| {
                        Some(s.repo.as_str()) == node.repo_id.as_deref()
                            && (s.paths.is_empty()
                                || s.paths
                                    .iter()
                                    .any(|p| node.path.as_ref().is_some_and(|v| v.starts_with(p))))
                    })
                    .max_by_key(|s| s.paths.iter().map(String::len).max().unwrap_or(0));
                service
                    .map(|s| Value::String(s.name.clone()))
                    .unwrap_or_else(|| {
                        node.repo_id
                            .clone()
                            .map(Value::String)
                            .unwrap_or(Value::Null)
                    })
            }
            "resolution" => Value::String(
                if edge.as_ref().is_some_and(|e| {
                    matches!(
                        e.edge_type.as_str(),
                        "Binds" | "SendsHttp" | "Consumes" | "CallsHttp"
                    )
                }) {
                    "resolved"
                } else {
                    "unresolved"
                }
                .into(),
            ),
            "provenance" | "kind" => edge
                .as_ref()
                .and_then(|e| e.provenance.as_ref())
                .map(|p| {
                    Value::String(
                        match p {
                            crate::schema::EdgeProvenance::Static { .. } => "static",
                            crate::schema::EdgeProvenance::Heuristic { .. } => "heuristic",
                            crate::schema::EdgeProvenance::Runtime { .. } => "runtime",
                            crate::schema::EdgeProvenance::Confirmed { .. } => "confirmed",
                            crate::schema::EdgeProvenance::Ref { .. } => "ref",
                        }
                        .into(),
                    )
                })
                .unwrap_or(Value::Null),
            "source" => edge
                .as_ref()
                .and_then(|e| e.provenance.as_ref())
                .map(|p| {
                    Value::String(match p {
                        crate::schema::EdgeProvenance::Static { source } => format!("{source:?}"),
                        crate::schema::EdgeProvenance::Heuristic { detector, .. } => {
                            detector.clone()
                        }
                        crate::schema::EdgeProvenance::Runtime { trace_id, .. } => trace_id.clone(),
                        crate::schema::EdgeProvenance::Confirmed { source } => source.clone(),
                        crate::schema::EdgeProvenance::Ref { .. } => "service_reference".into(),
                    })
                })
                .unwrap_or(Value::Null),
            "usage" => edge
                .as_ref()
                .and_then(|e| e.usage)
                .and_then(|v| serde_json::to_value(v).ok())
                .unwrap_or(Value::Null),
            "confidence" => edge
                .and_then(|e| e.confidence)
                .map(|v| serde_json::json!(v))
                .unwrap_or(Value::Null),
            "commit" => node
                .commit
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
            _ => self.stored_property(node, property).unwrap_or(Value::Null),
        }
    }

    fn stored_property(
        &self,
        node: &GraphNodeRef,
        property: &str,
    ) -> Result<serde_json::Value, LainError> {
        let indices = self.graph.find_indices_by_name(&node.name);
        let stored = self.graph.graph_ref_for_read();
        let node = indices
            .into_iter()
            .filter_map(|index| stored.node_weight(index))
            .find(|n| n.id == node.id)
            .ok_or_else(|| {
                LainError::Graph(format!("property lookup failed for node {}", node.id))
            })?;
        let mut parts = property.split('.');
        let value = match parts.next().unwrap_or_default() {
            "signature" => serde_json::to_value(&node.signature).map_err(LainError::Json)?,
            "docstring" => serde_json::to_value(&node.docstring).map_err(LainError::Json)?,
            "fan_in" => serde_json::to_value(&node.fan_in).map_err(LainError::Json)?,
            "fan_out" => serde_json::to_value(&node.fan_out).map_err(LainError::Json)?,
            "calls_in" => serde_json::to_value(&node.calls_in).map_err(LainError::Json)?,
            "calls_out" => serde_json::to_value(&node.calls_out).map_err(LainError::Json)?,
            "anchor_score" => serde_json::to_value(&node.anchor_score).map_err(LainError::Json)?,
            "depth_from_main" => {
                serde_json::to_value(&node.depth_from_main).map_err(LainError::Json)?
            }
            "co_change_count" => {
                serde_json::to_value(&node.co_change_count).map_err(LainError::Json)?
            }
            "is_deprecated" => {
                serde_json::to_value(&node.is_deprecated).map_err(LainError::Json)?
            }
            "container" => serde_json::to_value(&node.container).map_err(LainError::Json)?,
            "last_lsp_sync" => {
                serde_json::to_value(&node.last_lsp_sync).map_err(LainError::Json)?
            }
            "last_git_sync" => {
                serde_json::to_value(&node.last_git_sync).map_err(LainError::Json)?
            }
            "commit_hash" => serde_json::to_value(&node.commit_hash).map_err(LainError::Json)?,
            "is_hydrated" => serde_json::to_value(&node.is_hydrated).map_err(LainError::Json)?,
            "contract" => serde_json::to_value(&node.contract).map_err(LainError::Json)?,
            "entry" => serde_json::to_value(&node.entry).map_err(LainError::Json)?,
            _ => return Ok(serde_json::Value::Null),
        };
        let mut current = &value;
        for part in parts {
            let Some(next) = current.get(part) else {
                return Ok(serde_json::Value::Null);
            };
            current = next;
        }
        Ok(current.clone())
    }

    /// Grouping propagates failed stored-node lookups instead of manufacturing
    /// a null bucket. Missing optional field values remain valid null keys.
    fn group_property(
        &self,
        node: &GraphNodeRef,
        path: Option<&GraphPath>,
        property: &str,
    ) -> Result<serde_json::Value, LainError> {
        if let Some(property) = property.strip_prefix("consumer.") {
            return path
                .and_then(|p| p.nodes.first())
                .map_or(Ok(serde_json::Value::Null), |node| {
                    self.group_property(node, None, property)
                });
        }
        if let Some(property) = property.strip_prefix("provider.") {
            return self.group_property(node, None, property);
        }
        if let Some(property) = property
            .strip_prefix("neighbour.")
            .or_else(|| property.strip_prefix("neighbor."))
        {
            return path
                .and_then(|p| p.nodes.iter().rev().nth(1))
                .map_or(Ok(serde_json::Value::Null), |node| {
                    self.group_property(node, None, property)
                });
        }
        if [
            "signature",
            "docstring",
            "fan_in",
            "fan_out",
            "calls_in",
            "calls_out",
            "anchor_score",
            "depth_from_main",
            "co_change_count",
            "is_deprecated",
            "container",
            "last_lsp_sync",
            "last_git_sync",
            "commit_hash",
            "is_hydrated",
            "contract",
            "entry",
        ]
        .contains(&property.split('.').next().unwrap_or_default())
        {
            return self.stored_property(node, property);
        }
        Ok(self.property(node, path, property))
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

        nodes.retain(|node_ref| {
            let full_node = match self.graph.get_node(&node_ref.id) {
                Ok(Some(node)) => node,
                _ => return false,
            };
            let node_emb = self.get_node_embedding(&full_node);
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
        let text = self.node_embedding_text(node);
        self.embedder.embed(&text).ok().inspect(|emb| {
            self.embedding_cache
                .lock()
                .put(node.id.clone(), emb.clone());
        })
    }

    fn node_embedding_text(&self, node: &crate::schema::GraphNode) -> String {
        let workspace = node
            .repo_id
            .as_ref()
            .and_then(|repo| self.repo_workspaces.get(repo))
            .map(std::path::PathBuf::as_path)
            .unwrap_or(self.workspace);
        build_enriched_text(node, workspace)
    }

    fn apply_group(
        &self,
        nodes: &[GraphNodeRef],
        rows: &[GraphPath],
        group: &GroupOp,
    ) -> Result<Vec<QueryGroup>, LainError> {
        for node in nodes.iter().chain(rows.iter().flat_map(|p| p.nodes.iter())) {
            if !self.graph.has_node(&node.id) {
                return Err(LainError::Graph(format!("group node {} vanished", node.id)));
            }
        }
        let mut groups: BTreeMap<String, Vec<GraphNodeRef>> = BTreeMap::new();
        let properties = match &group.by {
            GroupBy::Type => vec!["type".to_string()],
            GroupBy::Label => vec!["label".to_string()],
            GroupBy::Name => vec!["name".to_string()],
            GroupBy::Property(p) => vec![p.clone()],
            GroupBy::Properties(p) => p.clone(),
            GroupBy::PathPrefix(_) => vec!["path".to_string()],
        };
        for property in &properties {
            let leaf = property
                .strip_prefix("consumer.")
                .or_else(|| property.strip_prefix("provider."))
                .or_else(|| property.strip_prefix("neighbor."))
                .or_else(|| property.strip_prefix("neighbour."))
                .unwrap_or(property);
            let valid = if let Some(edge) = leaf.strip_prefix("edge.") {
                [
                    "id",
                    "type",
                    "from",
                    "to",
                    "provenance",
                    "confidence",
                    "kind",
                    "source",
                    "resolution",
                    "site",
                    "detail",
                ]
                .contains(&edge.split('.').next().unwrap_or_default())
            } else {
                let root = leaf.split('.').next().unwrap_or_default();
                [
                    "type",
                    "repo",
                    "service",
                    "resolution",
                    "provenance",
                    "confidence",
                    "kind",
                    "source",
                    "commit",
                ]
                .contains(&root)
                    || [
                        "id",
                        "node_type",
                        "name",
                        "label",
                        "path",
                        "repo_id",
                        "line_start",
                        "line_end",
                        "signature",
                        "docstring",
                        "fan_in",
                        "fan_out",
                        "calls_in",
                        "calls_out",
                        "anchor_score",
                        "depth_from_main",
                        "co_change_count",
                        "is_deprecated",
                        "container",
                        "last_lsp_sync",
                        "last_git_sync",
                        "commit_hash",
                        "is_hydrated",
                        "contract",
                        "entry",
                    ]
                    .contains(&root)
            };
            if !valid {
                return Err(LainError::InvalidArgument(format!("unknown group property `{property}`; expected a node property, repo, service, resolution, provenance, confidence, consumer.*, provider.*, neighbour.*, or edge.*")));
            }
        }
        let add = |node: &GraphNodeRef,
                   path: Option<&GraphPath>,
                   groups: &mut BTreeMap<String, Vec<GraphNodeRef>>|
         -> Result<(), LainError> {
            let values: Vec<_> = properties
                .iter()
                .map(|p| self.group_property(node, path, p))
                .collect::<Result<Vec<_>, _>>()?;
            let key = if let GroupBy::PathPrefix(prefix) = &group.by {
                if !node.path.as_ref().is_some_and(|p| p.starts_with(prefix)) {
                    return Ok(());
                }
                prefix.clone()
            } else if values.len() == 1 {
                values[0]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| values[0].to_string())
            } else {
                serde_json::to_string(&values).unwrap_or_default()
            };
            groups.entry(key).or_default().push(node.clone());
            Ok(())
        };
        if rows.is_empty() {
            for node in nodes {
                add(node, None, &mut groups)?;
            }
        } else {
            for path in rows {
                if let Some(node) = path.nodes.last() {
                    add(node, Some(path), &mut groups)?;
                }
            }
        }
        Ok(groups
            .into_iter()
            .map(|(key, nodes)| QueryGroup {
                key,
                count: nodes.len(),
                nodes,
            })
            .collect())
    }

    fn refresh_groups(
        &self,
        groups: &mut Option<Vec<QueryGroup>>,
        operation: Option<&GroupOp>,
        nodes: &[GraphNodeRef],
        rows: &[GraphPath],
    ) -> Result<(), LainError> {
        if let (Some(previous), Some(operation)) = (groups, operation) {
            let mut updated: HashMap<_, _> = self
                .apply_group(nodes, rows, operation)?
                .into_iter()
                .map(|group| (group.key.clone(), group))
                .collect();
            // Preserve group sorting and explicit group-limit selection.
            *previous = previous
                .iter()
                .filter_map(|group| updated.remove(&group.key))
                .collect();
        }
        Ok(())
    }

    fn apply_sort(&self, nodes: &mut [GraphNodeRef], sort: &SortOp) {
        let cmp = match (sort.by, sort.direction) {
            (SortField::Count, _) => return,
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
        nodes.sort_by(|a, b| {
            cmp(a, b)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.id.cmp(&b.id))
        });
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

fn compose_paths(prefix: &GraphPath, segment: &GraphPath) -> GraphPath {
    let mut nodes = prefix.nodes.clone();
    nodes.extend(segment.nodes.iter().skip(1).cloned());
    let mut edges = prefix.edges.clone();
    edges.extend(segment.edges.iter().cloned());
    GraphPath { nodes, edges }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct QueryCursor {
    fingerprint: String,
    after: String,
}

fn query_fingerprint(spec: &QuerySpec) -> Result<String, LainError> {
    let mut canonical = spec.clone();
    canonical.cursor = None;
    let bytes = serde_json::to_vec(&canonical).map_err(LainError::Json)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn encode_cursor(spec: &QuerySpec, after: &str) -> String {
    let cursor = QueryCursor {
        fingerprint: query_fingerprint(spec).unwrap_or_default(),
        after: after.to_string(),
    };
    let bytes = serde_json::to_vec(&cursor).unwrap_or_default();
    format!("q1_{}", hex::encode(bytes))
}

fn decode_cursor(spec: &QuerySpec, raw: &str) -> Result<String, LainError> {
    let encoded = raw
        .strip_prefix("q1_")
        .ok_or_else(|| LainError::InvalidArgument("malformed query cursor".to_string()))?;
    let bytes = hex::decode(encoded)
        .map_err(|_| LainError::InvalidArgument("malformed query cursor".to_string()))?;
    let cursor: QueryCursor = serde_json::from_slice(&bytes)
        .map_err(|_| LainError::InvalidArgument("malformed query cursor".to_string()))?;
    if cursor.fingerprint != query_fingerprint(spec)? {
        return Err(LainError::InvalidArgument(
            "cursor was created for a different query".to_string(),
        ));
    }
    Ok(cursor.after)
}

/// Wire ref for an edge: `id` is the `(from, to, type)` key, so the same
/// physical edge always carries the same id wherever it appears.
fn edge_ref(edge: &GraphEdge) -> GraphEdgeRef {
    GraphEdgeRef::from(edge)
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

#[cfg(test)]
mod review_g_property_tests {
    use super::*;

    #[test]
    fn review_g_semantic_text_uses_owning_repository() {
        let home = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("lib.rs"), "wrong body\nwrong end\n").unwrap();
        std::fs::write(foreign.path().join("lib.rs"), "foreign body\nforeign end\n").unwrap();
        let graph = GraphDatabase::empty_writable();
        let embedder = NlpEmbedder::new_stub();
        let cache = Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(8).unwrap(),
        )));
        let executor = Executor::new(&graph, &embedder, &cache, home.path()).with_repo_workspaces(
            HashMap::from([("foreign".into(), foreign.path().to_path_buf())]),
        );
        let mut node = crate::schema::GraphNode::new(
            crate::schema::NodeType::Function,
            "f".into(),
            "lib.rs".into(),
        )
        .with_location(1, 2);
        node.repo_id = Some("foreign".into());
        let text = executor.node_embedding_text(&node);
        assert!(text.contains("foreign body"), "{text}");
        assert!(!text.contains("wrong body"), "{text}");
    }

    #[test]
    fn review_g_properties_use_refs_and_missing_group_nodes_error() {
        let graph = GraphDatabase::empty_writable();
        let embedder = NlpEmbedder::new_stub();
        let cache = Arc::new(Mutex::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(8).unwrap(),
        )));
        let executor = Executor::new(&graph, &embedder, &cache, std::path::Path::new("."));
        let node = crate::schema::GraphNode::new(
            crate::schema::NodeType::Function,
            "retained".into(),
            "lib.rs".into(),
        );
        let reference = GraphNodeRef::from(&node);
        assert_eq!(
            executor.property(&reference, None, "name"),
            serde_json::json!("retained")
        );
        let group: GroupOp = serde_json::from_value(serde_json::json!({"by":"signature"})).unwrap();
        assert!(executor
            .apply_group(&[reference.clone()], &[], &group)
            .is_err());
        // An existing ID with a stale name also represents a failed lookup,
        // rather than a legitimate missing optional signature.
        let mut renamed = node;
        renamed.name = "renamed".into();
        graph.upsert_node(renamed).unwrap();
        assert!(executor.apply_group(&[reference], &[], &group).is_err());
    }
}
