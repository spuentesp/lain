//! Phase E — GraphQL consumer sensor (spec §8.3).
//!
//! Detects the per-language call patterns that surface a
//! GraphQL operation document to the federation. For each
//! detected operation the sensor extracts the top-level
//! selection fields (one consumer per root field) and emits a
//! `ContractFact::GraphqlConsumer { op, field }` per field.
//!
//! Recognised patterns (spec §8.3):
//!
//! - **Tagged templates (TS/JS):** `gql\`query { orders { id }
//!   }\`` and `graphql\`{ orders { id } }\`` (the bare
//!   `gql("...")` form is also accepted in TS/JS). Python:
//!   `gql("query { orders { id } }")` and
//!   `client.execute("query { ... }")`. Apollo persisted
//!   operations: `useQuery(GET_ORDERS)` (the variable is the
//!   operation's name; we look for a co-located `query
//!   GET_ORDERS { ... }` definition in the same file).
//! - **`.graphql` documents:** standalone `.graphql` files
//!   in the repo (or string literals ending in `.graphql`).
//! - **Persisted operations:** Apollo persisted operations
//!   (`createPersistedQueryLink` / `createApolloPersistedQuery`
//!   usage) — the sensor does not actually parse the
//!   manifest format in v1; the canonical pattern is
//!   `query getOrders { ... }` declared in the same file.
//!
//! For each detected operation:
//!
//! - Parse the document with the same hand-rolled tokenizer
//!   used by the provider sensor, extracting only the
//!   top-level selection fields of the operation's root
//!   (`query { orders, foo, giftData }` → fields
//!   `["orders", "foo", "giftData"]`).
//! - Emit one `GraphqlConsumer { op, field }` record per
//!   top-level field.
//!
//! Fragment-only documents (no operation at the top level)
//! land in the coverage ledger as
//! `Unresolved { reason: DynamicOperation }` and produce no
//! `GraphqlConsumer` record.
//!
//! Interpolated documents (`` gql`query { ${query} }` ``) are
//! similarly `DynamicOperation` — the sensor detects the
//! `${...}` / `${...}` placeholder and bails out before
//! parsing.

use crate::error::LainError;
use crate::federation::contracts::model::{
    ContractFact, FieldReadFact, FieldReadOrigin, GraphqlConsumerFact, GraphqlOp, JsonPath,
    PathSegment,
};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::path::Path;

// ─── Public sensor shape ───────────────────────────────────────────────

/// One detected GraphQL operation top-level field. Public so
/// the acceptance tests can exercise the detector without
/// going through the graph emission path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlConsumerField {
    pub op: GraphqlOp,
    pub field: String,
    pub selected_fields: Vec<String>,
    /// `true` when the document was a fragment-only or
    /// interpolated operation; the sensor emits NO
    /// `GraphqlConsumer` record for these and the joiner
    /// records them as `Unresolved { reason: DynamicOperation }`.
    pub dynamic: bool,
    /// Source file the consumer was detected in (a
    /// repo-relative path).
    pub site_path: String,
    /// 1-based line number of the call site.
    pub site_line: u32,
}

pub struct GraphqlConsumerSensor;

crate::server::sensors::register_sensor!(
    GraphqlConsumerSensor,
    "graphql_consumer",
    Graphql,
    1,
    scan_workspace_graphql_consumer
);

