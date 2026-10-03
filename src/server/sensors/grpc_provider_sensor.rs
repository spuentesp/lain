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
use crate::federation::contracts::model::{ContractFact, RpcProviderFact, RpcSystem, SourceSite};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{GraphEdge, GraphNode, NodeType, RepoNamespace};
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

crate::server::sensors::register_sensor!(
    GrpcProviderSensor,
    "grpc_provider",
    Proto,
    scan_workspace_grpc
);

// ─── Workspace scan ───────────────────────────────────────────────────

/// Walk `root`, find every `.proto` file, parse the service
/// surface, and emit one `RpcProvider` `Module` node per
/// `(package, service, method)` triple. Returns the count of
/// `RpcProvider` nodes minted.
pub fn scan_workspace_grpc(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
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
    for (path, content, _tag) in crate::server::sensors::util::scan_files(root, proto_ext) {
        let graph_path_str = graph_path(root, &path);
        let providers = parse_proto_providers(&content, &graph_path_str);
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
        }
    }
    if !all_nodes.is_empty() {
        // The grpc provider sensor owns its own nodes; a rescan
        // retracts the previous output before inserting the new
        // one. Using the same `SensorOwner::ProtoSensor` owner
        // keeps the contract with the existing proto_sensor
        // (which also writes Module nodes keyed off the proto
        // path) — the E1 acceptance test confirms the two
        // sensors coexist without overwriting each other.
        let _ =
            graph.replace_sensor_output(SensorOwner::ProtoSensor, &all_nodes, &[] as &[GraphEdge]);
    }
    Ok(total)
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
/// line continuation. The newlines inside `option { ... }` are
/// preserved so braces stay balanced.
fn join_continued_lines(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_ended_with_continuation = false;
    for ch in input.chars() {
        if prev_ended_with_continuation && ch == '\n' {
            prev_ended_with_continuation = false;
            continue;
        }
        prev_ended_with_continuation = ch == '\\';
        out.push(ch);
    }
    out
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
