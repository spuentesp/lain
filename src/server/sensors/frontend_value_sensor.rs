use crate::error::LainError;
use crate::federation::contracts::coverage::UnresolvedRecord;
use crate::federation::contracts::model::{ContractFact, SourceSite, UnresolvedReason};
use crate::graph::{graph_path, GraphDatabase};
use crate::schema::{EdgeType, GraphEdge, NodeType, RepoNamespace};
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::util::{enclosing_symbol, parse_for_lang, Lang};
use crate::server::sensors::{ScanReport, SensorEntry};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::Node;

pub struct FrontendValueSensor;

impl crate::server::sensors::Sensor for FrontendValueSensor {
    fn name(&self) -> &'static str {
        "frontend_values"
    }

    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::FrontendValues
    }

    fn phase(&self) -> u8 {
        2
    }

    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        scan_workspace_frontend_values(graph, root, namespace)
    }

    fn scan_with_report(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<ScanReport, LainError> {
        scan_workspace_frontend_values_with_report(graph, root, namespace)
    }
}
inventory::submit!(SensorEntry(&FrontendValueSensor));

#[derive(Debug, Clone)]
struct Binding {
    reader_id: String,
    field_id: String,
    name: String,
    declaration_line: u32,
    declaration_start: u32,
    scope_start: u32,
    scope_end: u32,
    react_setters: BTreeSet<String>,
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

#[derive(Clone, Copy)]
enum ReactHook {
    UseState,
    UseReducer,
}

struct ReactImports {
    by_local_name: BTreeMap<String, ReactHook>,
    namespaces: BTreeSet<String>,
}

impl ReactImports {
    fn kind_of(&self, local_name: &str) -> Option<ReactHook> {
        self.by_local_name.get(local_name).copied()
    }
}

fn hook_from_name(name: &str) -> Option<ReactHook> {
    match name {
        "useState" => Some(ReactHook::UseState),
        "useReducer" => Some(ReactHook::UseReducer),
        _ => None,
    }
}

fn process_react_import(node: Node, src: &[u8], imports: &mut ReactImports) {
    if node.kind() == "import_specifier" {
        let original = node
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(src).ok())
            .unwrap_or("");
        if let Some(hook) = hook_from_name(original) {
            let local = node
                .child_by_field_name("alias")
                .and_then(|alias| alias.utf8_text(src).ok())
                .unwrap_or(original);
            imports.by_local_name.insert(local.to_string(), hook);
        }
    } else if node.kind() == "namespace_import" || node.kind() == "identifier" {
        if let Ok(name) = node.utf8_text(src) {
            if name == "React" {
                imports.namespaces.insert(name.to_string());
            }
        }
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    for child in children {
        process_react_import(child, src, imports);
    }
}

fn react_imports(root: Node, src: &[u8]) -> ReactImports {
    let mut imports = ReactImports {
        by_local_name: BTreeMap::new(),
        namespaces: BTreeSet::new(),
    };
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.kind() == "import_statement" {
            let source = node
                .child_by_field_name("source")
                .and_then(|source| source.utf8_text(src).ok())
                .unwrap_or("");
            if source == "\"react\"" || source == "'react'" {
                process_react_import(node, src, &mut imports);
            }
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.named_children(&mut cursor).collect();
        pending.extend(children);
    }
    imports
}

fn called_hook_name(value: Node, src: &[u8], imports: &ReactImports) -> Option<ReactHook> {
    let call = (value.kind() == "call_expression").then_some(value)?;
    let function = call.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => imports.kind_of(function.utf8_text(src).ok()?),
        "member_expression" => {
            let object = function.child_by_field_name("object")?;
            if !imports.namespaces.contains(object.utf8_text(src).ok()?) {
                return None;
            }
            let property = function.child_by_field_name("property")?;
            hook_from_name(property.utf8_text(src).ok()?)
        }
        _ => None,
    }
}

fn array_pattern_element(name: Node, slot: usize, src: &[u8]) -> Option<String> {
    if name.kind() != "array_pattern" {
        return None;
    }
    let mut cursor = name.walk();
    let element = name.named_children(&mut cursor).nth(slot)?;
    if element.kind() != "identifier" {
        return None;
    }
    element.utf8_text(src).ok().map(str::to_string)
}

