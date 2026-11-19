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
//! emitted as a field. Balanced argument lists preserve multiline
//! declarations and source locations; type references stop before
//! directives and adjacent field declarations.
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
use crate::server::sensors::util_tokenize::starts_with_keyword;
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
        let root_blocks = extract_root_type_blocks(&strip_comments(&content));

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
            schema_node.contract = vec![ContractFact::Schema {
                direction: Direction::Response,
            }];
            all_nodes.push(schema_node);
            schemas_in_file.insert(obj.name.clone(), schema_id.clone());

            for field in obj.fields {
                let field_id = GraphNode::generate_id(
                    &NodeType::Field,
                    &graph_path_str,
                    &format!("{}.{}", obj.name, field.name),
                    Some(obj.line + field.line_offset),
                    namespace,
                );
                let mut field_node =
                    GraphNode::new(NodeType::Field, field.name.clone(), graph_path_str.clone());
                field_node.id = field_id.clone();
                field_node.line_start = Some(obj.line + field.line_offset);
                field_node.line_end = Some(obj.line + field.line_offset);
                field_node.signature = Some(field.return_type.clone());
                let required = field.return_type.ends_with('!');
                field_node.contract = vec![ContractFact::Field(
                    crate::federation::contracts::model::FieldMeta {
                        ty: graphql_type_to_typedesc(&field.return_type),
                        required,
                        nullable: !required,
                        enum_values: None,
                    },
                )];
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
            node.contract = vec![ContractFact::GraphqlProvider(GraphqlProviderFact {
                op: provider.op,
                field: provider.field.clone(),
                return_type: provider.return_type.clone(),
            })];
            all_nodes.push(node);
            total += 1;

            let args = root_blocks
                .iter()
                .filter(|b| b.op == provider.op)
                .flat_map(|b| &b.fields)
                .find(|f| f.name == provider.field)
                .map(|f| f.arguments.clone())
                .unwrap_or_default();
            emit_schema(
                graph_path_str.as_str(),
                namespace,
                &id,
                &format!("{id_name}:request"),
                provider.site.line,
                Direction::Request,
                args,
                &mut all_nodes,
                &mut all_edges,
            );
            let bare_ret = provider
                .return_type
                .trim_matches(|c| matches!(c, '[' | ']' | '!' | ' '));
            if let Some(resp_schema_id) = schemas_in_file.get(bare_ret) {
                all_edges.push(GraphEdge::new(
                    EdgeType::ResponseSchema,
                    id,
                    resp_schema_id.clone(),
                ));
            } else {
                // Scalars, enums, unions, and externally defined object types
                // still describe a result even without local object fields.
                emit_schema(
                    graph_path_str.as_str(),
                    namespace,
                    &id,
                    &format!("{id_name}:response"),
                    provider.site.line,
                    Direction::Response,
                    vec![FieldDecl {
                        name: provider.field,
                        return_type: provider.return_type,
                        line_offset: 0,
                        arguments: vec![],
                    }],
                    &mut all_nodes,
                    &mut all_edges,
                );
            }
        }
    }
    graph.replace_sensor_output(SensorOwner::GraphqlProviderSensor, &all_nodes, &all_edges)?;
    Ok(total)
}

