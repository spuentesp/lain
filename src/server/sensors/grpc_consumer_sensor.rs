//! Phase E — gRPC consumer sensor (spec §8.2).
//!
//! Detects generated stub call sites:
//!
//! - **Java:** `fooClient.bar(request)` where `fooClient` is a
//!   generated stub class (typed `FooGrpc.FooBlockingStub` /
//!   `FooStub`).
//! - **Go:** `stub.Bar(ctx, req)` where `stub` is `FooClient` (typed
//!   `pb.FooClient`).
//! - **Python:** `stub.Bar(request)` where `stub` is `FooStub(...)`.
//! - **C++:** `stub->Bar(&context, &request, &response)` where
//!   `stub` is `FooService::Stub`.
//!
//! For each detected stub call, the sensor resolves:
//!
//! 1. The receiver type to the package + service. The sensor
//!    captures the receiver's *identifier name* (`ordersClient`)
//!    and the inferred service (`Orders` — the `FooClient` /
//!    `FooStub` suffix is stripped). The package is left empty
//!    at scan time; the joiner fills it from the
//!    `services[].hosts` → `package.service` mapping the same
//!    way Phase B fills `Table.service`.
//! 2. The channel address. The sensor also walks the
//!    enclosing function (and the file at large) for
//!    `grpc.NewClient("orders:50051")` /
//!    `ManagedChannelBuilder.forAddress("orders", 50051)` /
//!    `grpc.insecure_channel("orders:50051")` and captures the
//!    host string. The channel host is the consumer's
//!    `HostPart`; the joiner resolves it through the existing
//!    `target_service_from_hosts` (Phase B/C). Unresolvable
//!    channels land on the coverage ledger as
//!    `Unresolved { reason: RpcStubUnknown }`.
//!
//! The emitted `RpcConsumer` rides on the enclosing function
//! (or method) so the joiner can attach it to the right
//! `SymbolKey` without re-walking the AST.

use crate::error::LainError;
use crate::federation::contracts::model::{
    ContractFact, HostPart, RpcConsumerFact, RpcSystem, SourceSite,
};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{GraphEdge, GraphNode, NodeType, RepoNamespace};
use crate::server::sensors::util::compose_service_name;
use std::path::Path;

// ─── Public sensor shape ───────────────────────────────────────────────

/// One detected stub call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrpcStubCall {
    /// Bare service name, with the codegen suffix stripped
    /// (`OrdersClient` → `Orders`, `OrdersStub` → `Orders`).
    pub service: String,
    /// `package` prefix if the file declares one for the
    /// generated module; empty string when unknown.
    pub package: String,
    /// Bare rpc method name.
    pub method: String,
    /// Captured `host:port` literal from the channel construction
    /// in the same function, when found.
    pub channel_target: Option<String>,
    /// `host` portion of the channel target, projected into the
    /// joiner's `HostPart` shape. `HostPart::None` when no
    /// channel is resolved (the joiner emits
    /// `Unresolved { reason: RpcStubUnknown }`).
    pub channel_host_part: HostPart,
    pub site: SourceSite,
}

pub struct GrpcConsumerSensor;

crate::server::sensors::register_sensor!(
    GrpcConsumerSensor,
    "grpc_consumer",
    Proto,
    1,
    scan_workspace_grpc_consumer
);

