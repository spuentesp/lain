//! Phase E — GraphQL SDL provider sensor (spec §8.3).
//!
//! Recognises the per-`.graphql`/`.gql` SDL file provider surface
//! and emits one `ContractFact::GraphqlProvider { op, field,
//! return_type }` per root field on a `type Query { ... }` /
//! `Mutation` / `Subscription` block.
//!
//! Per spec §8.3's escape hatch, the parser is a focused
//! hand-rolled tokenizer (no new `graphql-parser` crate) that
//! recognises the canonical SDL shapes the test fixtures emit:
//!
//! - `schema { ... }` blocks (parse, do not emit providers)
//! - `type Query { ... }`, `type Mutation { ... }`, `type
//!   Subscription { ... }` — root types whose fields become
//!   providers
//! - `fieldName(args): ReturnType` — root field declarations
//! - `type Foo { ... }` — object types (parse, do not emit
//!   providers)
//! - `enum`, `scalar`, `input` declarations (parse, do not emit
//!   providers)
//!
//! Comments (`#` line, `""" ... """` block) are stripped so a
//! `# orders` comment inside a `type` block does not get
//! emitted as a field. Multi-line field declarations are joined
//! before parsing so `fieldName(\n  arg: Type\n): RetType` still
//! resolves to a single field.
//!
//! The `GraphqlProvider` shape is public so the acceptance
//! tests can exercise the detector directly without going
//! through the graph emission path.

use crate::error::LainError;
use crate::federation::contracts::model::{
    ContractFact, Direction, GraphqlOp, GraphqlProviderFact, SourceSite,
};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::path::Path;

// ─── Public sensor shape ───────────────────────────────────────────────

/// One GraphQL root-field provider declaration extracted from an
/// SDL file. Public so the acceptance tests can exercise the
/// detector without going through the graph emission path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlProvider {
    pub op: GraphqlOp,
    pub field: String,
    pub return_type: String,
    pub site: SourceSite,
}

/// Unit-struct Sensor impl. Registered via `inventory::submit!`
/// below; no central registry to edit.
pub struct GraphqlProviderSensor;

crate::server::sensors::register_sensor!(
    GraphqlProviderSensor,
    "graphql_provider",
    Graphql,
    scan_workspace_graphql_provider
);

// ─── Workspace scan ───────────────────────────────────────────────────