/// Materialize an operation's argument or scalar-result schema with SDL types.
#[allow(clippy::too_many_arguments)]
fn emit_schema(
    path: &str,
    namespace: &RepoNamespace,
    provider_id: &str,
    name: &str,
    line: u32,
    direction: Direction,
    fields: Vec<FieldDecl>,
    nodes: &mut Vec<GraphNode>,
    edges: &mut Vec<GraphEdge>,
) {
    let id = GraphNode::generate_id(&NodeType::Schema, path, name, Some(line), namespace);
    let mut node = GraphNode::new(NodeType::Schema, name.to_string(), path.to_string());
    node.id = id.clone();
    node.line_start = Some(line);
    node.line_end = Some(line);
    node.contract = vec![ContractFact::Schema { direction }];
    nodes.push(node);
    edges.push(GraphEdge::new(
        match direction {
            Direction::Request => EdgeType::RequestSchema,
            _ => EdgeType::ResponseSchema,
        },
        provider_id.to_string(),
        id.clone(),
    ));
    for field in fields {
        let field_id = GraphNode::generate_id(
            &NodeType::Field,
            path,
            &format!("{name}.{}", field.name),
            Some(line),
            namespace,
        );
        let mut node = GraphNode::new(NodeType::Field, field.name, path.to_string());
        node.id = field_id.clone();
        node.line_start = Some(line);
        node.line_end = Some(line);
        node.signature = Some(field.return_type.clone());
        let required = field.return_type.ends_with('!');
        node.contract = vec![ContractFact::Field(
            crate::federation::contracts::model::FieldMeta {
                ty: graphql_type_to_typedesc(&field.return_type),
                required,
                nullable: !required,
                enum_values: None,
            },
        )];
        nodes.push(node);
        edges.push(GraphEdge::new(EdgeType::HasField, id.clone(), field_id));
    }
}

// ─── Detection (hand-rolled SDL tokenizer) ─────────────────────────────

/// Parse an SDL file and return one `GraphqlProvider` per
/// `(op, field)` root-field declaration. `sdl_path` is the path
/// the sensor records as the source site (typically a
/// repo-relative path).
pub fn parse_sdl_providers(content: &str, sdl_path: &str) -> Vec<GraphqlProvider> {
    let stripped = strip_comments(content);
    let joined = stripped;
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
    pub arguments: Vec<FieldDecl>,
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
/// the subsequent line-number math stays correct. String contents are
/// masked as well, so braces inside defaults cannot change block depth.
fn strip_comments(input: &str) -> String {
    // Mask descriptions and string values before balancing SDL syntax. Keep
    // byte positions and newlines, including non-ASCII description text.
    let bytes = input.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i = crate::server::sensors::util_tokenize::extract_string_literal(input, i)
                .map(|(end, _)| end)
                .unwrap_or(bytes.len());
        } else {
            i += 1;
            continue;
        }
        for byte in &mut out[start..i] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        // A harmless scalar token lets default-value parsing consume strings.
        if bytes[start] == b'"' {
            out[start] = b'0';
        }
    }
    String::from_utf8(out).expect("masked SDL remains UTF-8")
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
        if starts_with_keyword(content, i, "type") {
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
            // Federation SDL allows one or more directives
            // (`@key(...)`, `@shareable`, …) and an optional
            // `implements A & B` clause between the type name
            // and the `{`. Consume them before requiring the
            // opening brace.
            let Some(brace_pos) = skip_type_modifiers(content, i, &mut line_no) else {
                continue;
            };
            i = brace_pos;
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
    let joined = stripped;
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
        if starts_with_keyword(&joined, i, "type") {
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
            // Federation SDL allows `@directive(...)` and
            // `implements A & B` between the name and the `{`.
            let Some(brace_pos) = skip_type_modifiers(&joined, i, &mut line_no) else {
                continue;
            };
            i = brace_pos;
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

fn is_ident_continue(b: u8) -> bool {
    (b as char).is_ascii_alphanumeric() || b == b'_'
}

/// Consume everything between a type name and its `{` body:
/// zero or more `@directive(args)` invocations and an optional
/// `implements A & B` clause. Federation SDL is nearly always
/// `type User @key(fields: "id") implements Node { … }`, so
/// the type-name reader must allow it. Returns the position of
/// the `{` on success, or `None` when the next non-space byte
/// is neither a directive, an `implements` clause, nor `{`.
fn skip_type_modifiers(content: &str, at: usize, line_no: &mut u32) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut i = at;
    let mut consumed = false;
    loop {
        // Skip whitespace, tracking newlines so the opening
        // brace's line number is reported correctly.
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            if bytes[i] == b'\n' {
                *line_no += 1;
            }
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'{' {
            return Some(i);
        }
        if bytes[i] == b'@' {
            // Directive: `@name(args)?`.
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    *line_no += 1;
                }
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'(' {
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
            }
            consumed = true;
            continue;
        }
        if starts_with_keyword(content, i, "implements") {
            i += "implements".len();
            loop {
                while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                    if bytes[i] == b'\n' {
                        *line_no += 1;
                    }
                    i += 1;
                }
                if i >= bytes.len() || !is_ident_continue(bytes[i]) {
                    break;
                }
                while i < bytes.len() && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                    if bytes[i] == b'\n' {
                        *line_no += 1;
                    }
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'&' {
                    i += 1;
                    continue;
                }
                break;
            }
            consumed = true;
            continue;
        }
        if !consumed {
            // Nothing recognised and we have not yet eaten
            // any modifier — the type name has no `{` follow
            // at all, e.g. `type Foo` as a forward
            // declaration. Bail so the caller can `continue`.
            return None;
        }
        // We consumed something earlier but now hit a byte we
        // do not recognise (e.g. an unexpected keyword).
        return None;
    }
}

