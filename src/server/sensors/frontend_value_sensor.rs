use crate::error::LainError;
use crate::federation::contracts::model::{ContractFact, SourceSite};
use crate::graph::{graph_path, GraphDatabase};
use crate::schema::{EdgeType, GraphEdge, NodeType, RepoNamespace};
use crate::server::sensors::patterns::{FrontendValueShapes, Patterns};
use crate::server::sensors::util::{enclosing_symbol, parse_for_lang, Lang};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

pub struct FrontendValueSensor;
crate::server::sensors::register_sensor!(
    FrontendValueSensor,
    "frontend_values",
    FrontendValues,
    2,
    scan_workspace_frontend_values
);

#[derive(Debug, Clone)]
struct Binding {
    reader_id: String,
    field_id: String,
    name: String,
    declaration_line: u32,
    declaration_start: u32,
    scope_start: u32,
    scope_end: u32,
}

fn pattern(shapes: &FrontendValueShapes, key: &str, fallback: &str) -> Regex {
    let source = shapes
        .patterns
        .get(key)
        .map(String::as_str)
        .unwrap_or(fallback);
    Regex::new(source)
        .unwrap_or_else(|error| panic!("invalid frontend value pattern {key}: {error}"))
}

fn line(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn ancestors(node: Node) -> impl Iterator<Item = Node> {
    std::iter::successors(node.parent(), |parent| parent.parent())
}

fn inside_jsx_attribute(node: Node) -> bool {
    ancestors(node).any(|parent| parent.kind() == "jsx_attribute")
}

fn inside_jsx_expression(node: Node) -> bool {
    ancestors(node).any(|parent| parent.kind() == "jsx_expression")
}

fn call_using(node: Node, src: &[u8]) -> Option<String> {
    let arguments = ancestors(node).find(|parent| parent.kind() == "arguments")?;
    let call = arguments
        .parent()
        .filter(|parent| parent.kind() == "call_expression")?;
    call.child_by_field_name("function")?
        .utf8_text(src)
        .ok()
        .map(str::to_string)
}

fn variable_declaration(node: Node, src: &[u8]) -> Option<(String, u32)> {
    if node.kind() != "variable_declarator" {
        return None;
    }
    let name = node.child_by_field_name("name")?;
    if name.kind() != "identifier" || node.child_by_field_name("value").is_none() {
        return None;
    }
    Some((name.utf8_text(src).ok()?.to_string(), line(node)))
}

fn lexical_scope(node: Node) -> (u32, u32) {
    ancestors(node)
        .find(|parent| parent.kind() == "statement_block")
        .map(|block| (block.start_byte() as u32, block.end_byte() as u32))
        .unwrap_or((0, u32::MAX))
}

fn is_declaration_name(node: Node, src: &[u8], name: &str) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "variable_declarator" || parent.child_by_field_name("name") != Some(node) {
        return false;
    }
    node.utf8_text(src).ok() == Some(name)
}

fn emit(
    edges: &mut Vec<GraphEdge>,
    seen: &mut BTreeSet<(String, String, String)>,
    edge_type: EdgeType,
    binding: &Binding,
    path: &str,
    site_line: u32,
) {
    if !seen.insert((
        edge_type.to_string(),
        binding.reader_id.clone(),
        binding.field_id.clone(),
    )) {
        return;
    }
    let mut edge = if edge_type == EdgeType::Discards {
        GraphEdge::new_heuristic(
            edge_type,
            binding.reader_id.clone(),
            binding.field_id.clone(),
            "frontend_unused_value",
            0.8,
        )
    } else {
        let mut edge = GraphEdge::new(
            edge_type,
            binding.reader_id.clone(),
            binding.field_id.clone(),
        );
        edge.weight = Some(1.0);
        edge
    };
    edge.site = Some(SourceSite {
        path: path.to_string(),
        line: site_line,
    });
    edges.push(edge);
}