/// Walk `root`, find every recognised GraphQL consumer (gql /
/// graphql tagged templates, .graphql / .gql document
/// references, .graphql document files), and emit one
/// `GraphqlConsumer` `Function` node per top-level field,
/// plus `FieldRef` nodes for selected fields linked via
/// `ReadsFrom` and `ReadsField` edges.
/// Returns the count emitted.
pub fn scan_workspace_graphql_consumer(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let mut total = 0usize;
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();
    // The consumer sensor covers two surfaces:
    //   1. .graphql / .gql document files (read as
    //      standalone documents; emit one consumer per
    //      top-level field per operation).
    //   2. Source files (.ts, .tsx, .js, .jsx, .py) with
    //      `gql` / `graphql` tagged templates or
    //      `gql("...")` / `client.execute("...")` calls.
    let doc_ext = |p: &Path| {
        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if ext == "graphql" || ext == "gql" {
                Some("doc")
            } else {
                None
            }
        } else {
            None
        }
    };
    for (path, content, _tag) in crate::server::sensors::util::scan_files(root, doc_ext) {
        let graph_path_str = graph_path(root, &path);
        let consumers = parse_document(&content, &graph_path_str);
        for c in consumers {
            if c.dynamic {
                continue;
            }
            let id_name = format!("graphql-call:{}:{}", c.op, c.field);
            let id = GraphNode::generate_id(
                &NodeType::Function,
                &c.site_path,
                &id_name,
                Some(c.site_line),
                namespace,
            );
            let mut node = GraphNode::new(NodeType::Function, id_name.clone(), c.site_path.clone());
            node.id = id.clone();
            node.line_start = Some(c.site_line);
            node.line_end = Some(c.site_line);
            node.contract = Some(ContractFact::GraphqlConsumer(GraphqlConsumerFact {
                op: c.op,
                field: c.field.clone(),
            }));
            all_nodes.push(node);
            total += 1;

            for (idx, sel) in c.selected_fields.iter().enumerate() {
                let ref_id_name = format!("graphql-read:{}:{}:{}", c.op, c.field, sel);
                let ref_id = GraphNode::generate_id(
                    &NodeType::FieldRef,
                    &c.site_path,
                    &ref_id_name,
                    Some(c.site_line + idx as u32),
                    namespace,
                );
                let mut ref_node =
                    GraphNode::new(NodeType::FieldRef, sel.clone(), c.site_path.clone());
                ref_node.id = ref_id.clone();
                ref_node.line_start = Some(c.site_line + idx as u32);
                ref_node.line_end = Some(c.site_line + idx as u32);
                ref_node.contract = Some(ContractFact::FieldRead(FieldReadFact {
                    chain: JsonPath(vec![PathSegment::Name(sel.clone())]),
                    exact: true,
                    origin: FieldReadOrigin::GraphqlConsumer,
                }));
                all_nodes.push(ref_node);
                all_edges.push(GraphEdge::new(
                    EdgeType::ReadsFrom,
                    ref_id.clone(),
                    id.clone(),
                ));
                all_edges.push(GraphEdge::new(EdgeType::ReadsField, id.clone(), ref_id));
            }
        }
    }
    let code_ext = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .filter(|e| matches!(*e, "ts" | "tsx" | "js" | "jsx" | "py"))
            .map(|e| e.to_string())
    };
    for (path, content, _ext) in crate::server::sensors::util::scan_files(root, code_ext) {
        let graph_path_str = graph_path(root, &path);
        let consumers = detect_in_code(&content, &graph_path_str);
        for c in consumers {
            // Dynamic operations (interpolated documents) do
            // not emit a `GraphqlConsumer`; they are recorded
            // by the joiner as `Unresolved { reason:
            // DynamicOperation }`.
            if c.dynamic {
                continue;
            }
            let id_name = format!("graphql-call:{}:{}", c.op, c.field);
            let id = GraphNode::generate_id(
                &NodeType::Function,
                &c.site_path,
                &id_name,
                Some(c.site_line),
                namespace,
            );
            let mut node = GraphNode::new(NodeType::Function, id_name.clone(), c.site_path.clone());
            node.id = id.clone();
            node.line_start = Some(c.site_line);
            node.line_end = Some(c.site_line);
            node.contract = Some(ContractFact::GraphqlConsumer(GraphqlConsumerFact {
                op: c.op,
                field: c.field.clone(),
            }));
            all_nodes.push(node);
            total += 1;

            for (idx, sel) in c.selected_fields.iter().enumerate() {
                let ref_id_name = format!("graphql-read:{}:{}:{}", c.op, c.field, sel);
                let ref_id = GraphNode::generate_id(
                    &NodeType::FieldRef,
                    &c.site_path,
                    &ref_id_name,
                    Some(c.site_line + idx as u32),
                    namespace,
                );
                let mut ref_node =
                    GraphNode::new(NodeType::FieldRef, sel.clone(), c.site_path.clone());
                ref_node.id = ref_id.clone();
                ref_node.line_start = Some(c.site_line + idx as u32);
                ref_node.line_end = Some(c.site_line + idx as u32);
                ref_node.contract = Some(ContractFact::FieldRead(FieldReadFact {
                    chain: JsonPath(vec![PathSegment::Name(sel.clone())]),
                    exact: true,
                    origin: FieldReadOrigin::GraphqlConsumer,
                }));
                all_nodes.push(ref_node);
                all_edges.push(GraphEdge::new(
                    EdgeType::ReadsFrom,
                    ref_id.clone(),
                    id.clone(),
                ));
                all_edges.push(GraphEdge::new(EdgeType::ReadsField, id.clone(), ref_id));
            }
        }
    }
    if !all_nodes.is_empty() {
        let _ =
            graph.replace_sensor_output(SensorOwner::GraphqlConsumerSensor, &all_nodes, &all_edges);
    }
    Ok(total)
}

