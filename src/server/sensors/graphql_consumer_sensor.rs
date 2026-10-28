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
use crate::federation::contracts::model::{ContractFact, GraphqlConsumerFact, GraphqlOp};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, RepoNamespace, StaticSource};
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
    /// The operation's declared name (`query ProductDetails { … }` →
    /// `ProductDetails`); `None` for anonymous operations. Codegen names
    /// its document / hook identifiers after it, which is how
    /// [`link_generated_documents`] finds the code that runs the call.
    pub operation_name: Option<String>,
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
    // `<Operation>Document` identifier → consumer node ids of that operation.
    let mut documents: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
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
            let id_name = format!(
                "{}{}:{}",
                crate::server::sensors::util::GRAPHQL_CALL_PREFIX,
                c.op,
                c.field
            );
            let mut node = crate::server::sensors::util::synthetic_site_node(
                id_name,
                &c.site_path,
                c.site_line,
                namespace,
            );
            node.contract = vec![ContractFact::GraphqlConsumer(GraphqlConsumerFact {
                op: c.op,
                field: c.field.clone(),
            })];
            let id = node.id.clone();
            all_nodes.push(node);
            total += 1;
            if let Some(name) = &c.operation_name {
                for ident in document_identifiers(name) {
                    documents.entry(ident).or_default().push(id.clone());
                }
            }

            crate::server::sensors::util::emit_graphql_field_refs(
                crate::server::sensors::util::GraphqlFieldRefs {
                    consumer_id: &id,
                    op: c.op,
                    field: &c.field,
                    site_path: &c.site_path,
                    site_line: c.site_line,
                    selected_fields: &c.selected_fields,
                    namespace,
                },
                &mut all_nodes,
                &mut all_edges,
            );
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
            let id_name = format!(
                "{}{}:{}",
                crate::server::sensors::util::GRAPHQL_CALL_PREFIX,
                c.op,
                c.field
            );
            let mut node = crate::server::sensors::util::synthetic_site_node(
                id_name,
                &c.site_path,
                c.site_line,
                namespace,
            );
            node.contract = vec![ContractFact::GraphqlConsumer(GraphqlConsumerFact {
                op: c.op,
                field: c.field.clone(),
            })];
            let id = node.id.clone();
            all_nodes.push(node);
            total += 1;
            if let Some(name) = &c.operation_name {
                for ident in document_identifiers(name) {
                    documents.entry(ident).or_default().push(id.clone());
                }
            }

            crate::server::sensors::util::emit_graphql_field_refs(
                crate::server::sensors::util::GraphqlFieldRefs {
                    consumer_id: &id,
                    op: c.op,
                    field: &c.field,
                    site_path: &c.site_path,
                    site_line: c.site_line,
                    selected_fields: &c.selected_fields,
                    namespace,
                },
                &mut all_nodes,
                &mut all_edges,
            );
        }
    }
    link_generated_documents(graph, root, &documents, &mut all_edges);
    graph.replace_sensor_output(SensorOwner::GraphqlConsumerSensor, &all_nodes, &all_edges)?;
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
            operation_name: None,
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
                operation_name: decl.name.clone(),
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
                operation_name: decl.name.clone(),
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
            operation_name: None,
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

#[derive(Debug, Clone)]
struct OpDecl {
    op: GraphqlOp,
    line: u32,
    name: Option<String>,
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
        let name = leading_identifier(&content[i..]);
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
            name,
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

/// Remove each `fragment <Name> on <Type> { ... }` definition whose `<Type>` is
/// not `Query`, `Mutation` or `Subscription`. A fragment on one of those root
/// types keeps its text, because its top-level fields are root fields.
fn without_non_root_fragments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = find_fragment_keyword(rest) {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let Some(open) = after.find('{') else {
            break;
        };
        let root_type = matches!(
            after[..open].split_whitespace().nth(3),
            Some("Query" | "Mutation" | "Subscription")
        );
        if root_type {
            out.push_str(&after[..=open]);
            rest = &after[open + 1..];
            continue;
        }
        let mut depth: u32 = 0;
        let mut close = None;
        for (i, b) in after.bytes().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = close else {
            break;
        };
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Byte offset of the next `fragment` keyword in `text`, on a word boundary.
fn find_fragment_keyword(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    text.match_indices("fragment").map(|(i, _)| i).find(|&i| {
        let before_ok = i == 0 || !is_ident_continue(bytes[i - 1]);
        let after_ok = bytes
            .get(i + "fragment".len())
            .is_some_and(|b| b.is_ascii_whitespace());
        before_ok && after_ok
    })
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
            operation_name: None,
        });
        return out;
    }
    // A fragment on an object type is not a root operation: its fields are
    // reached through the operation that spreads it. Strip those first, so
    // the first `{` below is the operation's, whatever order they come in.
    let body = without_non_root_fragments(body);
    let body = body.as_str();
    let operation_name = first_operation_name(body);
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
            operation_name: operation_name.clone(),
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
            operation_name: None,
        });
    }
    out
}

