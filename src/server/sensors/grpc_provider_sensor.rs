//! Phase E — gRPC provider sensor (spec §8.2).
//!
//! Recognises the per-`.proto`-file provider surface for gRPC. The
//! sensor:
//!
//! 1. Walks every `.proto` file under `root`.
//! 2. Tokenizes the file with a focused hand-rolled parser (per
//!    spec §8.2's escape hatch — no new tree-sitter grammar crate).
//! 3. Extracts the `package` declaration (or the empty string when
//!    absent) and every `service Foo { rpc Bar(Request) returns
//!    (Response); }` declaration.
//! 4. Mints one `Module` node per `(package, service, method)` triple
//!    carrying `ContractFact::RpcProvider { system: Grpc, service:
//!    "<package>.<service>", method: "<method>", ... }`.
//!
//! Server-registration linkage (`RegisterFooServer(...)`,
//! `@GrpcService(impl = FooImpl.class)`, etc.) is in
//! [`crate::server::sensors::grpc_handler_link_sensor`]. The
//! provider sensor only emits the proto-side declaration; the
//! handler-link sensor fills the `handler` field on the
//! `RpcProvider`.
//!
//! The parser is intentionally narrow: it recognises the canonical
//! proto shapes the test fixtures emit (`syntax = "protoN";`,
//! `package com.example.orders;`, `service Foo { rpc Bar(Request)
//! returns (Response); option { ... } }`). Multi-line declarations
//! are joined before parsing. Comments (`//` line, `/* */` block)
//! are stripped so a `// rpc Skip` inside a `service` block does
//! not get emitted as a method.

use crate::error::LainError;
use crate::federation::contracts::coverage::UnresolvedRecord;
use crate::federation::contracts::model::{
    ContractFact, Direction, RpcProviderFact, RpcSystem, SourceSite, UnresolvedReason,
};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use crate::server::sensors::payload_schema::{
    parse_proto_messages_with_diagnostics, ProtoParseDiagnostic,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// ─── Public sensor shape ───────────────────────────────────────────────

/// One gRPC provider surface declaration extracted from a `.proto`
/// file. Public so the acceptance tests can exercise the detector
/// without going through the graph emission path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcProvider {
    pub package: String,
    pub service: String,
    pub method: String,
    pub request_type: String,
    pub response_type: String,
    pub site: SourceSite,
}

/// Unit-struct Sensor impl. Registered via `inventory::submit!`
/// below; no central registry to edit.
pub struct GrpcProviderSensor;

impl crate::server::sensors::Sensor for GrpcProviderSensor {
    fn name(&self) -> &'static str {
        "grpc_provider"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::Proto
    }
    fn phase(&self) -> u8 {
        0
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        // The richer entry point is `scan_workspace_grpc_with_report`;
        // the legacy `scan` ignores the diagnostics and returns the
        // count so existing callers / coverage paths that key on the
        // integer keep working.
        Ok(scan_workspace_grpc_with_report(graph, root, namespace)?.emitted)
    }
    fn scan_with_report(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<crate::server::sensors::ScanReport, LainError> {
        scan_workspace_grpc_with_report(graph, root, namespace)
    }
}
inventory::submit!(crate::server::sensors::SensorEntry(&GrpcProviderSensor));

// ─── Workspace scan ───────────────────────────────────────────────────

/// Walk `root`, find every `.proto` file, parse the service
/// surface, and emit one `RpcProvider` `Module` node per
/// `(package, service, method)` triple, along with message `Schema`
/// and `Field` nodes linked via `RequestSchema`, `ResponseSchema`,
/// and `HasField` edges. Returns the count of `RpcProvider` nodes minted.
pub fn scan_workspace_grpc(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    // Delegate to the richer entry point so the legacy count
    // matches the new report's `emitted` field exactly. The
    // diagnostic side-band is ignored by `scan()`; the coverage
    // ledger reads it via `scan_with_report`.
    Ok(scan_workspace_grpc_with_report(graph, root, namespace)?.emitted)
}