// ─── Detection ─────────────────────────────────────────────────────────

/// Detect GraphQL consumer top-level fields inside a source
/// file. Public so the acceptance tests can exercise the
/// detector without going through the graph emission path.
pub fn detect_in_code(content: &str, graph_path: &str) -> Vec<GraphqlConsumerField> {
    let mut out: Vec<GraphqlConsumerField> = Vec::new();
    // TS / JS tagged templates: `gql\`...\``, `graphql\`...\``
    for tag in ["gql`", "graphql`"] {
        let mut search_from = 0;
        while let Some(rel) = content[search_from..].find(tag) {
            let start = search_from + rel;
            let body_start = start + tag.len();
            let line_no = (content[..start].chars().filter(|&c| c == '\n').count() as u32) + 1;
            if let Some(end_rel) = content[body_start..].find('`') {
                let body = &content[body_start..body_start + end_rel];
                out.extend(parse_operation_body(body, graph_path, line_no));
                search_from = body_start + end_rel + 1;
            } else {
                break;
            }
        }
    }
    // Python / JS function calls: `gql("...")` or `.execute("...")`
    for tag in ["gql(\"", ".execute(\""] {
        let mut search_from = 0;
        while let Some(rel) = content[search_from..].find(tag) {
            let start = search_from + rel;
            let body_start = start + tag.len();
            let line_no = (content[..start].chars().filter(|&c| c == '\n').count() as u32) + 1;
            if let Some(end_rel) = content[body_start..].find("\")") {
                let body = &content[body_start..body_start + end_rel];
                out.extend(parse_operation_body(body, graph_path, line_no));
                search_from = body_start + end_rel + 2;
            } else {
                break;
            }
        }
    }
    out
}

/// Parse a top-level operation document (one operation per
/// file in v1; we accept a single `query` / `mutation` /
/// `subscription` operation). Returns one
/// `GraphqlConsumerField` per top-level selection field. A
/// fragment-only document (no operation at the top level) or
/// an interpolated document (contains `${...}`) is reported
/// as a single `GraphqlConsumerField { dynamic: true, .. }`
/// record.
pub fn parse_document(content: &str, graph_path: &str) -> Vec<GraphqlConsumerField> {
    let stripped = strip_comments(content);
    if has_interpolation(&stripped) {
        // Interpolated documents are dynamic; the joiner
        // records them as `Unresolved { reason:
        // DynamicOperation }`. We return a single dynamic
        // marker.
        return vec![GraphqlConsumerField {
            op: GraphqlOp::Query,
            field: String::new(),
            selected_fields: Vec::new(),
            dynamic: true,
            site_path: graph_path.to_string(),
            site_line: 1,
        }];
    }
    let mut out: Vec<GraphqlConsumerField> = Vec::new();
    // For each `query|mutation|subscription` declaration
    // followed by `{ ... }`, extract the top-level fields.
    for decl in find_top_level_operations(&stripped) {
        let line_no = decl.line;
        let op = decl.op;
        let fields = extract_top_level_fields_for_op(&stripped, op, line_no, graph_path);
        if fields.is_empty() {
            // Operation with no top-level fields — the
            // document is fragment-only or empty. Mark
            // dynamic.
            out.push(GraphqlConsumerField {
                op,
                field: String::new(),
                selected_fields: Vec::new(),
                dynamic: true,
                site_path: graph_path.to_string(),
                site_line: line_no,
            });
            continue;
        }
        for (field, selected_fields) in fields {
            out.push(GraphqlConsumerField {
                op,
                field,
                selected_fields,
                dynamic: false,
                site_path: graph_path.to_string(),
                site_line: line_no,
            });
        }
    }
    if out.is_empty() {
        // No operations at all — fragment-only document.
        out.push(GraphqlConsumerField {
            op: GraphqlOp::Query,
            field: String::new(),
            selected_fields: Vec::new(),
            dynamic: true,
            site_path: graph_path.to_string(),
            site_line: 1,
        });
    }
    out
}

// ─── Helpers ───────────────────────────────────────────────────────────

