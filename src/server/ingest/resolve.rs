//! Resolve phases shared by both ingestion pipelines.
//!
//! The single-workspace pipeline ([`crate::server::ingest::LainServer::build_core_memory`])
//! and the federation pipeline
//! ([`crate::server::ingest::ingestion::index_one_repo`]) used to
//! re-implement the same three resolve loops verbatim: external refs
//! into Calls edges, tree-sitter static refs into Calls/Uses edges,
//! and string-literal pattern refs into Pattern edges. The
//! federation path used tighter constants
//! ([`PatternLimits::FEDERATION`]) while the single-workspace path
//! used the defaults ([`PatternLimits::DEFAULT`]). All three
//! functions in this module are pure: `&self` only on the `db` they
//! mutate through the public `insert_edges_batch`/`upsert_edge` API.

use crate::graph::{graph_path, GraphDatabase};
use crate::schema::{is_type_level_target, EdgeType, GraphEdge};
use crate::lsp::ReferenceLocation;
use super::scan::{PatternRef, StaticFileRef};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Budget knobs for [`resolve_pattern_edges`]. The single-workspace
/// pipeline wants broader coverage; the federation pipeline wants a
/// tighter cap so two repos don't bloat the cross-boundary edge set.
#[derive(Clone, Copy)]
pub struct PatternLimits {
    pub max_files_per_value: usize,
    pub min_dirs: usize,
    pub max_edges: usize,
    pub edges_per_value: usize,
}

impl PatternLimits {
    /// The single-workspace defaults — values lifted from the
    /// pre-extraction constants in `ingestion.rs`.
    pub const DEFAULT: PatternLimits = PatternLimits {
        max_files_per_value: 200,
        min_dirs: 2,
        max_edges: 200,
        edges_per_value: 10,
    };

    /// Tighter caps used by the federation pipeline — also lifted
    /// from the pre-extraction `PATTERN_MAX_REF_COUNT` and friends.
    pub const FEDERATION: PatternLimits = PatternLimits {
        max_files_per_value: 20,
        min_dirs: 2,
        max_edges: 200,
        edges_per_value: 10,
    };
}

/// Link external refs (already-resolved source ids + LSP reference
/// locations) to internal nodes. For every ref whose target resolves
/// to a known node at `path:line`, emit a `Calls` edge from the
/// source id to the target id — skipping self-edges.
///
/// `workspace` is the path used by [`graph_path`] to translate an
/// absolute reference path into the same relative key the scanner
/// uses; passing the wrong workspace silently produces 0 edges.
pub fn resolve_call_edges(
    db: &GraphDatabase,
    workspace: &Path,
    refs: &[(String, ReferenceLocation)],
) -> Vec<GraphEdge> {
    let mut edges = Vec::with_capacity(refs.len());
    for (source_id, ref_loc) in refs {
        let path_str = graph_path(workspace, &ref_loc.path);
        if let Some(target) = db.get_node_at_location(&path_str, ref_loc.line) {
            if target.id != *source_id {
                edges.push(GraphEdge::new(EdgeType::Calls, source_id.clone(), target.id));
            }
        }
    }
    edges
}

/// Resolve tree-sitter-derived `Calls`/`Uses` references to internal
/// nodes by name. Self-edges are dropped; `Uses` edges are only kept
/// when the target is a type-level declaration
/// (see [`is_type_level_target`]). Each (source, target) pair is
/// emitted at most once via the local `seen` set.
///
/// `refs` already carries the source file path + line for each entry;
/// no workspace path is needed here.
pub fn resolve_static_edges(db: &GraphDatabase, refs: &[StaticFileRef]) -> Vec<GraphEdge> {
    let mut name_index: HashMap<String, Vec<(String, crate::schema::NodeType)>> = HashMap::new();
    for node in db.get_all_nodes() {
        name_index
            .entry(node.name.clone())
            .or_default()
            .push((node.id.clone(), node.node_type.clone()));
    }

    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for sr in refs {
        let Some(source_node) = db.get_node_at_location(&sr.file_path, sr.source_line) else {
            continue;
        };
        let Some(candidates) = name_index.get(sr.target_name.as_str()) else {
            continue;
        };
        for (target_id, target_type) in candidates {
            if *target_id == source_node.id {
                continue;
            }
            if sr.edge_type == EdgeType::Uses && !is_type_level_target(target_type) {
                continue;
            }
            let key = (source_node.id.clone(), (*target_id).to_string());
            if seen.insert(key) {
                edges.push(GraphEdge::new(
                    sr.edge_type.clone(),
                    source_node.id.clone(),
                    (*target_id).to_string(),
                ));
            }
        }
    }
    edges
}

/// Detect cross-boundary coupling via repeated string literals: any
/// value that appears in `>= min_dirs` distinct parent directories and
/// `>= 2` files (and no more than `max_files_per_value`) becomes a
/// `Pattern` edge between each pair of files. The edge budget is
/// capped by `max_edges` overall and `edges_per_value` per value.
pub fn resolve_pattern_edges(
    db: &GraphDatabase,
    refs: &[PatternRef],
    limits: PatternLimits,
) -> Vec<GraphEdge> {
    if refs.is_empty() {
        return Vec::new();
    }

    let nodes: Vec<crate::schema::GraphNode> = db
        .get_all_nodes()
        .into_iter()
        .filter(|n| matches!(n.node_type, crate::schema::NodeType::File))
        .collect();
    let file_nodes: HashMap<&str, &crate::schema::GraphNode> = nodes
        .iter()
        .map(|n| (n.path.as_str(), n))
        .collect();

    let mut value_to_files: HashMap<String, Vec<String>> = HashMap::new();
    for pr in refs {
        let entry = value_to_files.entry(pr.value.clone()).or_default();
        if !entry.contains(&pr.file_path) {
            entry.push(pr.file_path.clone());
        }
    }

    let mut scored: Vec<(usize, String, Vec<String>)> = Vec::new();
    for (value, files) in value_to_files {
        if files.len() < 2 || files.len() > limits.max_files_per_value {
            continue;
        }
        let mut dirs: HashSet<String> = HashSet::new();
        for f in &files {
            if let Some(parent) = std::path::Path::new(f).parent() {
                dirs.insert(parent.to_string_lossy().to_string());
            }
        }
        if dirs.len() < limits.min_dirs {
            continue;
        }
        let pairs = dirs.len() * (dirs.len() - 1) / 2;
        scored.push((pairs, value, files));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let max_edges = (scored.len() * limits.edges_per_value).min(limits.max_edges);
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for (_score, _value, files) in scored {
        if edges.len() >= max_edges {
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
            if edges.len() >= max_edges {
                break;
            }
            for j in (i + 1)..all_dirs.len() {
                if edges.len() >= max_edges {
                    break;
                }
                let (_dir_a, file_a) = &all_dirs[i];
                let (_dir_b, file_b) = &all_dirs[j];
                let key = (file_a.clone(), file_b.clone());
                if seen.insert(key) {
                    if let (Some(node_a), Some(node_b)) = (
                        file_nodes.get(file_a.as_str()),
                        file_nodes.get(file_b.as_str()),
                    ) {
                        edges.push(GraphEdge::new(
                            EdgeType::Pattern,
                            node_a.id.clone(),
                            node_b.id.clone(),
                        ));
                    }
                }
                if edges.len() >= max_edges {
                    break;
                }
            }
        }
    }
    edges
}
