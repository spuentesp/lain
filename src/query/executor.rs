//! Query executor for graph operations

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::query::spec::{
    ConnectOp, Direction, EdgeSelector, FilterOp, GraphNodeRef, GraphPath, GraphEdgeRef,
    GroupBy, GroupOp, LimitOp, QueryExplanation, QueryGroup, QueryMeta,
    QueryResult, QuerySpec, SortDirection, SortField, SortOp, TypeSelector,
    FindOp,
};
use petgraph::Direction as PetDirection;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::time::Instant;

/// Executor for running queries against the graph
pub struct Executor<'a> {
    graph: &'a GraphDatabase,
    nodes_visited: usize,
}

impl<'a> Executor<'a> {
    pub fn new(graph: &'a GraphDatabase) -> Self {
        Self {
            graph,
            nodes_visited: 0,
        }
    }

    /// Execute a query spec and return results
    pub fn execute(&mut self, spec: &QuerySpec) -> Result<QueryResult, LainError> {
        let start = Instant::now();

        if let Some(name) = &spec.named {
            return self.execute_named(name);
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

            if !depth_range.contains(&((current_depth + 1) as u32)) {
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

    pub fn explain(&self, spec: &QuerySpec) -> QueryExplanation {
        let mut steps = Vec::new();
        let mut warnings = Vec::new();

        for (i, op) in spec.ops.iter().enumerate() {
            match op {
                crate::query::spec::GraphOp::Find(find) => {
                    let mut desc = String::from("Find nodes");
                    if let Some(ref ty) = find.type_selector {
                        match ty {
                            TypeSelector::Single(s) => desc.push_str(&format!(" of type '{}'", s)),
                            TypeSelector::Or(types) => desc.push_str(&format!(" of types {:?}", types)),
                        }
                    }
                    if let Some(ref name) = find.name {
                        desc.push_str(&format!(" matching '{:?}'", name));
                    }
                    if let Some(ref label) = find.label_selector {
                        desc.push_str(&format!(" with label '{:?}'", label));
                    }
                    steps.push(desc);
                }
                crate::query::spec::GraphOp::Connect(connect) => {
                    let dir = match connect.direction {
                        Direction::Outgoing => "outgoing",
                        Direction::Incoming => "incoming",
                        Direction::Both => "both",
                    };
                    let depth = connect.depth.to_range();
                    let depth_str = if depth.start() == depth.end() {
                        format!("{}", depth.start())
                    } else {
                        format!("{}..={}", depth.start(), depth.end())
                    };
                    steps.push(format!(
                        "Traverse {:?} edges ({}) up to depth {}",
                        connect.edge, dir, depth_str
                    ));
                }
                crate::query::spec::GraphOp::Filter(filter) => {
                    let mut desc = String::from("Filter results");
                    if let Some(ref ty) = filter.type_filter {
                        desc.push_str(&format!(", type = '{:?}'", ty));
                    }
                    if let Some(ref label) = filter.label_filter {
                        desc.push_str(&format!(", label = '{:?}'", label));
                    }
                    steps.push(desc);
                }
                crate::query::spec::GraphOp::Sort(sort) => {
                    steps.push(format!("Sort by {:?} ({:?})", sort.by, sort.direction));
                }
                crate::query::spec::GraphOp::Limit(limit) => {
                    steps.push(format!("Limit to {} results, offset {}", limit.count, limit.offset));
                }
                crate::query::spec::GraphOp::Group(group) => {
                    steps.push(format!("Group by {:?}", group.by));
                }
            }

            if i > 0 {
                if matches!(op, crate::query::spec::GraphOp::Connect(_)) {
                    warnings.push("Deep traversals can be expensive on large graphs".into());
                }
            }
        }

        QueryExplanation {
            plan: format!("Execute {} ops: {}", spec.ops.len(), steps.join(" -> ")),
            steps,
            warnings: if warnings.is_empty() { None } else { Some(warnings) },
        }
    }
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