/// Walk `root`, parse every `.proto` file, and return a
/// [`ScanReport`] carrying the per-file diagnostics the message
/// parser surfaced. The diagnostics are recorded against
/// [`UnresolvedReason::BaseUnknown`] (the closest semantic match —
/// "the extractor did not recognise the form") so the coverage
/// ledger can mark the repo as incomplete instead of silently
/// serving `NoKnownImpact` for an unparseable `.proto` file.
///
/// One `UnresolvedRecord` is emitted per reason-bucket with the
/// file path as a sample id (up to 5 samples per bucket, the
/// same cap the sql sensor and the coverage ledger use).
pub fn scan_workspace_grpc_with_report(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<crate::server::sensors::ScanReport, LainError> {
    if graph.is_read_only() {
        return Ok(crate::server::sensors::ScanReport::default());
    }
    let proto_ext = |p: &Path| {
        if p.extension().and_then(|e| e.to_str()) == Some("proto") {
            Some(())
        } else {
            None
        }
    };
    let mut total = 0usize;
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();
    // Per-reason bucket of (count, sample_paths). The coverage
    // ledger collapses these to one `UnresolvedRecord` per
    // reason; we cap `sample_ids` at 5 to match `sql_sensor`'s
    // behaviour and the §4.1 `sample_paths(≤5)` contract.
    let mut unresolved_by_reason: BTreeMap<UnresolvedReason, (usize, Vec<String>)> =
        BTreeMap::new();
    for (path, content, _tag) in crate::server::sensors::util::scan_files(root, proto_ext) {
        let graph_path_str = graph_path(root, &path);
        let providers = parse_proto_providers(&content, &graph_path_str);

        // Collect the bare names of every type used as a request
        // argument before emitting them as Schema nodes, so the
        // Schema node's `Direction` reflects how the message is
        // actually consumed (`Request` vs `Response`). A type that
        // appears on both sides of any RPC falls back to `Request`
        // — the consumer-facing view is the one joiner dispatch
        // uses to derive the schema lineage.
        let mut request_types: BTreeSet<String> = BTreeSet::new();
        for provider in &providers {
            let bare = provider
                .request_type
                .rsplit('.')
                .next()
                .unwrap_or(&provider.request_type);
            request_types.insert(bare.to_string());
            request_types.insert(provider.request_type.clone());
        }

        // Parse protobuf message schemas and fields, *and* the
        // diagnostics the parser surfaced for shapes it could
        // not classify. The rich entry point replaces the
        // line-based `parse_proto_messages` so the sensor sees
        // every malformed input instead of silently swallowing
        // it.
        let mut schemas_in_file: BTreeMap<String, String> = BTreeMap::new();
        let (parsed_messages, diagnostics) = parse_proto_messages_with_diagnostics(&content);
        for msg in parsed_messages {
            let line = msg.fields.first().map_or(1, |f| f.line);
            let schema_id = GraphNode::generate_id(
                &NodeType::Schema,
                &graph_path_str,
                &msg.name,
                Some(line),
                namespace,
            );
            let direction = if request_types.contains(&msg.name) {
                Direction::Request
            } else {
                Direction::Response
            };
            let mut schema_node =
                GraphNode::new(NodeType::Schema, msg.name.clone(), graph_path_str.clone());
            schema_node.id = schema_id.clone();
            schema_node.line_start = Some(line);
            schema_node.line_end = Some(line);
            schema_node.contract = Some(ContractFact::Schema { direction });
            all_nodes.push(schema_node);
            schemas_in_file.insert(msg.name.clone(), schema_id.clone());

            for field in msg.fields {
                let field_name = field.path.to_string();
                let field_id = GraphNode::generate_id(
                    &NodeType::Field,
                    &graph_path_str,
                    &field_name,
                    Some(field.line),
                    namespace,
                );
                let mut field_node =
                    GraphNode::new(NodeType::Field, field_name, graph_path_str.clone());
                field_node.id = field_id.clone();
                field_node.line_start = Some(field.line);
                field_node.line_end = Some(field.line);
                field_node.contract = Some(ContractFact::Field(field.meta));
                all_nodes.push(field_node);
                all_edges.push(GraphEdge::new(
                    EdgeType::HasField,
                    schema_id.clone(),
                    field_id,
                ));
            }
        }

        for provider in providers {
            let full_service = compose_service_name(&provider.package, &provider.service);
            let id_name = format!("{}/{}", full_service, provider.method);
            let id = GraphNode::generate_id(
                &NodeType::Module,
                &provider.site.path,
                &id_name,
                Some(provider.site.line),
                namespace,
            );
            let mut node = GraphNode::new(
                NodeType::Module,
                format!("{}.{}", provider.service, provider.method),
                provider.site.path.clone(),
            );
            node.id = id.clone();
            node.line_start = Some(provider.site.line);
            node.line_end = Some(provider.site.line);
            node.contract = Some(ContractFact::RpcProvider(RpcProviderFact {
                system: RpcSystem::Grpc,
                service: full_service,
                method: provider.method.clone(),
                request_type: provider.request_type.clone(),
                response_type: provider.response_type.clone(),
                handler: None,
            }));
            all_nodes.push(node);
            total += 1;

            let req_type_bare = provider
                .request_type
                .rsplit('.')
                .next()
                .unwrap_or(&provider.request_type);
            if let Some(req_schema_id) = schemas_in_file
                .get(req_type_bare)
                .or_else(|| schemas_in_file.get(&provider.request_type))
            {
                all_edges.push(GraphEdge::new(
                    EdgeType::RequestSchema,
                    id.clone(),
                    req_schema_id.clone(),
                ));
            }

            let resp_type_bare = provider
                .response_type
                .rsplit('.')
                .next()
                .unwrap_or(&provider.response_type);
            if let Some(resp_schema_id) = schemas_in_file
                .get(resp_type_bare)
                .or_else(|| schemas_in_file.get(&provider.response_type))
            {
                all_edges.push(GraphEdge::new(
                    EdgeType::ResponseSchema,
                    id.clone(),
                    resp_schema_id.clone(),
                ));
            }
        }

        // Surface every parser diagnostic. The message parser
        // records (line, kind, message) for each shape it could
        // not classify; we roll those up by reason so the
        // coverage ledger can flag the repo. `BaseUnknown` is
        // the closest semantic match for "the extractor did not
        // recognise the form" — adding a per-protocol reason
        // variant would force a federation-graph schema bump
        // (see AGENTS.md) so we reuse what is there.
        record_diagnostics(&diagnostics, &graph_path_str, &mut unresolved_by_reason);
    }
    graph.replace_sensor_output(SensorOwner::GrpcProviderSensor, &all_nodes, &all_edges)?;
    let unresolved_records: Vec<UnresolvedRecord> = unresolved_by_reason
        .into_iter()
        .map(|(reason, (count, sample_ids))| UnresolvedRecord {
            reason,
            count,
            sample_ids,
        })
        .collect();
    Ok(crate::server::sensors::ScanReport {
        emitted: total,
        error: None,
        unresolved: unresolved_records,
    })
}

fn record_diagnostics(
    diagnostics: &[ProtoParseDiagnostic],
    graph_path: &str,
    bucket: &mut BTreeMap<UnresolvedReason, (usize, Vec<String>)>,
) {
    if diagnostics.is_empty() {
        return;
    }
    let entry = bucket
        .entry(UnresolvedReason::BaseUnknown)
        .or_insert((0, Vec::new()));
    // Count each diagnostic separately so a single file with N
    // unparseable shapes contributes N to the bucket, not 1.
    entry.0 += diagnostics.len();
    if entry.1.len() < 5 {
        entry.1.push(graph_path.to_string());
    }
}

// ─── Detection (hand-rolled tokenizer) ────────────────────────────────

/// Parse a `.proto` file and return one `GrpcProvider` per
/// `(service, method)` declaration. `proto_path` is the path the
/// sensor should record as the source site (typically a
/// repo-relative path).
pub fn parse_proto_providers(content: &str, proto_path: &str) -> Vec<GrpcProvider> {
    let stripped = strip_comments(content);
    let joined = join_continued_lines(&stripped);
    let package = extract_package(&joined);
    let mut out: Vec<GrpcProvider> = Vec::new();
    for service_block in extract_service_blocks(&joined) {
        let service_name = service_block.name;
        let line_offset = service_block.line;
        for method in service_block.methods {
            out.push(GrpcProvider {
                package: package.clone(),
                service: service_name.clone(),
                method: method.name,
                request_type: method.request_type,
                response_type: method.response_type,
                site: SourceSite {
                    path: proto_path.to_string(),
                    line: line_offset + method.line_offset,
                },
            });
        }
    }
    out
}

/// Re-export of the canonical `crate::server::sensors::util::compose_service_name`
/// so existing callers keep working. Phase A review §D2 collapsed
/// the two identical 3-line bodies (one here, one in
/// `grpc_consumer_sensor`) into the shared util implementation.
pub use crate::server::sensors::util::compose_service_name;

// ─── Tokenizer helpers ────────────────────────────────────────────────

struct ServiceBlock {
    name: String,
    line: u32,
    methods: Vec<MethodDecl>,
}

struct MethodDecl {
    name: String,
    request_type: String,
    response_type: String,
    line_offset: u32,
}

/// Strip `//` line comments and `/* … */` block comments. Block
/// comments spanning multiple lines preserve newlines so the
/// subsequent line-number math stays correct. Thin wrapper around
/// `crate::server::sensors::util_tokenize::strip_comments` so the
/// per-sensor API stays unchanged for callers.
fn strip_comments(input: &str) -> String {
    crate::server::sensors::util_tokenize::strip_comments(
        input,
        crate::server::sensors::util_tokenize::CommentSyntax::CStyle,
    )
}

/// Join continued lines: a single `\` at end of line is a proto
/// line continuation. Shared via `util_tokenize::join_continued_lines`
/// so `payload_schema::parse_proto_messages` reuses the same lexer
/// rather than re-implementing it.
fn join_continued_lines(input: &str) -> String {
    crate::server::sensors::util_tokenize::join_continued_lines(input)
}

/// Extract the value of the single `package NAME;` declaration.
/// Returns the empty string when the file has no package.
fn extract_package(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("package") else {
            continue;
        };
        let rest = rest.trim();
        let Some(rest) = rest.strip_suffix(';') else {
            continue;
        };
        return rest.trim().to_string();
    }
    String::new()
}