fn react_setter(node: Node, src: &[u8], react_hooks: &ReactImports) -> Option<String> {
    let declarator = (node.kind() == "variable_declarator").then_some(node)?;
    let name = declarator.child_by_field_name("name")?;
    let value = declarator.child_by_field_name("value")?;
    match called_hook_name(value, src, react_hooks)? {
        ReactHook::UseState | ReactHook::UseReducer => array_pattern_element(name, 1, src),
    }
}

fn setters_for_scope(
    setter_scopes: &[(u32, u32, String)],
    scope_start: u32,
    scope_end: u32,
) -> BTreeSet<String> {
    let mut closest = BTreeMap::<String, (u32, u32)>::new();
    for (start, end, setter) in setter_scopes {
        if *start > scope_start || *end < scope_end {
            continue;
        }
        let candidate = (*start, *end);
        closest
            .entry(setter.clone())
            .and_modify(|current| {
                if end - start < current.1 - current.0 || *start > current.0 {
                    *current = candidate;
                }
            })
            .or_insert(candidate);
    }
    closest.into_keys().collect()
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

pub fn scan_workspace_frontend_values_with_report(
    graph: &GraphDatabase,
    root: &Path,
    _namespace: &RepoNamespace,
) -> Result<ScanReport, LainError> {
    if graph.is_read_only() {
        return Ok(ScanReport::default());
    }
    let patterns = Patterns::with_overrides(root).unwrap_or_else(|error| {
        tracing::warn!(
            "frontend_value_sensor: override load failed ({error}); using bundled patterns"
        );
        std::borrow::Cow::Borrowed(Patterns::patterns())
    });
    let shapes = patterns.frontend_value_shapes();
    let storage_calls = &shapes.storage_calls;
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
    let mut unclassified_sites = Vec::new();

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
        let react_hooks = react_imports(tree.root_node(), src);
        let mut setter_scopes = Vec::new();
        let mut setter_pending = vec![tree.root_node()];
        while let Some(node) = setter_pending.pop() {
            if let Some(setter) = react_setter(node, src, &react_hooks) {
                let (scope_start, scope_end) = lexical_scope(node);
                setter_scopes.push((scope_start, scope_end, setter));
            }
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            setter_pending.extend(children.into_iter().rev());
        }
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
                                react_setters: setters_for_scope(
                                    &setter_scopes,
                                    scope_start,
                                    scope_end,
                                ),
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
                                let disposition = if inside_jsx_attribute(node) {
                                    Some(EdgeType::Forwards)
                                } else if inside_jsx_expression(node) {
                                    Some(EdgeType::Renders)
                                } else if let Some(call) = call_using(node, src) {
                                    if storage_calls.contains(&call)
                                        || binding.react_setters.contains(&call)
                                    {
                                        Some(EdgeType::Stores)
                                    } else {
                                        Some(EdgeType::Forwards)
                                    }
                                } else {
                                    None
                                };
                                if let Some(edge_type) = disposition {
                                    emit(
                                        &mut edges, &mut seen, edge_type, binding, &path, use_line,
                                    );
                                } else {
                                    unclassified_sites.push(format!("{path}:{use_line}"));
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
    graph.insert_edges_batch(&edges)?;
    unclassified_sites.sort();
    unclassified_sites.dedup();
    Ok(ScanReport {
        emitted: edges.len(),
        error: None,
        unresolved: grouped_unresolved(&unclassified_sites),
    })
}

fn grouped_unresolved(sites: &[String]) -> Vec<UnresolvedRecord> {
    if sites.is_empty() {
        return Vec::new();
    }
    vec![UnresolvedRecord {
        reason: UnresolvedReason::FrontendValueUnclassified,
        count: sites.len(),
        sample_ids: sites.iter().take(5).cloned().collect(),
    }]
}

pub fn scan_workspace_frontend_values(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    scan_workspace_frontend_values_with_report(graph, root, namespace).map(|report| report.emitted)
}