/// Walk `root`, find every `.graphql` / `.gql` SDL file, parse
/// the root types, and emit one `GraphqlProvider` `Module` node
/// per `(op, field)` declaration, plus object type `Schema` and
/// `Field` nodes linked via `ResponseSchema` and `HasField` edges.
/// Returns the count of `GraphqlProvider` nodes minted.
pub fn scan_workspace_graphql_provider(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let sdl_ext = |p: &Path| {
        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            if ext == "graphql" || ext == "gql" {
                Some(())
            } else {
                None
            }
        } else {
            None
        }
    };
    let mut total = 0usize;
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();
    for (path, content, _tag) in crate::server::sensors::util::scan_files(root, sdl_ext) {
        let graph_path_str = graph_path(root, &path);
        let providers = parse_sdl_providers(&content, &graph_path_str);
        let object_types = extract_object_type_blocks(&content);

        let mut schemas_in_file: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for obj in object_types {
            let schema_id = GraphNode::generate_id(
                &NodeType::Schema,
                &graph_path_str,
                &obj.name,
                Some(obj.line),
                namespace,
            );
            let mut schema_node =
                GraphNode::new(NodeType::Schema, obj.name.clone(), graph_path_str.clone());
            schema_node.id = schema_id.clone();
            schema_node.line_start = Some(obj.line);
            schema_node.line_end = Some(obj.line);
            schema_node.contract = Some(ContractFact::Schema {
                direction: Direction::Response,
            });
            all_nodes.push(schema_node);
            schemas_in_file.insert(obj.name.clone(), schema_id.clone());

            for field in obj.fields {
                let field_id = GraphNode::generate_id(
                    &NodeType::Field,
                    &graph_path_str,
                    &field.name,
                    Some(obj.line + field.line_offset),
                    namespace,
                );
                let mut field_node =
                    GraphNode::new(NodeType::Field, field.name.clone(), graph_path_str.clone());
                field_node.id = field_id.clone();
                field_node.line_start = Some(obj.line + field.line_offset);
                field_node.line_end = Some(obj.line + field.line_offset);
                let required = field.return_type.ends_with('!');
                field_node.contract = Some(ContractFact::Field(
                    crate::federation::contracts::model::FieldMeta {
                        ty: graphql_type_to_typedesc(&field.return_type),
                        required,
                        nullable: !required,
                        enum_values: None,
                    },
                ));
                all_nodes.push(field_node);
                all_edges.push(GraphEdge::new(
                    EdgeType::HasField,
                    schema_id.clone(),
                    field_id,
                ));
            }
        }

        for provider in providers {
            let id_name = format!("{}:{}", provider.op, provider.field);
            let id = GraphNode::generate_id(
                &NodeType::Module,
                &provider.site.path,
                &id_name,
                Some(provider.site.line),
                namespace,
            );
            let mut node = GraphNode::new(
                NodeType::Module,
                id_name.clone(),
                provider.site.path.clone(),
            );
            node.id = id.clone();
            node.line_start = Some(provider.site.line);
            node.line_end = Some(provider.site.line);
            node.contract = Some(ContractFact::GraphqlProvider(GraphqlProviderFact {
                op: provider.op,
                field: provider.field.clone(),
                return_type: provider.return_type.clone(),
            }));
            all_nodes.push(node);
            total += 1;

            let bare_ret = provider
                .return_type
                .trim()
                .trim_end_matches('!')
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .trim_end_matches('!');
            if let Some(resp_schema_id) = schemas_in_file.get(bare_ret) {
                all_edges.push(GraphEdge::new(
                    EdgeType::ResponseSchema,
                    id,
                    resp_schema_id.clone(),
                ));
            }
        }
    }
    if !all_nodes.is_empty() {
        let _ =
            graph.replace_sensor_output(SensorOwner::GraphqlProviderSensor, &all_nodes, &all_edges);
    }
    Ok(total)
}

// ─── Detection (hand-rolled SDL tokenizer) ─────────────────────────────

/// Parse an SDL file and return one `GraphqlProvider` per
/// `(op, field)` root-field declaration. `sdl_path` is the path
/// the sensor records as the source site (typically a
/// repo-relative path).
pub fn parse_sdl_providers(content: &str, sdl_path: &str) -> Vec<GraphqlProvider> {
    let stripped = strip_comments(content);
    let joined = join_continued_lines(&stripped);
    let mut out: Vec<GraphqlProvider> = Vec::new();
    for root_block in extract_root_type_blocks(&joined) {
        let op = root_block.op;
        let line_offset = root_block.line;
        for field in root_block.fields {
            out.push(GraphqlProvider {
                op,
                field: field.name,
                return_type: field.return_type,
                site: SourceSite {
                    path: sdl_path.to_string(),
                    line: line_offset + field.line_offset,
                },
            });
        }
    }
    out
}

// ─── Tokenizer helpers ────────────────────────────────────────────────

struct RootTypeBlock {
    op: GraphqlOp,
    line: u32,
    fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectTypeBlock {
    pub name: String,
    pub line: u32,
    pub fields: Vec<FieldDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub name: String,
    pub return_type: String,
    pub line_offset: u32,
}

fn graphql_type_to_typedesc(sdl_type: &str) -> crate::federation::contracts::model::TypeDesc {
    use crate::federation::contracts::model::TypeDesc;
    let trimmed = sdl_type.trim().trim_end_matches('!');
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let inner = trimmed[1..trimmed.len() - 1].trim().trim_end_matches('!');
        let inner_desc = graphql_scalar_to_typedesc(inner);
        return TypeDesc::Array(Box::new(inner_desc));
    }
    graphql_scalar_to_typedesc(trimmed)
}