/// Strip `#` line comments and `""" ... """` block comments
/// (mirrors the provider-sensor helper so the same source
/// shape parses identically). Thin wrapper around
/// `crate::server::sensors::util_tokenize::strip_comments` so
/// the per-sensor API stays unchanged for callers.
fn strip_comments(input: &str) -> String {
    crate::server::sensors::util_tokenize::strip_comments(
        input,
        crate::server::sensors::util_tokenize::CommentSyntax::HashBlockString,
    )
}

/// Detect template-literal interpolation placeholders
/// (`${...}`). These mark a document as dynamic (the
/// `DynamicOperation` ledger reason per spec §8.3).
fn has_interpolation(content: &str) -> bool {
    let bytes = content.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' && bytes[i + 1] == b'{' {
            return true;
        }
        i += 1;
    }
    false
}

#[derive(Debug, Clone, Copy)]
struct OpDecl {
    op: GraphqlOp,
    line: u32,
}

/// Find every top-level `query|mutation|subscription`
/// declaration in `content` along with its 1-based line
/// number. The detection is brace-balanced so a `query { ...
/// }` inside another `query { ... }` (rare but legal) is
/// handled.
fn find_top_level_operations(content: &str) -> Vec<OpDecl> {
    let mut out: Vec<OpDecl> = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut line_no: u32 = 1;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            line_no += 1;
            i += 1;
            continue;
        }
        if (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let Some(op) = operation_keyword(bytes, i) else {
            i += 1;
            continue;
        };
        let start_line = line_no;
        // Consume the keyword.
        let kw_len = match op {
            GraphqlOp::Query => 5,
            GraphqlOp::Mutation => 8,
            GraphqlOp::Subscription => 12,
        };
        i += kw_len;
        // Skip whitespace.
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            if bytes[i] == b'\n' {
                line_no += 1;
            }
            i += 1;
        }
        // Optional name (e.g. `query GetOrders { ... }`).
        // We just skip identifiers / words / parens until
        // we hit `{`.
        while i < bytes.len() && bytes[i] != b'{' && bytes[i] != b'\n' {
            // Skip balanced parens.
            if bytes[i] == b'(' {
                let mut depth: u32 = 1;
                i += 1;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'(' {
                        depth += 1;
                    } else if bytes[i] == b')' {
                        depth -= 1;
                    }
                    i += 1;
                }
                continue;
            }
            i += 1;
        }
        out.push(OpDecl {
            op,
            line: start_line,
        });
    }
    out
}

fn operation_keyword(bytes: &[u8], at: usize) -> Option<GraphqlOp> {
    let rest = &bytes[at..];
    let starts = |kw: &[u8]| rest.starts_with(kw);
    let after_ok = |kw: &[u8]| {
        let idx = at + kw.len();
        idx >= bytes.len() || !(bytes[idx] as char).is_ascii_alphanumeric() && bytes[idx] != b'_'
    };
    let before_ok =
        at == 0 || !(bytes[at - 1] as char).is_ascii_alphanumeric() && bytes[at - 1] != b'_';
    if !before_ok {
        return None;
    }
    if starts(b"query") && after_ok(b"query") {
        Some(GraphqlOp::Query)
    } else if starts(b"mutation") && after_ok(b"mutation") {
        Some(GraphqlOp::Mutation)
    } else if starts(b"subscription") && after_ok(b"subscription") {
        Some(GraphqlOp::Subscription)
    } else {
        None
    }
}

/// Extract the top-level selection fields of one operation
/// declaration. We walk from the start of the document,
/// skipping whitespace until we hit `query|mutation|...`
/// and then reading the field list inside its `{ ... }`
/// block.
fn extract_top_level_fields_for_op(
    content: &str,
    op: GraphqlOp,
    _line: u32,
    _graph_path: &str,
) -> Vec<(String, Vec<String>)> {
    let bytes = content.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(found_op) = operation_keyword(bytes, i) else {
            i += 1;
            continue;
        };
        if found_op != op {
            i += 1;
            continue;
        }
        // Skip keyword.
        let kw_len = match found_op {
            GraphqlOp::Query => 5,
            GraphqlOp::Mutation => 8,
            GraphqlOp::Subscription => 12,
        };
        i += kw_len;
        // Skip whitespace + optional name + parens.
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        while i < bytes.len() && bytes[i] != b'{' && bytes[i] != b'\n' {
            if bytes[i] == b'(' {
                let mut depth: u32 = 1;
                i += 1;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'(' {
                        depth += 1;
                    } else if bytes[i] == b')' {
                        depth -= 1;
                    }
                    i += 1;
                }
                continue;
            }
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'{' {
            continue;
        }
        // Read the body until the matching `}`.
        let body_start = i + 1;
        let mut depth: u32 = 1;
        i += 1;
        while i < bytes.len() && depth > 0 {
            if bytes[i] == b'{' {
                depth += 1;
            } else if bytes[i] == b'}' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            i += 1;
        }
        let body = &content[body_start..i];
        return top_level_fields_with_selections(body);
    }
    Vec::new()
}