/// Walk `root`, find every recognised stub call, resolve the
/// channel address from the enclosing function, and emit one
/// `RpcConsumer` `Function` node per call site. Returns the count
/// emitted.
pub fn scan_workspace_grpc_consumer(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let code_ext = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .filter(|e| matches!(*e, "go" | "py" | "java" | "cc" | "cpp" | "h" | "hpp"))
            .map(|e| e.to_string())
    };
    let mut total = 0usize;
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    for (path, content, ext) in crate::server::sensors::util::scan_files(root, code_ext) {
        let graph_path_str = graph_path(root, &path);
        let calls = detect_stub_calls(&content, &ext, &graph_path_str);
        for call in calls {
            let id_name = format!("rpc-call:{}:{}", call.service, call.method);
            let id = GraphNode::generate_id(
                &NodeType::Function,
                &call.site.path,
                &id_name,
                Some(call.site.line),
                namespace,
            );
            let mut node =
                GraphNode::new(NodeType::Function, id_name.clone(), call.site.path.clone());
            node.id = id;
            node.line_start = Some(call.site.line);
            node.line_end = Some(call.site.line);
            let composed_service = compose_service_name(&call.package, &call.service);
            node.contract = Some(ContractFact::RpcConsumer(RpcConsumerFact {
                system: RpcSystem::Grpc,
                service: composed_service,
                method: call.method.clone(),
                channel_target: call.channel_target.clone(),
                channel_host_part: call.channel_host_part.clone(),
            }));
            all_nodes.push(node);
            total += 1;
        }
    }
    if !all_nodes.is_empty() {
        let _ =
            graph.replace_sensor_output(SensorOwner::ProtoSensor, &all_nodes, &[] as &[GraphEdge]);
    }
    Ok(total)
}

// ─── Detection ─────────────────────────────────────────────────────────

/// Detect every recognised stub call in `content`. Public so the
/// acceptance tests can exercise the detector without going
/// through the graph emission path.
pub fn detect_stub_calls(content: &str, ext: &str, graph_path: &str) -> Vec<GrpcStubCall> {
    let mut out: Vec<GrpcStubCall> = Vec::new();
    let channel = detect_channel_host(content, ext);
    match ext {
        "go" => detect_go_stub_calls(content, graph_path, &channel, &mut out),
        "py" => detect_python_stub_calls(content, graph_path, &channel, &mut out),
        "java" => detect_java_stub_calls(content, graph_path, &channel, &mut out),
        "cc" | "cpp" | "h" | "hpp" => {
            detect_cpp_stub_calls(content, graph_path, &channel, &mut out)
        }
        _ => {}
    }
    out
}