fn graphql_scalar_to_typedesc(name: &str) -> crate::federation::contracts::model::TypeDesc {
    use crate::federation::contracts::model::TypeDesc;
    match name {
        "Int" => TypeDesc::Integer,
        "Float" => TypeDesc::Number,
        "String" | "ID" => TypeDesc::String,
        "Boolean" => TypeDesc::Boolean,
        _ => TypeDesc::Unknown,
    }
}

/// Strip `#` line comments and `""" ... """` block comments.
/// Block comments spanning multiple lines preserve newlines so
/// the subsequent line-number math stays correct. Thin wrapper
/// around `crate::server::sensors::util_tokenize::strip_comments`
/// so the per-sensor API stays unchanged for callers.
fn strip_comments(input: &str) -> String {
    crate::server::sensors::util_tokenize::strip_comments(
        input,
        crate::server::sensors::util_tokenize::CommentSyntax::HashBlockString,
    )
}

/// Join continued lines: a trailing comma or opening bracket
/// means the next line continues the current declaration. This
/// keeps multi-line field shapes
/// (`fieldName(\n  arg: Type\n): RetType`) parseable as a single
/// field.
fn join_continued_lines(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_token_ends_continuation = false;
    for ch in input.chars() {
        if prev_token_ends_continuation && ch == '\n' {
            prev_token_ends_continuation = false;
            // Drop the newline; the continuation is consumed.
            continue;
        }
        prev_token_ends_continuation = matches!(ch, ',' | '(' | '[' | '{');
        out.push(ch);
    }
    out
}

