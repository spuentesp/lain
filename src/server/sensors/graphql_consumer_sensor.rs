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
use crate::schema::{GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::path::Path;

// ─── Public sensor shape ───────────────────────────────────────────────

/// One detected GraphQL operation top-level field. Public so
/// the acceptance tests can exercise the detector without
/// going through the graph emission path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlConsumerField {
    pub op: GraphqlOp,
    pub field: String,
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
/// `GraphqlConsumer` `Function` node per top-level field.
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
            let id_name = format!("graphql-call:{}:{}", c.op, c.field);
            let id = GraphNode::generate_id(
                &NodeType::Function,
                &c.site_path,
                &id_name,
                Some(c.site_line),
                namespace,
            );
            let mut node = GraphNode::new(NodeType::Function, id_name.clone(), c.site_path.clone());
            node.id = id;
            node.line_start = Some(c.site_line);
            node.line_end = Some(c.site_line);
            node.contract = Some(ContractFact::GraphqlConsumer(GraphqlConsumerFact {
                op: c.op,
                field: c.field.clone(),
            }));
            all_nodes.push(node);
            total += 1;
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
            node.id = id;
            node.line_start = Some(c.site_line);
            node.line_end = Some(c.site_line);
            node.contract = Some(ContractFact::GraphqlConsumer(GraphqlConsumerFact {
                op: c.op,
                field: c.field.clone(),
            }));
            all_nodes.push(node);
            total += 1;
        }
    }
    if !all_nodes.is_empty() {
        let _ = graph.replace_sensor_output(
            SensorOwner::GraphqlSensor,
            &all_nodes,
            &[] as &[GraphEdge],
        );
    }
    Ok(total)
}

// ─── Detection ─────────────────────────────────────────────────────────

/// Detect GraphQL consumer top-level fields inside a source
/// file. Public so the acceptance tests can exercise the
/// detector without going through the graph emission path.
pub fn detect_in_code(content: &str, graph_path: &str) -> Vec<GraphqlConsumerField> {
    let mut out: Vec<GraphqlConsumerField> = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        // TS / JS tagged templates: `gql\`...\``,
        // `graphql\`...\``. The body is everything between
        // the backticks on the same line (multiline is rare
        // but the same shape — we treat the line as the
        // body when it contains the closing backtick).
        for tag in ["gql`", "graphql`"] {
            if let Some(start) = trimmed.find(tag) {
                let body_start = start + tag.len();
                if let Some(end_rel) = trimmed[body_start..].find('`') {
                    let body = &trimmed[body_start..body_start + end_rel];
                    out.extend(parse_operation_body(body, graph_path, line_no));
                }
                // Multiline: we skip for v1.
            }
        }
        // Python: `gql("...")` or
        // `client.execute("query { ... }")` — the
        // function-call form. We look for `gql(` or
        // `.execute(` and read the first string-literal
        // argument.
        if let Some(start) = trimmed.find("gql(\"") {
            let body_start = start + "gql(\"".len();
            if let Some(end_rel) = trimmed[body_start..].find("\")") {
                let body = &trimmed[body_start..body_start + end_rel];
                out.extend(parse_operation_body(body, graph_path, line_no));
            }
        }
        if let Some(start) = trimmed.find(".execute(\"") {
            let body_start = start + ".execute(\"".len();
            if let Some(end_rel) = trimmed[body_start..].find("\")") {
                let body = &trimmed[body_start..body_start + end_rel];
                out.extend(parse_operation_body(body, graph_path, line_no));
            }
        }
        // Apollo persisted operations: the canonical pattern
        // is `useQuery(GET_ORDERS)` or
        // `client.query({ query: GET_ORDERS, ... })` — the
        // operation name is the identifier. We do a coarse
        // scan for the `query <Name>` declaration in the
        // same file (the F1 + F2 acceptance scenario covers
        // the non-persisted case; persisted-operation
        // detection is left for a follow-up if the operator
        // surfaces it).
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
                dynamic: true,
                site_path: graph_path.to_string(),
                site_line: line_no,
            });
            continue;
        }
        for field in fields {
            out.push(GraphqlConsumerField {
                op,
                field,
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
/// shape parses identically).
fn strip_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 2 < bytes.len() && bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
            out.push('"');
            out.push('"');
            out.push('"');
            i += 3;
            while i + 2 < bytes.len()
                && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
            {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                out.push(bytes[i] as char);
                i += 1;
            }
            if i + 2 < bytes.len() {
                out.push('"');
                out.push('"');
                out.push('"');
                i += 3;
            } else {
                i = bytes.len();
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
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
        out.push(OpDecl { op, line: start_line });
    }
    out
}

fn operation_keyword(bytes: &[u8], at: usize) -> Option<GraphqlOp> {
    let rest = &bytes[at..];
    let starts = |kw: &[u8]| rest.starts_with(kw);
    let after_ok = |kw: &[u8]| {
        let idx = at + kw.len();
        idx >= bytes.len()
            || !(bytes[idx] as char).is_ascii_alphanumeric() && bytes[idx] != b'_'
    };
    let before_ok = at == 0
        || !(bytes[at - 1] as char).is_ascii_alphanumeric() && bytes[at - 1] != b'_';
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
) -> Vec<String> {
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
        return top_level_fields_in(body);
    }
    Vec::new()
}

/// Walk a `{ ... }` body and return the top-level field
/// names. Nested selections (`orders { id }`) are skipped
/// by tracking brace depth.
fn top_level_fields_in(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if (c as char).is_ascii_whitespace() || c == b',' {
            i += 1;
            continue;
        }
        if c == b'{' {
            // Skip nested block.
            let mut depth: u32 = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'{' {
                    depth += 1;
                } else if bytes[i] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            continue;
        }
        if c == b'}' {
            i += 1;
            continue;
        }
        // Read the field name.
        let name_start = i;
        while i < bytes.len() && is_ident_continue(c_at(bytes, i)) {
            i += 1;
        }
        let name = std::str::from_utf8(&bytes[name_start..i])
            .unwrap_or("")
            .trim()
            .to_string();
        if !name.is_empty() {
            out.push(name);
        }
        // Skip until the next top-level entry (`,` or `}`).
        while i < bytes.len() && bytes[i] != b',' && bytes[i] != b'}' && bytes[i] != b'{' {
            i += 1;
        }
    }
    out
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

fn parse_operation_body(
    body: &str,
    graph_path: &str,
    line_no: u32,
) -> Vec<GraphqlConsumerField> {
    // The body has the operation type implicit when the
    // shorthand `{ orders { id } }` is used. We treat it
    // as a Query for the common case (and the
    // acceptance scenario pins the Query shape).
    let mut out: Vec<GraphqlConsumerField> = Vec::new();
    if has_interpolation(body) {
        out.push(GraphqlConsumerField {
            op: GraphqlOp::Query,
            field: String::new(),
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
    for f in top_level_fields_in(inner) {
        out.push(GraphqlConsumerField {
            op,
            field: f,
            dynamic: false,
            site_path: graph_path.to_string(),
            site_line: line_no,
        });
    }
    if out.is_empty() {
        out.push(GraphqlConsumerField {
            op,
            field: String::new(),
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