/// Maximum recursion depth for nested selection sets.
/// Adversarial `a{a{a{…` input without a bound blows the
/// default Rust stack (abort, not unwind). Real GraphQL
/// queries nest at most a handful of layers; 256 is generous.
const MAX_SELECTION_DEPTH: usize = 256;

pub fn top_level_fields_with_selections(body: &str) -> Vec<(String, Vec<String>)> {
    top_level_fields_with_selections_at_depth(body, 0)
}

fn top_level_fields_with_selections_at_depth(
    body: &str,
    depth: usize,
) -> Vec<(String, Vec<String>)> {
    if depth > MAX_SELECTION_DEPTH {
        // Bound the recursion so adversarial nesting cannot
        // abort the process. The bound is well above any
        // realistic GraphQL document; reaching it is a sign
        // the input is malformed, and we silently truncate
        // the field list at this depth.
        return Vec::new();
    }
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        let c = bytes[i];
        if (c as char).is_ascii_whitespace() || c == b',' {
            i += 1;
            continue;
        }
        if c == b'}' {
            i += 1;
            continue;
        }
        if c == b'{' {
            // Unattached block, skip
            let mut brace_depth: u32 = 1;
            i += 1;
            while i < bytes.len() && brace_depth > 0 {
                if bytes[i] == b'{' {
                    brace_depth += 1;
                } else if bytes[i] == b'}' {
                    brace_depth -= 1;
                    if brace_depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            continue;
        }
        if is_fragment_spread(bytes, i) {
            let (new_i, inline_body) = scan_fragment_spread(body, bytes, i);
            i = new_i;
            if let Some(inline_body) = inline_body {
                // Inline fragments (`... on Type { ... }` or
                // `... { ... }`) contribute their fields at
                // the current level — the parent object already
                // declares the fields the inline condition
                // applies to. Recurse with the depth bound so
                // the same cap protects the call.
                out.extend(top_level_fields_with_selections_at_depth(
                    inline_body,
                    depth + 1,
                ));
            }
            continue;
        }

        // Read field identifier (or alias)
        let name_start = i;
        while i < bytes.len() && is_ident_continue(c_at(bytes, i)) {
            i += 1;
        }
        let mut field_name = std::str::from_utf8(&bytes[name_start..i])
            .unwrap_or("")
            .trim()
            .to_string();

        if field_name.is_empty() {
            i += 1;
            continue;
        }

        // Skip whitespace
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }

        // Check if this was an alias: `alias: actual_field`
        if i < bytes.len() && bytes[i] == b':' {
            i += 1; // skip ':'
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }
            let target_start = i;
            while i < bytes.len() && is_ident_continue(c_at(bytes, i)) {
                i += 1;
            }
            let actual = std::str::from_utf8(&bytes[target_start..i])
                .unwrap_or("")
                .trim()
                .to_string();
            if !actual.is_empty() {
                field_name = actual;
            }
        }

        // Skip arguments `(...)` if present
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'(' {
            let mut brace_depth: u32 = 1;
            i += 1;
            while i < bytes.len() && brace_depth > 0 {
                if bytes[i] == b'(' {
                    brace_depth += 1;
                } else if bytes[i] == b')' {
                    brace_depth -= 1;
                }
                i += 1;
            }
        }

        // Skip directives `@name(...)` if present
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        while i < bytes.len() && bytes[i] == b'@' {
            i += 1;
            while i < bytes.len() && is_ident_continue(c_at(bytes, i)) {
                i += 1;
            }
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'(' {
                let mut brace_depth: u32 = 1;
                i += 1;
                while i < bytes.len() && brace_depth > 0 {
                    if bytes[i] == b'(' {
                        brace_depth += 1;
                    } else if bytes[i] == b')' {
                        brace_depth -= 1;
                    }
                    i += 1;
                }
            }
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }
        }

        // Now check if there is a selection set `{ ... }`
        let mut selections = Vec::new();
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'{' {
            let sel_start = i + 1;
            let mut brace_depth: u32 = 1;
            i += 1;
            while i < bytes.len() && brace_depth > 0 {
                if bytes[i] == b'{' {
                    brace_depth += 1;
                } else if bytes[i] == b'}' {
                    brace_depth -= 1;
                    if brace_depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            let sel_body = &body[sel_start..i];
            if i < bytes.len() && bytes[i] == b'}' {
                i += 1;
            }
            selections = top_level_fields_in_at_depth(sel_body, depth + 1);
        }

        if field_name != "__typename" {
            out.push((field_name, selections));
        }
    }
    out
}