/// Extract every `service Foo { ... }` block along with the rpc
/// declarations inside. Handles nested braces (the canonical proto
/// shape carries `option { ... }` and message bodies inside a
/// service block, which we want to skip over).
fn extract_service_blocks(content: &str) -> Vec<ServiceBlock> {
    let mut out: Vec<ServiceBlock> = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut line_no: u32 = 1;
    while i < bytes.len() {
        // Track newlines so we can return line numbers.
        if bytes[i] == b'\n' {
            line_no += 1;
            i += 1;
            continue;
        }
        // Skip whitespace.
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Look for `service` at the start of a token.
        if starts_with_keyword(bytes, i, "service") {
            let block_start_line = line_no;
            // Consume `service`.
            i += "service".len();
            // Skip whitespace.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_no += 1;
                }
                i += 1;
            }
            // Read the service identifier.
            let name_start = i;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let service_name = std::str::from_utf8(&bytes[name_start..i])
                .unwrap_or("")
                .trim()
                .to_string();
            // Skip whitespace until the `{`.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_no += 1;
                }
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'{' {
                continue;
            }
            // Walk the block, collecting `rpc` declarations.
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
            let methods = parse_rpc_decls(&content[find_byte_offset(content, block_start_line)..]);
            out.push(ServiceBlock {
                name: service_name,
                line: block_start_line,
                methods,
            });
            // Skip the closing `}`.
            if i < bytes.len() && bytes[i] == b'}' {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    out
}