pub fn scan_workspace_frontend_values(
    graph: &GraphDatabase,
    root: &Path,
    _namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let patterns = Patterns::with_overrides(root).unwrap_or_else(|error| {
        tracing::warn!(
            "frontend_value_sensor: override load failed ({error}); using bundled patterns"
        );
        std::borrow::Cow::Borrowed(Patterns::patterns())
    });
    let shapes = patterns.frontend_value_shapes();
    let storage_call = pattern(
        shapes,
        "storage_call",
        r"^(?:localStorage|sessionStorage)\.setItem$",
    );
    let state_setter = pattern(shapes, "state_setter", r"^set[A-Z_]");
    let nodes = graph.get_all_nodes();
    let mut field_nodes = BTreeMap::new();
    for node in &nodes {
        if node.node_type == NodeType::FieldRef
            && node
                .contract
                .iter()
                .any(|fact| matches!(fact, ContractFact::FieldRead(read) if read.exact))
        {
            if let Some(line) = node.line_start {
                field_nodes
                    .entry((node.path.clone(), line))
                    .or_insert_with(Vec::new)
                    .push(node);
            }
        }
    }
    let graph_edges = graph.all_edges();
    let mut edges = Vec::new();

    for (file_path, content, extension) in crate::server::sensors::util::scan_files(root, |path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| matches!(*extension, "ts" | "tsx"))
            .map(|extension| extension.to_string())
    }) {
        let path = graph_path(root, &file_path);
        let lang = if extension == "tsx" {
            Lang::Tsx
        } else {
            Lang::Ts
        };
        let Some(tree) = parse_for_lang(lang, &content) else {
            continue;
        };
        let src = content.as_bytes();
        let mut pending = vec![tree.root_node()];
        let mut bindings = Vec::new();
        while let Some(node) = pending.pop() {
            if let Some((name, declaration_line)) = variable_declaration(node, src) {
                if let Some(reader) = enclosing_symbol(graph, &path, declaration_line) {
                    for field in field_nodes
                        .get(&(path.clone(), declaration_line))
                        .into_iter()
                        .flatten()
                    {
                        if graph_edges.iter().any(|edge| {
                            edge.edge_type == EdgeType::ReadsField
                                && edge.source_id == reader.id
                                && edge.target_id == field.id
                        }) {
                            let (scope_start, scope_end) = lexical_scope(node);
                            bindings.push(Binding {
                                reader_id: reader.id.clone(),
                                field_id: field.id.clone(),
                                name: name.clone(),
                                declaration_line,
                                declaration_start: node.start_byte() as u32,
                                scope_start,
                                scope_end,
                            });
                        }
                    }
                }
            }
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            pending.extend(children.into_iter().rev());
        }
        if bindings.is_empty() {
            continue;
        }

        let mut by_name = BTreeMap::<String, Vec<usize>>::new();
        for (index, binding) in bindings.iter().enumerate() {
            by_name.entry(binding.name.clone()).or_default().push(index);
        }
        let mut used = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![tree.root_node()];
        while let Some(node) = pending.pop() {
            if node.kind() == "identifier" {
                if let Ok(name) = node.utf8_text(src) {
                    if let Some(candidates) = by_name.get(name) {
                        let use_line = line(node);
                        if let Some(reader) = enclosing_symbol(graph, &path, use_line) {
                            let use_start = node.start_byte() as u32;
                            let Some(index) = candidates
                                .iter()
                                .copied()
                                .filter(|index| {
                                    let binding = &bindings[*index];
                                    binding.reader_id == reader.id
                                        && binding.declaration_start <= use_start
                                        && binding.scope_start <= use_start
                                        && use_start < binding.scope_end
                                })
                                .max_by_key(|index| bindings[*index].declaration_start)
                            else {
                                continue;
                            };
                            let binding = &bindings[index];
                            if !is_declaration_name(node, src, name) {
                                used.insert(index);
                                if inside_jsx_attribute(node) {
                                    emit(
                                        &mut edges,
                                        &mut seen,
                                        EdgeType::Forwards,
                                        binding,
                                        &path,
                                        use_line,
                                    );
                                } else if inside_jsx_expression(node) {
                                    emit(
                                        &mut edges,
                                        &mut seen,
                                        EdgeType::Renders,
                                        binding,
                                        &path,
                                        use_line,
                                    );
                                } else if let Some(call) = call_using(node, src) {
                                    let edge_type = if storage_call.is_match(&call)
                                        || state_setter.is_match(&call)
                                    {
                                        EdgeType::Stores
                                    } else {
                                        EdgeType::Forwards
                                    };
                                    emit(
                                        &mut edges, &mut seen, edge_type, binding, &path, use_line,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            pending.extend(children.into_iter().rev());
        }
        for (index, binding) in bindings.iter().enumerate() {
            if !used.contains(&index) {
                emit(
                    &mut edges,
                    &mut seen,
                    EdgeType::Discards,
                    binding,
                    &path,
                    binding.declaration_line,
                );
            }
        }
    }

    let stale: Vec<_> = graph_edges
        .into_iter()
        .filter(|edge| {
            matches!(
                edge.edge_type,
                EdgeType::Renders | EdgeType::Stores | EdgeType::Forwards | EdgeType::Discards
            )
        })
        .collect();
    graph.remove_edges(&stale)?;
    let emitted = edges.len();
    graph.insert_edges_batch(&edges)?;
    Ok(emitted)
}