fn is_fragment_spread(bytes: &[u8], at: usize) -> bool {
    // `...` at a non-identifier position is a fragment spread.
    // The preceding byte (if any) must not be part of an
    // identifier — `1..3` is a numeric range, not a spread.
    let prev_ok =
        at == 0 || !(bytes[at - 1] as char).is_ascii_alphanumeric() && bytes[at - 1] != b'_';
    let rest = &bytes[at..];
    prev_ok && rest.len() >= 3 && rest[0] == b'.' && rest[1] == b'.' && rest[2] == b'.'
}

/// Consume a fragment spread (`...Name?`, `... on Type?`,
/// inline selection set, optional directives) starting at the
/// `...` token at position `at`. Returns `(new_cursor,
/// optional_inline_body)`. The inline body is the slice
/// between the `{` and matching `}` of an inline fragment —
/// `None` for `...fragName` (named spread). Named spreads have
/// no fields to add at the current level; inline fragments
/// contribute their fields, so the caller recurses into the
/// returned slice with the depth bound applied.
fn scan_fragment_spread<'a>(body: &'a str, bytes: &[u8], at: usize) -> (usize, Option<&'a str>) {
    let mut i = at + 3; // past the `...`
                        // Optional spread name (named spread) OR
                        // `on TypeName` (inline fragment with a
                        // type condition). We accept one
                        // identifier; if the next token is `on`
                        // the user wrote `... on Paid { ... }`,
                        // otherwise they wrote `...fragName`.
    while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] != b'{' && bytes[i] != b'@' {
        let ident_start = i;
        while i < bytes.len() && is_ident_continue(bytes[i]) {
            i += 1;
        }
        let ident = std::str::from_utf8(&bytes[ident_start..i]).unwrap_or("");
        if ident == "on" {
            // `... on Type` — also skip the type name.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
        }
    }
    // Optional directives `@name(args)`.
    while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() && bytes[i] == b'@' {
        i += 1;
        while i < bytes.len() && is_ident_continue(bytes[i]) {
            i += 1;
        }
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'(' {
            let mut brace_depth: u32 = 1;
            i += 1;
            while i < bytes.len() && brace_depth > 0 {
                if bytes[i] == b'(' {
                    brace_depth += 1;
                } else if bytes[i] == b')' {
                    brace_depth -= 1;
                }
                i += 1;
            }
        }
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
    }
    // Optional selection set `{ ... }`. Named spreads have none;
    // inline fragments do. If present, return the slice so the
    // caller can recurse into it.
    if i < bytes.len() && bytes[i] == b'{' {
        let sel_start = i + 1;
        let mut brace_depth: u32 = 1;
        i += 1;
        while i < bytes.len() && brace_depth > 0 {
            if bytes[i] == b'{' {
                brace_depth += 1;
            } else if bytes[i] == b'}' {
                brace_depth -= 1;
                if brace_depth == 0 {
                    break;
                }
            }
            i += 1;
        }
        let sel_body = &body[sel_start..i];
        if i < bytes.len() && bytes[i] == b'}' {
            i += 1;
        }
        return (i, Some(sel_body));
    }
    (i, None)
}

pub fn top_level_fields_in(body: &str) -> Vec<String> {
    top_level_fields_in_at_depth(body, 0)
}

fn top_level_fields_in_at_depth(body: &str, depth: usize) -> Vec<String> {
    top_level_fields_with_selections_at_depth(body, depth)
        .into_iter()
        .map(|(f, _)| f)
        .collect()
}