/// Map a 1-based line number back to a byte offset inside `content`.
fn find_byte_offset(content: &str, line: u32) -> usize {
    let mut current_line: u32 = 1;
    for (idx, ch) in content.char_indices() {
        if current_line == line {
            return idx;
        }
        if ch == '\n' {
            current_line += 1;
        }
    }
    content.len()
}

fn starts_with_keyword(bytes: &[u8], at: usize, kw: &str) -> bool {
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };
    crate::server::sensors::util_tokenize::starts_with_keyword(s, at, kw)
}

fn is_ident_continue(b: u8) -> bool {
    (b as char).is_ascii_alphanumeric() || b == b'_' || b == b'.'
}

/// Walk every `rpc Name(Request) returns (Response);` inside a
/// service block. `from` is a slice of the original file starting
/// at the service's first line; we only descend into the block by
/// tracking brace depth.
fn parse_rpc_decls(from: &str) -> Vec<MethodDecl> {
    let mut out: Vec<MethodDecl> = Vec::new();
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
        if starts_with_keyword(bytes, i, "rpc") {
            let method_line = line_offset;
            i += "rpc".len();
            // Skip whitespace.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_offset += 1;
                }
                i += 1;
            }
            // Method name.
            let name_start = i;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let name = std::str::from_utf8(&bytes[name_start..i])
                .unwrap_or("")
                .trim()
                .to_string();
            // Skip whitespace and `(`.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_offset += 1;
                }
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'(' {
                continue;
            }
            i += 1;
            // Read request type until matching `)`.
            let request_start = i;
            while i < bytes.len() && bytes[i] != b')' {
                if bytes[i] == b'\n' {
                    line_offset += 1;
                }
                i += 1;
            }
            let request_type = std::str::from_utf8(&bytes[request_start..i])
                .unwrap_or("")
                .trim()
                .to_string();
            if i < bytes.len() {
                i += 1;
            }
            // Skip whitespace; expect `returns`.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_offset += 1;
                }
                i += 1;
            }
            if !starts_with_keyword(bytes, i, "returns") {
                continue;
            }
            i += "returns".len();
            // Skip whitespace and `(`.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_offset += 1;
                }
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'(' {
                continue;
            }
            i += 1;
            let response_start = i;
            while i < bytes.len() && bytes[i] != b')' {
                if bytes[i] == b'\n' {
                    line_offset += 1;
                }
                i += 1;
            }
            let response_type = std::str::from_utf8(&bytes[response_start..i])
                .unwrap_or("")
                .trim()
                .to_string();
            if i < bytes.len() {
                i += 1;
            }
            // Skip whitespace; expect `;`.
            while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
                if bytes[i] == b'\n' {
                    line_offset += 1;
                }
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b';' {
                i += 1;
            }
            out.push(MethodDecl {
                name,
                request_type,
                response_type,
                line_offset: method_line,
            });
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_service_rpc() {
        let src = "\
syntax = \"proto3\";

package com.acme.orders;

service Orders {
  rpc Get (GetRequest) returns (GetResponse);
  rpc Create (CreateRequest) returns (CreateResponse);
}
";
        let providers = parse_proto_providers(src, "orders.proto");
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].package, "com.acme.orders");
        assert_eq!(providers[0].service, "Orders");
        assert_eq!(providers[0].method, "Get");
        assert_eq!(providers[0].request_type, "GetRequest");
        assert_eq!(providers[0].response_type, "GetResponse");
        assert_eq!(providers[1].method, "Create");
    }

    #[test]
    fn strips_block_comments_before_parsing() {
        let src = "\
package acme;

/* this service is the public one */
service Public {
  rpc Hi (HiRequest) returns (HiResponse);
}
";
        let providers = parse_proto_providers(src, "p.proto");
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].service, "Public");
        assert_eq!(providers[0].method, "Hi");
    }

    #[test]
    fn strips_line_comments_inside_service_block() {
        let src = "\
package acme;
service Public {
  // rpc SkipThisOne (X) returns (Y);
  rpc Real (R) returns (RR);
}
";
        let providers = parse_proto_providers(src, "p.proto");
        assert_eq!(providers.len(), 1, "commented rpc must not be emitted");
        assert_eq!(providers[0].method, "Real");
    }

    #[test]
    fn empty_package_yields_bare_service_name() {
        let src = "service Plain { rpc M (R) returns (RR); }\n";
        let providers = parse_proto_providers(src, "p.proto");
        assert_eq!(providers[0].package, "");
        assert_eq!(
            compose_service_name(&providers[0].package, &providers[0].service),
            "Plain"
        );
    }

    #[test]
    fn compose_service_name_joins_with_dot() {
        assert_eq!(
            compose_service_name("com.acme.orders", "Orders"),
            "com.acme.orders.Orders"
        );
    }

    #[test]
    fn starts_with_keyword_requires_word_boundary() {
        let bytes = b"service Foo { rpc M (R) returns (RR); }";
        assert!(starts_with_keyword(bytes, 0, "service"));
        assert!(!starts_with_keyword(bytes, 0, "services"));
        let bytes2 = b"aservice Foo";
        assert!(!starts_with_keyword(bytes2, 0, "service"));
    }
}