/// Extract every `type Query { ... }` /
/// `type Mutation { ... }` / `type Subscription { ... }` block
/// and the field declarations inside. The block parser is
/// brace-balanced: nested `type Foo { ... }` declarations inside
/// a root block (rare but legal) are skipped without altering
/// brace depth.
fn extract_root_type_blocks(content: &str) -> Vec<RootTypeBlock> {
    let mut out: Vec<RootTypeBlock> = Vec::new();
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
        if starts_with_keyword(bytes, i, "type") {
            // Consume `type`.
            i += "type".len();
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_no += 1;
                }
                i += 1;
            }
            // Read the type name.
            let name_start = i;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let name = std::str::from_utf8(&bytes[name_start..i])
                .unwrap_or("")
                .trim()
                .to_string();
            // Skip whitespace.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_no += 1;
                }
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'{' {
                continue;
            }
            // Only the three root types emit providers.
            let op = match name.as_str() {
                "Query" => Some(GraphqlOp::Query),
                "Mutation" => Some(GraphqlOp::Mutation),
                "Subscription" => Some(GraphqlOp::Subscription),
                _ => None,
            };
            // Walk the block — even when `op is None`, we still
            // need to consume the body so brace depth stays
            // correct for any nested declarations.
            let block_start_line = line_no;
            // The `{` we are about to consume is at `i`; the
            // body starts AFTER the `{` (i.e. at `i + 1` once we
            // advance). Track the body's byte range so the field
            // parser sees only the block's contents — not the
            // tail of the file.
            let body_start = i + 1;
            i += 1;
            let mut depth: u32 = 1;
            while i < bytes.len() && depth > 0 {
                let c = bytes[i];
                if c == b'\n' {
                    line_no += 1;
                }
                if c == b'{' {
                    depth += 1;
                } else if c == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            // `i` is now the position of the matching `}`. The
            // body is `content[body_start..i]`.
            if let Some(op) = op {
                let fields = parse_field_decls(&content[body_start..i]);
                out.push(RootTypeBlock {
                    op,
                    line: block_start_line,
                    fields,
                });
            }
            if i < bytes.len() && bytes[i] == b'}' {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Extract object type blocks (`type <Name> { ... }`) from an SDL file,
/// where `<Name>` is not a root operation type (`Query`, `Mutation`, `Subscription`).
pub fn extract_object_type_blocks(content: &str) -> Vec<ObjectTypeBlock> {
    let stripped = strip_comments(content);
    let joined = join_continued_lines(&stripped);
    let mut out: Vec<ObjectTypeBlock> = Vec::new();
    let bytes = joined.as_bytes();
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
        if starts_with_keyword(bytes, i, "type") {
            i += "type".len();
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_no += 1;
                }
                i += 1;
            }
            let name_start = i;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let name = std::str::from_utf8(&bytes[name_start..i])
                .unwrap_or("")
                .trim()
                .to_string();
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_no += 1;
                }
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'{' {
                continue;
            }
            let is_root = matches!(name.as_str(), "Query" | "Mutation" | "Subscription");
            let block_start_line = line_no;
            let body_start = i + 1;
            i += 1;
            let mut depth: u32 = 1;
            while i < bytes.len() && depth > 0 {
                let c = bytes[i];
                if c == b'\n' {
                    line_no += 1;
                }
                if c == b'{' {
                    depth += 1;
                } else if c == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            if !is_root && !name.is_empty() {
                let fields = parse_field_decls(&joined[body_start..i]);
                out.push(ObjectTypeBlock {
                    name,
                    line: block_start_line,
                    fields,
                });
            }
            if i < bytes.len() && bytes[i] == b'}' {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    out
}

/// True when `bytes[at..]` starts with `kw` and the surrounding
/// characters are word boundaries (the byte before is not
/// alphanumeric / `_`, and the byte after is not alphanumeric /
/// `_`). The graphql SDL grammar is open enough that
/// `starts_with_keyword(bytes, 0, "type")` must not match
/// `types` or `atype` — without the boundary check those would
/// land in the field-parser's identifier path and yield junk.
/// Thin wrapper around
/// `crate::server::sensors::util_tokenize::starts_with_keyword` so
/// the per-sensor API stays unchanged.
fn starts_with_keyword(bytes: &[u8], at: usize, kw: &str) -> bool {
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };
    crate::server::sensors::util_tokenize::starts_with_keyword(s, at, kw)
}

fn is_ident_continue(b: u8) -> bool {
    (b as char).is_ascii_alphanumeric() || b == b'_'
}

/// Walk every `fieldName(args): ReturnType` declaration inside
/// a root type block. The body of the block is the slice
/// starting at the block's `{` line. Multi-line declarations
/// (`fieldName(\n  arg: Type\n): RetType`) were joined by
/// `join_continued_lines` so each field is one logical line
/// here.
fn parse_field_decls(from: &str) -> Vec<FieldDecl> {
    let mut out: Vec<FieldDecl> = Vec::new();
    let bytes = from.as_bytes();
    let mut i = 0usize;
    let mut line_offset: u32 = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            line_offset += 1;
            i += 1;
            continue;
        }
        if (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Skip over `}` so we don't mistake the block terminator
        // for a field.
        if bytes[i] == b'}' {
            i += 1;
            continue;
        }
        // A field declaration starts with an identifier followed
        // by `(`, `:` (no-arg form), or whitespace+args. We
        // accept both `field: RetType` and
        // `field(args): RetType`.
        let name_start = i;
        while i < bytes.len() && is_ident_continue(bytes[i]) {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let name = std::str::from_utf8(&bytes[name_start..i])
            .unwrap_or("")
            .trim()
            .to_string();
        // Skip whitespace.
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            if bytes[i] == b'\n' {
                line_offset += 1;
            }
            i += 1;
        }
        // Optional `(args)` — track balanced parens.
        if i < bytes.len() && bytes[i] == b'(' {
            let mut depth: u32 = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                let c = bytes[i];
                if c == b'\n' {
                    line_offset += 1;
                }
                if c == b'(' {
                    depth += 1;
                } else if c == b')' {
                    depth -= 1;
                }
                i += 1;
            }
            // Skip whitespace.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_offset += 1;
                }
                i += 1;
            }
        }
        // Expect `:`.
        if i >= bytes.len() || bytes[i] != b':' {
            continue;
        }
        i += 1;
        // Skip whitespace.
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            if bytes[i] == b'\n' {
                line_offset += 1;
            }
            i += 1;
        }
        // Read the return type up to the next `,` or `}`.
        let ret_start = i;
        while i < bytes.len() && bytes[i] != b',' && bytes[i] != b'}' && bytes[i] != b'\n' {
            // Skip over `[Type!]` brackets so a comma inside
            // doesn't terminate the type.
            if bytes[i] == b'[' {
                let mut bracket_depth: u32 = 1;
                i += 1;
                while i < bytes.len() && bracket_depth > 0 {
                    if bytes[i] == b'[' {
                        bracket_depth += 1;
                    } else if bytes[i] == b']' {
                        bracket_depth -= 1;
                    }
                    i += 1;
                }
                continue;
            }
            i += 1;
        }
        let return_type = std::str::from_utf8(&bytes[ret_start..i])
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() || return_type.is_empty() {
            continue;
        }
        out.push(FieldDecl {
            name,
            return_type,
            line_offset,
        });
        // Skip the `,` if present.
        if i < bytes.len() && bytes[i] == b',' {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_query_root_fields() {
        let src = "\
type Query {
  orders: [Order!]!
  order(id: ID!): Order
  health: String!
}
";
        let providers = parse_sdl_providers(src, "schema.graphql");
        assert_eq!(providers.len(), 3);
        assert_eq!(providers[0].op, GraphqlOp::Query);
        assert_eq!(providers[0].field, "orders");
        assert_eq!(providers[0].return_type, "[Order!]!");
        assert_eq!(providers[1].field, "order");
        assert_eq!(providers[1].return_type, "Order");
        assert_eq!(providers[2].field, "health");
        assert_eq!(providers[2].return_type, "String!");
    }

    #[test]
    fn parses_mutation_and_subscription() {
        let src = "\
type Query {
  ping: String
}