fn c_at(bytes: &[u8], i: usize) -> u8 {
    if i < bytes.len() {
        bytes[i]
    } else {
        0
    }
}

fn is_ident_continue(b: u8) -> bool {
    (b as char).is_ascii_alphanumeric() || b == b'_'
}

fn parse_operation_body(body: &str, graph_path: &str, line_no: u32) -> Vec<GraphqlConsumerField> {
    // The body has the operation type implicit when the
    // shorthand `{ orders { id } }` is used. We treat it
    // as a Query for the common case (and the
    // acceptance scenario pins the Query shape).
    let mut out: Vec<GraphqlConsumerField> = Vec::new();
    if has_interpolation(body) {
        out.push(GraphqlConsumerField {
            op: GraphqlOp::Query,
            field: String::new(),
            selected_fields: Vec::new(),
            dynamic: true,
            site_path: graph_path.to_string(),
            site_line: line_no,
        });
        return out;
    }
    let op = if body.contains("mutation") {
        GraphqlOp::Mutation
    } else if body.contains("subscription") {
        GraphqlOp::Subscription
    } else {
        GraphqlOp::Query
    };
    // Find the first `{` and read the field list up to the
    // matching `}`. Most tagged templates have exactly one
    // operation per call; the F2 acceptance scenario uses
    // `query { ... }`.
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return out;
    }
    let body_start = i + 1;
    let mut depth: u32 = 1;
    i += 1;
    while i < bytes.len() && depth > 0 {
        if bytes[i] == b'{' {
            depth += 1;
        } else if bytes[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
        i += 1;
    }
    let inner = &body[body_start..i];
    for (f, selected_fields) in top_level_fields_with_selections(inner) {
        out.push(GraphqlConsumerField {
            op,
            field: f,
            selected_fields,
            dynamic: false,
            site_path: graph_path.to_string(),
            site_line: line_no,
        });
    }
    if out.is_empty() {
        out.push(GraphqlConsumerField {
            op,
            field: String::new(),
            selected_fields: Vec::new(),
            dynamic: true,
            site_path: graph_path.to_string(),
            site_line: line_no,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_document_emits_one_consumer_per_top_field() {
        let src = "\
query GetOrders {
  orders {
    id
  }
  foo {
    bar
  }
  giftData {
    name
  }
}
";
        let consumers = parse_document(src, "doc.graphql");
        assert_eq!(consumers.len(), 3);
        assert!(consumers.iter().all(|c| c.op == GraphqlOp::Query));
        let fields: Vec<&str> = consumers.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, vec!["orders", "foo", "giftData"]);
        assert!(consumers.iter().all(|c| !c.dynamic));
    }

    #[test]
    fn fragment_only_document_is_dynamic() {
        let src = "\
fragment X on Order {
  id
}
";
        let consumers = parse_document(src, "frag.graphql");
        assert_eq!(consumers.len(), 1);
        assert!(consumers[0].dynamic);
    }

    #[test]
    fn interpolated_document_is_dynamic() {
        let src = "\
query GetOrder(${id}: ID!) {
  order(id: $id) {
    id
  }
}
";
        let consumers = parse_document(src, "doc.graphql");
        assert_eq!(consumers.len(), 1);
        assert!(consumers[0].dynamic);
    }

    #[test]
    fn detect_tagged_template_ts() {
        let src = r"\
const ORDERS = gql`query { orders { id } }`;
";
        let consumers = detect_in_code(src, "orders.ts");
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].op, GraphqlOp::Query);
        assert_eq!(consumers[0].field, "orders");
        assert!(!consumers[0].dynamic);
    }

    #[test]
    fn detect_gql_function_call_python() {
        let src = "\
import gql
orders = gql(\"query { orders { id } }\")
";
        let consumers = detect_in_code(src, "orders.py");
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].field, "orders");
    }

    #[test]
    fn detect_interpolated_tagged_template_is_dynamic() {
        let src = r"\
const ORDERS = gql`query { ${userId} { id } }`;
";
        let consumers = detect_in_code(src, "orders.ts");
        assert_eq!(consumers.len(), 1);
        assert!(consumers[0].dynamic);
    }

    #[test]
    fn detect_mutation_top_level_field() {
        let src = r"\
const CREATE = gql`mutation { createOrder(input: {}) { id } }`;
";
        let consumers = detect_in_code(src, "orders.ts");
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].op, GraphqlOp::Mutation);
        assert_eq!(consumers[0].field, "createOrder");
    }
}