/// The identifier at the very start of `s`, if any (`GetOrders($id: ID)` →
/// `GetOrders`). `None` when `s` starts with `{`, `(` or `@`.
fn leading_identifier(s: &str) -> Option<String> {
    let name: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit())).then_some(name)
}

/// Name of the first `query|mutation|subscription <Name>` in `body`.
fn first_operation_name(body: &str) -> Option<String> {
    let bytes = body.as_bytes();
    (0..bytes.len())
        .filter_map(|i| operation_keyword(bytes, i).map(|op| (i, op)))
        .find_map(|(i, op)| {
            let kw_len = match op {
                GraphqlOp::Query => 5,
                GraphqlOp::Mutation => 8,
                GraphqlOp::Subscription => 12,
            };
            leading_identifier(body[i + kw_len..].trim_start())
        })
}

/// Codegen spells a document constant `<Operation>Document`
/// (typed-document-node / apollo) or `<Operation>Doc`.
fn document_identifiers(operation_name: &str) -> [String; 2] {
    [
        format!("{operation_name}Document"),
        format!("{operation_name}Doc"),
    ]
}

/// Whole-identifier occurrences of any key of `wanted` in `content`, as
/// `(1-based line, key)`. A longer identifier that merely contains a key
/// (`MyProductDetailsDocument`) is not a match.
fn find_identifier_refs<'a>(
    content: &str,
    wanted: &'a std::collections::BTreeMap<String, Vec<String>>,
) -> Vec<(u32, &'a str)> {
    let bytes = content.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut out = Vec::new();
    let (mut i, mut line) = (0usize, 1u32);
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            line += 1;
            i += 1;
        } else if is_ident(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_ident(bytes[i]) {
                i += 1;
            }
            if let Some((k, _)) = wanted.get_key_value(&content[start..i]) {
                out.push((line, k.as_str()));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Link the functions that use a generated document to the consumer nodes
/// of that document: every function/method whose body names
/// `<Operation>Document` (or `<Operation>Doc`) gets a `Calls` edge to the
/// `graphql-call:` node(s) of the operation. Generated hooks reference the
/// document in their body, so component → hook → consumer chains follow
/// from the hook's existing `Calls` edges. Name-matched, hence `Regex`
/// provenance and weight 0.8.
fn link_generated_documents(
    graph: &GraphDatabase,
    root: &Path,
    documents: &std::collections::BTreeMap<String, Vec<String>>,
    edges: &mut Vec<GraphEdge>,
) {
    if documents.is_empty() {
        return;
    }
    let js_ext = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .filter(|e| matches!(*e, "ts" | "tsx" | "js" | "jsx"))
            .map(|e| e.to_string())
    };
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    for (path, content, _ext) in crate::server::sensors::util::scan_files(root, js_ext) {
        let graph_path_str = graph_path(root, &path);
        for (line, ident) in find_identifier_refs(&content, documents) {
            let Some(user) =
                crate::server::sensors::util::enclosing_symbol(graph, &graph_path_str, line)
            else {
                continue;
            };
            for consumer_id in &documents[ident] {
                if !seen.insert((user.id.clone(), consumer_id.clone())) {
                    continue;
                }
                let mut edge =
                    GraphEdge::new(EdgeType::Calls, user.id.clone(), consumer_id.clone());
                edge.provenance = Some(EdgeProvenance::Static {
                    source: StaticSource::Regex,
                });
                edge.weight = Some(0.8);
                edges.push(edge);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_fields(ts: &str) -> Vec<String> {
        detect_in_code(ts, "shop.ts")
            .into_iter()
            .map(|f| f.field)
            .collect()
    }

    #[test]
    fn a_fragment_only_template_names_no_root_field() {
        let ts = "const P = gql`\n  fragment ProductFields on Product {\n    id\n    name\n    slug\n  }\n`;\n";
        assert!(root_fields(ts).is_empty(), "got {:?}", root_fields(ts));
    }

    #[test]
    fn a_fragment_after_the_operation_adds_no_root_field() {
        let ts = "const Q = gql`\n  query GetProduct { product { ...ProductFields } }\n  fragment ProductFields on Product { id name slug }\n`;\n";
        assert_eq!(root_fields(ts), vec!["product".to_string()]);
    }

    #[test]
    fn a_fragment_before_the_operation_adds_no_root_field() {
        let ts = "const Q = gql`\n  fragment ProductFields on Product { id name slug }\n  query GetProduct { product { ...ProductFields } }\n`;\n";
        assert_eq!(root_fields(ts), vec!["product".to_string()]);
    }

    #[test]
    fn a_fragment_on_query_keeps_its_root_fields() {
        let ts = "const Q = gql`\n  fragment CartRoot on Query { cart(id: $id) { id } }\n`;\n";
        assert_eq!(root_fields(ts), vec!["cart".to_string()]);
    }

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

    // ─── operation names + generated-document links ─────────────────

    #[test]
    fn operation_name_is_captured_from_documents_and_templates() {
        let doc = parse_document(
            "query ProductDetails($id: ID!) {\n  product(id: $id) { id }\n}\n",
            "ProductDetails.graphql",
        );
        assert_eq!(doc[0].operation_name.as_deref(), Some("ProductDetails"));
        let tpl = detect_in_code(
            "const Q = gql`mutation CreateOrder { createOrder(input: {}) { id } }`;\n",
            "orders.ts",
        );
        assert_eq!(tpl[0].operation_name.as_deref(), Some("CreateOrder"));
        let anon = parse_document("query { orders { id } }", "a.graphql");
        assert_eq!(anon[0].operation_name, None);
    }

    #[test]
    fn identifier_refs_match_whole_identifiers_only() {
        let mut wanted = std::collections::BTreeMap::new();
        wanted.insert("ProductDetailsDocument".to_string(), vec!["c".to_string()]);
        let src = "a(ProductDetailsDocument)\nMyProductDetailsDocument\nProductDetailsDocumentX\n  ProductDetailsDocument,";
        let refs = find_identifier_refs(src, &wanted);
        assert_eq!(refs.iter().map(|(l, _)| *l).collect::<Vec<_>>(), vec![1, 4]);
    }

    fn fn_node(name: &str, path: &str, start: u32, end: u32) -> GraphNode {
        let ns = RepoNamespace::for_test();
        let mut n = GraphNode::new_in(
            crate::schema::NodeType::Function,
            name.into(),
            path.into(),
            &ns,
        );
        n.line_start = Some(start);
        n.line_end = Some(end);
        n.id = GraphNode::generate_id(
            &crate::schema::NodeType::Function,
            path,
            name,
            Some(start),
            &ns,
        );
        n
    }

    const DOC: &str = "query ProductDetails($id: ID!) {\n  product(id: $id) { id name }\n}\n";
    const USER_TS: &str = "import { ProductDetailsDocument } from './generated';\n\nexport async function getProductData(id: string) {\n  return run(ProductDetailsDocument, { id });\n}\n\nexport function unrelated() {\n  return MyProductDetailsDocument;\n}\n";

    fn calls_to_consumer(g: &GraphDatabase, from: &GraphNode) -> Vec<GraphNode> {
        g.get_edges_from(&from.id)
            .unwrap()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::Calls)
            .filter_map(|e| g.get_node(&e.target_id).ok().flatten())
            .filter(|n| n.name.starts_with("graphql-call:"))
            .collect()
    }

    #[test]
    fn function_using_the_generated_document_calls_the_consumer_node() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/ProductDetails.graphql"), DOC).unwrap();
        std::fs::write(tmp.path().join("src/user.ts"), USER_TS).unwrap();
        let g = GraphDatabase::new(&tmp.path().join("db.bin")).unwrap();
        let user = fn_node("getProductData", "src/user.ts", 3, 5);
        let other = fn_node("unrelated", "src/user.ts", 7, 9);
        g.insert_nodes_batch(&[user.clone(), other.clone()])
            .unwrap();
        let ns = RepoNamespace::for_test();

        scan_workspace_graphql_consumer(&g, tmp.path(), &ns).unwrap();
        // Rescan: still exactly one edge.
        scan_workspace_graphql_consumer(&g, tmp.path(), &ns).unwrap();

        let edges = calls_to_consumer(&g, &user);
        assert_eq!(
            edges.len(),
            1,
            "getProductData → graphql-call:query:product"
        );
        assert_eq!(edges[0].name, "graphql-call:query:product");
        assert!(
            calls_to_consumer(&g, &other).is_empty(),
            "a longer identifier containing the document name must not link"
        );
    }

    #[test]
    fn no_document_reference_means_no_edge() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("ProductDetails.graphql"), DOC).unwrap();
        std::fs::write(
            tmp.path().join("user.ts"),
            "export function f() {\n  return 1;\n}\n",
        )
        .unwrap();
        let g = GraphDatabase::new(&tmp.path().join("db.bin")).unwrap();
        let f = fn_node("f", "user.ts", 1, 3);
        g.insert_nodes_batch(std::slice::from_ref(&f)).unwrap();
        scan_workspace_graphql_consumer(&g, tmp.path(), &RepoNamespace::for_test()).unwrap();
        assert!(calls_to_consumer(&g, &f).is_empty());
    }

    #[test]
    fn anonymous_operations_are_never_linked() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("a.graphql"),
            "query { product(id: 1) { id } }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("user.ts"),
            "export function f() {\n  return Document;\n}\n",
        )
        .unwrap();
        let g = GraphDatabase::new(&tmp.path().join("db.bin")).unwrap();
        let f = fn_node("f", "user.ts", 1, 3);
        g.insert_nodes_batch(std::slice::from_ref(&f)).unwrap();
        scan_workspace_graphql_consumer(&g, tmp.path(), &RepoNamespace::for_test()).unwrap();
        assert!(calls_to_consumer(&g, &f).is_empty());
    }
}