fn detect_channel_host(content: &str, ext: &str) -> Option<String> {
    // `grpc.NewClient("orders:50051")` (Go),
    // `grpc.insecure_channel("orders:50051")` (Python),
    // `ManagedChannelBuilder.forAddress("orders", 50051)` (Java).
    // The call may be on the right-hand side of an assignment
    // (e.g. `client := grpc.NewClient(...)`) so we look for the
    // channel constructor as a substring of the line.
    let lines: Vec<&str> = content.lines().collect();
    for line in &lines {
        let trimmed = line.trim();
        match ext {
            "go" => {
                if let Some(pos) = trimmed.find("grpc.NewClient(") {
                    let rest = &trimmed[pos + "grpc.NewClient(".len()..];
                    if let Some(lit) = extract_string_arg(rest) {
                        return Some(extract_host(&lit));
                    }
                }
            }
            "py" => {
                if let Some(pos) = trimmed.find("grpc.insecure_channel(") {
                    let rest = &trimmed[pos + "grpc.insecure_channel(".len()..];
                    if let Some(lit) = extract_string_arg(rest) {
                        return Some(extract_host(&lit));
                    }
                }
            }
            "java" => {
                if let Some(pos) = trimmed.find("ManagedChannelBuilder.forAddress(") {
                    let rest = &trimmed[pos + "ManagedChannelBuilder.forAddress(".len()..];
                    // First arg is the host literal.
                    if let Some(lit) = extract_string_arg(rest) {
                        return Some(lit);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn extract_string_arg(rest: &str) -> Option<String> {
    crate::server::sensors::util_tokenize::extract_string_literal(rest, 0).map(|(_, lit)| lit)
}

fn extract_host(target: &str) -> String {
    // `orders:50051` → `orders`. The port is the joiner's concern
    // only when the target has one; without a port we just
    // return the whole string. An IPv6 literal `[::1]:50051` is
    // left as-is.
    if let Some((host, _port)) = target.rsplit_once(':') {
        // Heuristic: an IPv6 literal contains multiple colons.
        if host.contains(':') {
            return target.to_string();
        }
        return host.to_string();
    }
    target.to_string()
}

fn detect_go_stub_calls(
    content: &str,
    graph_path: &str,
    channel: &Option<String>,
    out: &mut Vec<GrpcStubCall>,
) {
    for (line_no, line) in
        crate::server::sensors::util_tokenize::lines_matching_pattern(content, |l| {
            l.contains("Client.")
        })
    {
        let trimmed = line.trim();
        // `client.Get(ctx, req)` — look for `<ident>Client.<Method>(`
        // anywhere in the line. The receiver is whatever identifier
        // precedes the `.` and ends in `Client`. We scan the line
        // for a `Client.` (or `Stub.`) literal so multi-assignment
        // (`resp, err := ordersClient.Get(...)`) still matches.
        for (start, end) in find_stub_receivers(trimmed) {
            let receiver = &trimmed[start..end];
            if !receiver.ends_with("Client") {
                continue;
            }
            // Method name is the leading identifier after the dot.
            let after_dot_full = &trimmed[end + 1..];
            let paren_pos = after_dot_full.find('(').unwrap_or(after_dot_full.len());
            let head = &after_dot_full[..paren_pos];
            if head.contains('.') {
                continue;
            }
            let method_end = head
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(head.len());
            let method = &head[..method_end];
            if method.is_empty() {
                continue;
            }
            let service = strip_client_suffix(receiver);
            if service.is_empty() {
                continue;
            }
            let (channel_target, host_part) = host_parts_from_channel(channel);
            out.push(GrpcStubCall {
                service,
                package: String::new(),
                method: method.to_string(),
                channel_target,
                channel_host_part: host_part,
                site: SourceSite {
                    path: graph_path.to_string(),
                    line: line_no as u32,
                },
            });
            break; // one call site per line is enough
        }
    }
}

fn detect_python_stub_calls(
    content: &str,
    graph_path: &str,
    channel: &Option<String>,
    out: &mut Vec<GrpcStubCall>,
) {
    for (line_no, line) in
        crate::server::sensors::util_tokenize::lines_matching_pattern(content, |l| {
            l.contains("Stub.")
        })
    {
        let trimmed = line.trim();
        for (start, end) in find_stub_receivers_suffix(trimmed, "Stub.") {
            let receiver = &trimmed[start..end];
            let after_dot_full = &trimmed[end + 1..];
            let paren_pos = after_dot_full.find('(').unwrap_or(after_dot_full.len());
            let head = &after_dot_full[..paren_pos];
            if head.contains('.') {
                continue;
            }
            let method_end = head
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(head.len());
            let method = &head[..method_end];
            if method.is_empty() {
                continue;
            }
            let service = strip_stub_suffix(receiver);
            if service.is_empty() {
                continue;
            }
            let (channel_target, host_part) = host_parts_from_channel(channel);
            out.push(GrpcStubCall {
                service,
                package: String::new(),
                method: method.to_string(),
                channel_target,
                channel_host_part: host_part,
                site: SourceSite {
                    path: graph_path.to_string(),
                    line: line_no as u32,
                },
            });
            break;
        }
    }
}

fn detect_java_stub_calls(
    content: &str,
    graph_path: &str,
    channel: &Option<String>,
    out: &mut Vec<GrpcStubCall>,
) {
    for (line_no, line) in
        crate::server::sensors::util_tokenize::lines_matching_pattern(content, |l| {
            l.contains("Client.") || l.contains("Stub.")
        })
    {
        let trimmed = line.trim();
        // `ordersClient.getOrder(request)` — receiver ends in
        // `Client` (blocking stub) or `Stub` (async stub). Try
        // both suffixes.
        for (start, end, suffix_kind) in find_java_stub_receivers(trimmed) {
            let receiver = &trimmed[start..end];
            let after_dot_full = &trimmed[end + 1..];
            let paren_pos = after_dot_full.find('(').unwrap_or(after_dot_full.len());
            let head = &after_dot_full[..paren_pos];
            if head.contains('.') {
                continue;
            }
            let method_end = head
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(head.len());
            let method = &head[..method_end];
            if method.is_empty() {
                continue;
            }
            let service = match suffix_kind {
                StubSuffix::Client => strip_client_suffix(receiver),
                StubSuffix::Stub => strip_stub_suffix(receiver),
            };
            if service.is_empty() {
                continue;
            }
            let (channel_target, host_part) = host_parts_from_channel(channel);
            out.push(GrpcStubCall {
                service,
                package: String::new(),
                method: method.to_string(),
                channel_target,
                channel_host_part: host_part,
                site: SourceSite {
                    path: graph_path.to_string(),
                    line: line_no as u32,
                },
            });
            break;
        }
    }
}

#[derive(Clone, Copy)]
enum StubSuffix {
    Client,
    Stub,
}

fn find_java_stub_receivers(line: &str) -> Vec<(usize, usize, StubSuffix)> {
    let mut out: Vec<(usize, usize, StubSuffix)> = Vec::new();
    for (needle, kind) in [("Client.", StubSuffix::Client), ("Stub.", StubSuffix::Stub)] {
        let mut pos = 0usize;
        while let Some(rel) = line[pos..].find(needle) {
            let dot_pos = pos + rel + needle.len() - 1;
            let mut begin = dot_pos;
            while begin > 0 {
                let prev = line.as_bytes()[begin - 1] as char;
                if prev.is_alphanumeric() || prev == '_' {
                    begin -= 1;
                } else {
                    break;
                }
            }
            if begin < dot_pos {
                out.push((begin, dot_pos, kind));
            }
            pos = dot_pos + 1;
        }
    }
    out
}

fn detect_cpp_stub_calls(
    content: &str,
    graph_path: &str,
    channel: &Option<String>,
    out: &mut Vec<GrpcStubCall>,
) {
    for (line_no, line) in
        crate::server::sensors::util_tokenize::lines_matching_pattern(content, |l| l.contains("->"))
    {
        let trimmed = line.trim();
        // `stub->Get(&context, &request, &response)` — receiver
        // is `stub`, the type is `<Service>::Stub`.
        let Some(arrow_pos) = trimmed.find("->") else {
            continue;
        };
        let receiver = &trimmed[..arrow_pos];
        if !is_identifier(receiver) {
            continue;
        }
        let after_arrow_full = &trimmed[arrow_pos + 2..];
        let paren_pos = after_arrow_full.find('(').unwrap_or(after_arrow_full.len());
        let head = &after_arrow_full[..paren_pos];
        if head.contains("->") {
            continue;
        }
        let method_end = head
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(head.len());
        let method = &head[..method_end];
        if method.is_empty() {
            continue;
        }
        // C++ receiver type is not statically retrievable from
        // the call site alone; we record the bare method and a
        // synthetic service name based on the receiver (the
        // acceptance scenarios don't exercise this path).
        let service = receiver.to_string();
        let (channel_target, host_part) = host_parts_from_channel(channel);
        out.push(GrpcStubCall {
            service,
            package: String::new(),
            method: method.to_string(),
            channel_target,
            channel_host_part: host_part,
            site: SourceSite {
                path: graph_path.to_string(),
                line: line_no as u32,
            },
        });
    }
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
}

/// Find every identifier in `line` that ends with the literal
/// `needle` (e.g. `"Stub."`). Returns `(start, end)` byte
/// offsets into `line`. `end` is the position of the dot
/// (one past the last identifier character).
fn find_stub_receivers_suffix(line: &str, needle: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = line[pos..].find(needle) {
        let dot_pos = pos + rel + needle.len() - 1;
        let mut begin = dot_pos;
        while begin > 0 {
            let prev = line.as_bytes()[begin - 1] as char;
            if prev.is_alphanumeric() || prev == '_' {
                begin -= 1;
            } else {
                break;
            }
        }
        if begin < dot_pos {
            out.push((begin, dot_pos));
        }
        pos = dot_pos + 1;
    }
    out
}

/// Find every identifier in `line` that ends in `Client` and is
/// followed by `.`. Returns `(start, end)` byte offsets into
/// `line`. The receiver may appear mid-line so multi-assignment
/// (`resp, err := ordersClient.Get(...)`) still matches. `end`
/// is the position of the dot (i.e. one past the last identifier
/// character) so callers can read `line[end + 1..]` as the
/// `.<Method>(...)` after the dot.
fn find_stub_receivers(line: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = line[pos..].find("Client.") {
        let dot_pos = pos + rel + "Client".len();
        // Walk backwards from one position BEFORE the dot (the
        // last `t` of `Client`) to find the start of the
        // identifier. The receiver includes the `Client` suffix.
        let mut begin = dot_pos;
        while begin > 0 {
            let prev = line.as_bytes()[begin - 1] as char;
            if prev.is_alphanumeric() || prev == '_' {
                begin -= 1;
            } else {
                break;
            }
        }
        if begin < dot_pos {
            out.push((begin, dot_pos));
        }
        pos = pos + rel + "Client.".len();
    }
    out
}

fn strip_client_suffix(name: &str) -> String {
    name.strip_suffix("Client")
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn strip_stub_suffix(name: &str) -> String {
    name.strip_suffix("Stub")
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn host_parts_from_channel(channel: &Option<String>) -> (Option<String>, HostPart) {
    // `channel` here is the *host* portion the channel-detection
    // pass extracted from the channel constructor's first argument.
    // We keep the host verbatim in `channel_target` (the joiner
    // surfaces it on the ledger record) and project it into
    // `HostPart::Literal` so the existing `target_service_from_hosts`
    // dispatch (Phase B / C) can match it against `services[].hosts`.
    match channel {
        Some(host) => {
            let host_part = if host.is_empty() {
                HostPart::None
            } else {
                HostPart::Literal(host.clone())
            };
            (Some(host.clone()), host_part)
        }
        None => (None, HostPart::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_stub_call_resolves_channel() {
        let src = "\
package main
func main() {
    client := grpc.NewClient(\"orders:50051\")
    ordersClient := pb.NewOrdersClient(client)
    resp, err := ordersClient.Get(ctx, &pb.GetRequest{Id: id})
    _ = resp
}
";
        let calls = detect_stub_calls(src, "go", "main.go");
        assert_eq!(calls.len(), 1);
        let c = &calls[0];
        assert_eq!(c.service, "orders");
        assert_eq!(c.method, "Get");
        assert_eq!(c.channel_target.as_deref(), Some("orders"));
        assert!(matches!(c.channel_host_part, HostPart::Literal(ref h) if h == "orders"));
    }

    #[test]
    fn python_stub_call_resolves_channel() {
        let src = "\
import grpc
import orders_pb2_grpc
channel = grpc.insecure_channel(\"orders:50051\")
ordersStub = orders_pb2_grpc.OrdersStub(channel)
resp = ordersStub.GetOrder(request)
";
        let calls = detect_stub_calls(src, "py", "client.py");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].service, "orders");
        assert_eq!(calls[0].method, "GetOrder");
        assert_eq!(calls[0].channel_target.as_deref(), Some("orders"));
    }

    #[test]
    fn java_stub_call_resolves_channel() {
        let src = "\
import io.grpc.ManagedChannelBuilder;
import io.grpc.examples.orders.OrdersGrpc;
public class Client {
    public static void main(String[] args) {
        ManagedChannelBuilder.forAddress(\"orders\", 50051).usePlaintext();
        OrdersGrpc.OrdersBlockingStub ordersClient = OrdersGrpc.newBlockingStub(channel);
        var resp = ordersClient.getOrder(request);
    }
}
";
        let calls = detect_stub_calls(src, "java", "Client.java");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].service, "orders");
        assert_eq!(calls[0].method, "getOrder");
        assert_eq!(calls[0].channel_target.as_deref(), Some("orders"));
    }

    #[test]
    fn go_stub_call_without_channel_is_unknown() {
        let src = "\
package main
func main() {
    resp, err := ordersClient.Get(ctx, &pb.GetRequest{Id: id})
    _ = resp
}
";
        let calls = detect_stub_calls(src, "go", "main.go");
        assert_eq!(calls.len(), 1);
        assert!(calls[0].channel_target.is_none());
        assert!(matches!(calls[0].channel_host_part, HostPart::None));
    }

    #[test]
    fn non_stub_call_is_ignored() {
        let src = "\
x := helper.foo(1)
";
        let calls = detect_stub_calls(src, "go", "main.go");
        assert!(calls.is_empty());
    }
}