type Mutation {
  createOrder(input: CreateOrderInput!): Order!
}

type Subscription {
  onOrderUpdate: Order!
}
";
        let providers = parse_sdl_providers(src, "schema.graphql");
        assert_eq!(providers.len(), 3);
        let ops: Vec<GraphqlOp> = providers.iter().map(|p| p.op).collect();
        assert_eq!(
            ops,
            vec![
                GraphqlOp::Query,
                GraphqlOp::Mutation,
                GraphqlOp::Subscription
            ]
        );
        assert_eq!(providers[1].field, "createOrder");
        assert_eq!(providers[2].field, "onOrderUpdate");
    }

    #[test]
    fn ignores_non_root_type_blocks() {
        let src = "\
type Query {
  orders: [Order!]!
}

type Order {
  id: ID!
  total: Int!
}

type Mutation {
  createOrder: Order!
}
";
        let providers = parse_sdl_providers(src, "schema.graphql");
        assert_eq!(providers.len(), 2);
        assert!(providers
            .iter()
            .all(|p| p.field != "id" && p.field != "total"));
    }

    #[test]
    fn strips_line_comments() {
        let src = "\
type Query {
  # this one is the orders query
  orders: [Order!]!
  # skip: ignore
  ignore: String
}
";
        let providers = parse_sdl_providers(src, "schema.graphql");
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].field, "orders");
        assert_eq!(providers[1].field, "ignore");
    }

    #[test]
    fn handles_multiline_field_declaration() {
        let src = "\
type Query {
  orders(
    status: String
    limit: Int
  ): [Order!]!
}
";
        let providers = parse_sdl_providers(src, "schema.graphql");
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].field, "orders");
        assert_eq!(providers[0].return_type, "[Order!]!");
    }

    #[test]
    fn returns_empty_for_schema_only_file() {
        let src = "\
schema {
  query: Query
  mutation: Mutation
}

type Query {
  orders: [Order!]!
}
";
        let providers = parse_sdl_providers(src, "schema.graphql");
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].op, GraphqlOp::Query);
    }
}