/// Walk every `fieldName(args): ReturnType` declaration inside
/// a root type block. The body of the block is the slice
/// starting at the block's `{` line. Arguments are parsed recursively
/// after balancing their parentheses; whitespace and commas separate fields.
fn skip_space(bytes: &[u8], i: &mut usize) {
    while *i < bytes.len() && (bytes[*i].is_ascii_whitespace() || bytes[*i] == b',') {
        *i += 1;
    }
}

fn skip_group(bytes: &[u8], i: &mut usize, open: u8, close: u8) {
    let mut depth = 0;
    while *i < bytes.len() {
        let ch = bytes[*i];
        *i += 1;
        if ch == open {
            depth += 1;
        }
        if ch == close {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
    }
}

fn parse_field_decls(from: &str) -> Vec<FieldDecl> {
    let bytes = from.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut counted = 0;
    let mut line_offset = 0;
    while i < bytes.len() {
        skip_space(bytes, &mut i);
        line_offset += bytes[counted..i].iter().filter(|&&b| b == b'\n').count() as u32;
        counted = i;
        let field_line = line_offset;
        if i == bytes.len() {
            break;
        }
        // Descriptions have been masked with a numeric sentinel.
        if !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_ident_continue(bytes[i]) {
            i += 1;
        }
        let name = from[start..i].to_string();
        skip_space(bytes, &mut i);
        let mut arguments = Vec::new();
        if i < bytes.len() && bytes[i] == b'(' {
            let args_start = i + 1;
            skip_group(bytes, &mut i, b'(', b')');
            arguments = parse_field_decls(&from[args_start..i.saturating_sub(1)]);
        }
        skip_space(bytes, &mut i);
        if i == bytes.len() || bytes[i] != b':' {
            continue;
        }
        i += 1;
        skip_space(bytes, &mut i);
        let start = i;
        // A type reference ends before the next declaration or directive.
        if i < bytes.len() && bytes[i] == b'[' {
            skip_group(bytes, &mut i, b'[', b']');
        } else {
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
        }
        if i < bytes.len() && bytes[i] == b'!' {
            i += 1;
        }
        let return_type = from[start..i].split_whitespace().collect::<String>();
        if return_type.is_empty() {
            continue;
        }
        out.push(FieldDecl {
            name,
            return_type,
            line_offset: field_line,
            arguments,
        });
        skip_space(bytes, &mut i);
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            skip_space(bytes, &mut i);
            match bytes.get(i) {
                Some(b'{') => skip_group(bytes, &mut i, b'{', b'}'),
                Some(b'[') => skip_group(bytes, &mut i, b'[', b']'),
                _ => {
                    while i < bytes.len()
                        && (is_ident_continue(bytes[i]) || matches!(bytes[i], b'.' | b'-' | b'+'))
                    {
                        i += 1;
                    }
                }
            }
        }
        loop {
            skip_space(bytes, &mut i);
            if i == bytes.len() || bytes[i] != b'@' {
                break;
            }
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            skip_space(bytes, &mut i);
            if i < bytes.len() && bytes[i] == b'(' {
                skip_group(bytes, &mut i, b'(', b')');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdl_keeps_actual_field_lines_and_ignores_description_syntax() {
        let src = "type Mutation {\n  \"\"\"mentions type Query { fake: Missing }\"\"\"\n  checkoutComplete(\n    checkoutId: ID!\n  ): CheckoutComplete @deprecated(reason: \"use new\")\n}\n";
        let providers = parse_sdl_providers(src, "schema.graphql");
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].field, "checkoutComplete");
        assert_eq!(providers[0].return_type, "CheckoutComplete");
        assert_eq!(providers[0].site.line, 3);
    }

    #[test]
    fn saleor_sdl_emits_request_and_response_for_every_operation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("schema.graphql"),
            r#"
scalar JSONString
type Query { checkout(id: ID!): Checkout health: String! }
type Mutation {
  checkoutComplete(
    checkoutId: ID!
    paymentData: JSONString = "{}"
    redirectUrl: String
  ): CheckoutComplete
}
type Subscription { checkoutUpdated: Checkout }
type Checkout { id: ID! metadata: JSONString }
type CheckoutComplete { checkout: Checkout errors: [CheckoutError!]! }
type CheckoutError { field: String message: String! }
"#,
        )
        .unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        assert_eq!(
            scan_workspace_graphql_provider(&graph, dir.path(), &RepoNamespace::for_test())
                .unwrap(),
            4
        );
        let providers: Vec<_> = graph
            .get_all_nodes()
            .into_iter()
            .filter(|n| {
                n.contract
                    .iter()
                    .any(|f| matches!(f, ContractFact::GraphqlProvider(_)))
            })
            .collect();
        for provider in &providers {
            let edges = graph.get_edges_from(&provider.id).unwrap();
            assert!(
                edges
                    .iter()
                    .any(|e| e.edge_type == EdgeType::ResponseSchema),
                "{} response",
                provider.name
            );
        }
        let complete = providers
            .iter()
            .find(|p| p.name == "mutation:checkoutComplete")
            .unwrap();
        let edges = graph.get_edges_from(&complete.id).unwrap();
        let request = edges
            .iter()
            .find(|e| e.edge_type == EdgeType::RequestSchema)
            .expect("request arguments");
        let fields = graph.get_edges_from(&request.target_id).unwrap();
        let mut args = std::collections::BTreeMap::new();
        for field in fields {
            let node = graph.get_node(&field.target_id).unwrap().unwrap();
            args.insert(node.name, node.signature.unwrap());
        }
        assert_eq!(args["checkoutId"], "ID!");
        assert_eq!(args["paymentData"], "JSONString");
        assert_eq!(args["redirectUrl"], "String");
    }

    #[test]
    fn parses_large_sdl_without_revalidating_the_document_per_token() {
        // Large schemas have many non-root declarations. A whole-document
        // UTF-8 check at every keyword probe made this input quadratic.
        let mut source =
            String::from("\"\"\"Catalog description: café ☕\"\"\"\n# schema documentation\n");
        for i in 0..65_536 {
            source.push_str(&format!("scalar Scalar{i}\n"));
        }
        source.push_str("type Query { product: Product }\ntype Product { id: ID! }\n");
        let started = std::time::Instant::now();
        let providers = parse_sdl_providers(&source, "large.graphql");
        let objects = extract_object_type_blocks(&source);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].field, "product");
        assert_eq!(providers[0].site.path, "large.graphql");
        assert!(providers[0].site.line > 65_536);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].name, "Product");
        assert_eq!(objects[0].fields[0].name, "id");
        assert_eq!(objects[0].fields[0].return_type, "ID!");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "a 1 MiB SDL must parse within five seconds, including in debug builds"
        );
    }

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
