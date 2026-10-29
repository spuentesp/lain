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
//!    host string. When the constructor's first argument is a
//!    local bound from an env-get (`os.Getenv("X")` /
//!    `os.getenv("X")` / `System.getenv("X")`), the channel host
//!    is `HostPart::Env([X])` — the joiner resolves it through
//!    `services[].env` the same way the HTTP ladder does. The
//!    channel host is the consumer's `HostPart`; a literal host
//!    resolves through the existing `target_service_from_hosts`
//!    (Phase B/C). Unresolvable channels land on the coverage
//!    ledger as `Unresolved { reason: RpcStubUnknown }`
//!    (env-bound but unmapped channels land `EnvUnmapped`).
//!
//! The emitted `RpcConsumer` rides on the enclosing function
//! (or method) so the joiner can attach it to the right
//! `SymbolKey` without re-walking the AST.

use crate::error::LainError;
use crate::federation::contracts::model::{
    ContractFact, HostPart, RpcConsumerFact, RpcSystem, SourceSite,
};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{GraphEdge, GraphNode, RepoNamespace};
use crate::server::sensors::util::compose_service_name;
use std::collections::{BTreeMap, BTreeSet};
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
    /// joiner's `HostPart` shape: `Literal` for a captured
    /// `host:port`, `Env([var])` when the dial target comes from
    /// an env-get, `HostPart::None` when no channel is resolved
    /// (the joiner emits `Unresolved { reason: RpcStubUnknown }`).
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
            let id_name = format!(
                "{}{}:{}",
                crate::server::sensors::util::RPC_CALL_PREFIX,
                call.service,
                call.method
            );
            let mut node = crate::server::sensors::util::synthetic_site_node(
                id_name,
                &call.site.path,
                call.site.line,
                namespace,
            );
            let composed_service = compose_service_name(&call.package, &call.service);
            node.contract = vec![ContractFact::RpcConsumer(RpcConsumerFact {
                system: RpcSystem::Grpc,
                service: composed_service,
                method: call.method.clone(),
                channel_target: call.channel_target.clone(),
                channel_host_part: call.channel_host_part.clone(),
            })];
            all_nodes.push(node);
            total += 1;
        }
    }
    // Replace unconditionally. A scan that finds nothing must still
    // retract what a previous scan emitted — this guard used to skip
    // the only call that retracts, so deleting the last call site from
    // a repo left the old nodes forever. `sql_sensor` has always done
    // this unconditionally.
    graph.replace_sensor_output(
        SensorOwner::GrpcConsumerSensor,
        &all_nodes,
        &[] as &[GraphEdge],
    )?;
    Ok(total)
}

// ─── Detection ─────────────────────────────────────────────────────────

/// Detect every recognised stub call in `content`. Public so the
/// acceptance tests can exercise the detector without going
/// through the graph emission path.
pub fn detect_stub_calls(content: &str, ext: &str, graph_path: &str) -> Vec<GrpcStubCall> {
    let mut out: Vec<GrpcStubCall> = Vec::new();
    let channel = detect_channel(content, ext);
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

/// What the channel-construction scan found: a literal `host:port`
/// target, or the env var the target was assigned from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelTarget {
    Literal(String),
    Env(String),
}

/// The channel target for the file: a literal wins (the literal
/// pass is the historical behaviour); an env-get assignment is the
/// fallback when the constructor's first argument is not a string
/// literal (`target := os.Getenv("ORDERS_GRPC_ADDR")`).
fn detect_channel(content: &str, ext: &str) -> Option<ChannelTarget> {
    detect_channel_host(content, ext)
        .map(ChannelTarget::Literal)
        .or_else(|| detect_channel_env(content, ext).map(ChannelTarget::Env))
}

/// Env-get locals in the file: `name := os.Getenv("X")` (Go),
/// `name = os.getenv("X")` / `os.environ["X"]` (Python),
/// `String name = System.getenv("X")` (Java). The right-hand side
/// is recognised by the shared [`host_env_name`] grammar, so the
/// three languages need no per-shape code here.
fn collect_env_locals(content: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for line in content.lines() {
        let Some(eq) = line.find('=') else { continue };
        // Identifier before the `=`: the last token of the left
        // side, so Go's `target := …` and Java's
        // `String target = …` both yield `target`. `==` / `!=`
        // comparisons fall out because the last token is an
        // operator, not an identifier.
        let lhs = line[..eq].trim().trim_end_matches(':').trim();
        let name = lhs.split_whitespace().last().unwrap_or("");
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let rhs = line[eq + 1..].trim();
        if let Some(var) = crate::server::sensors::util::host_env_name(rhs) {
            out.insert(name.to_string(), var.to_string());
        }
    }
    out
}

/// The channel constructor's first argument is an env var when it
/// *is* an env call (`ManagedChannelBuilder.forAddress(System.getenv("X"), …)`)
/// or a bare identifier bound from one in the same file.
fn detect_channel_env(content: &str, ext: &str) -> Option<String> {
    let ctor = match ext {
        "go" => "grpc.NewClient(",
        "py" => "grpc.insecure_channel(",
        "java" => "ManagedChannelBuilder.forAddress(",
        _ => return None,
    };
    let env_locals = collect_env_locals(content);
    for line in content.lines() {
        let trimmed = line.trim();
        let Some(pos) = trimmed.find(ctor) else {
            continue;
        };
        let rest = &trimmed[pos + ctor.len()..];
        // Direct env call as the argument (Java). The shared
        // grammar cuts the name at the call's own `)`.
        if let Some(var) = crate::server::sensors::util::host_env_name(rest) {
            return Some(var.to_string());
        }
        // Bare identifier argument — resolve against the file's
        // env-get locals.
        let arg = rest.split([',', ')']).next().unwrap_or("").trim();
        if !arg.is_empty() && arg.chars().all(|c| c.is_alphanumeric() || c == '_') {
            if let Some(var) = env_locals.get(arg) {
                return Some(var.clone());
            }
            // The argument is a parameter, not an env-get local (an
            // SDK that takes the gateway address in its constructor).
            // Fall back to the file's gateway-shaped env read.
            return sole_file_env_var(&env_locals);
        }
    }
    None
}

/// The env var a file most plausibly uses as its gateway address, for a
/// channel whose address arrives as a constructor parameter. One distinct
/// env var is taken as is. With several, only address-shaped names
/// (`GATEWAY`, `HOST`, `URL`, `ADDR`, `ENDPOINT`, `SERVER`) count, and the
/// sensor refuses unless exactly one of those remains. This is a name
/// heuristic: the var names a gateway, which may resolve to an address
/// elsewhere, so the binding is service-level, not a verified dial address.
fn sole_file_env_var(env_locals: &BTreeMap<String, String>) -> Option<String> {
    const ADDRESS_SHAPED: [&str; 6] = ["GATEWAY", "HOST", "URL", "ADDR", "ENDPOINT", "SERVER"];
    let vars: BTreeSet<&String> = env_locals.values().collect();
    if vars.len() == 1 {
        return vars.into_iter().next().cloned();
    }
    let mut address = vars.into_iter().filter(|v| {
        ADDRESS_SHAPED
            .iter()
            .any(|shape| v.to_ascii_uppercase().contains(shape))
    });
    match (address.next(), address.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
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
    channel: &Option<ChannelTarget>,
    out: &mut Vec<GrpcStubCall>,
) {
    for det in crate::server::sensors::util_tokenize::detect_method_calls(
        content,
        |rcv| rcv.ends_with("Client"),
        |meth| !meth.is_empty(),
    ) {
        let service = strip_client_suffix(det.receiver);
        if service.is_empty() {
            continue;
        }
        let (channel_target, host_part) = host_parts_from_channel(channel);
        out.push(GrpcStubCall {
            service,
            package: String::new(),
            method: det.method.to_string(),
            channel_target,
            channel_host_part: host_part,
            site: SourceSite {
                path: graph_path.to_string(),
                line: det.line_no,
            },
        });
    }
}

fn detect_python_stub_calls(
    content: &str,
    graph_path: &str,
    channel: &Option<ChannelTarget>,
    out: &mut Vec<GrpcStubCall>,
) {
    let services = python_stub_services(content);
    for det in crate::server::sensors::util_tokenize::detect_method_calls(
        content,
        is_python_stub_receiver,
        |meth| !meth.is_empty(),
    ) {
        // A `<Name>Stub` receiver keeps its historical name
        // (`ordersStub` → `orders`). A handle like `_stub` / `stub`
        // carries no name, so it takes the service its constructor
        // was built from.
        let service = Some(strip_stub_suffix(det.receiver))
            .filter(|s| !s.is_empty())
            .or_else(|| services.get(det.receiver).cloned())
            .unwrap_or_default();
        if service.is_empty() {
            continue;
        }
        let (channel_target, host_part) = host_parts_from_channel(channel);
        out.push(GrpcStubCall {
            service,
            package: String::new(),
            method: det.method.to_string(),
            channel_target,
            channel_host_part: host_part,
            site: SourceSite {
                path: graph_path.to_string(),
                line: det.line_no,
            },
        });
    }
}

/// True when the receiver names a gRPC stub handle. The tokenizer
/// hands over the identifier run before the dot, so `self._stub.Health(`
/// arrives as `_stub` and `stub.Get(` as `stub`; `ordersStub` is the
/// generated-client spelling.
fn is_python_stub_receiver(rcv: &str) -> bool {
    rcv.to_ascii_lowercase().ends_with("stub")
}

/// The service each stub handle was built from, keyed by the variable
/// that holds it. The constructor is the only place a Python SDK names
/// the service: `self._stub = pb2_grpc.OpenShellStub(ch)` maps `_stub`
/// to `OpenShell`.
fn python_stub_services(content: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in content.lines() {
        let Some(eq) = line.find('=') else { continue };
        // `==` and friends are comparisons, not bindings.
        if line[eq + 1..].starts_with('=') {
            continue;
        }
        let lhs = line[..eq].trim();
        let rhs = line[eq + 1..].trim();
        let Some(service) = python_stub_constructor_service(rhs) else {
            continue;
        };
        // `self._stub` → `_stub`; `stub` → `stub`.
        let name = lhs.rsplit('.').next().unwrap_or(lhs);
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        out.insert(name.to_string(), service);
    }
    out
}

/// `openshell_pb2_grpc.OpenShellStub(channel)` → `OpenShell`.
fn python_stub_constructor_service(rhs: &str) -> Option<String> {
    let open = rhs.find('(')?;
    let head = rhs[..open].trim();
    let last = head.rsplit('.').next()?;
    let service = last.strip_suffix("Stub")?;
    if service.is_empty() {
        None
    } else {
        Some(service.to_string())
    }
}

fn detect_java_stub_calls(
    content: &str,
    graph_path: &str,
    channel: &Option<ChannelTarget>,
    out: &mut Vec<GrpcStubCall>,
) {
    for det in crate::server::sensors::util_tokenize::detect_method_calls(
        content,
        |rcv| rcv.ends_with("Client") || rcv.ends_with("Stub"),
        |meth| !meth.is_empty(),
    ) {
        let service = if det.receiver.ends_with("Client") {
            strip_client_suffix(det.receiver)
        } else {
            strip_stub_suffix(det.receiver)
        };
        if service.is_empty() {
            continue;
        }
        let (channel_target, host_part) = host_parts_from_channel(channel);
        out.push(GrpcStubCall {
            service,
            package: String::new(),
            method: det.method.to_string(),
            channel_target,
            channel_host_part: host_part,
            site: SourceSite {
                path: graph_path.to_string(),
                line: det.line_no,
            },
        });
    }
}

fn detect_cpp_stub_calls(
    content: &str,
    graph_path: &str,
    channel: &Option<ChannelTarget>,
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

fn host_parts_from_channel(channel: &Option<ChannelTarget>) -> (Option<String>, HostPart) {
    // `channel` is what the channel-detection pass extracted from
    // the channel constructor's first argument. A literal host is
    // kept verbatim in `channel_target` (the joiner surfaces it on
    // the ledger record) and projected into `HostPart::Literal` so
    // the existing `target_service_from_hosts` dispatch (Phase B /
    // C) can match it against `services[].hosts`. An env-bound
    // target carries no host:port literal — the var name rides on
    // `HostPart::Env` for the joiner's `services[].env` dispatch.
    match channel {
        Some(ChannelTarget::Literal(host)) => {
            let host_part = if host.is_empty() {
                HostPart::None
            } else {
                HostPart::Literal(host.clone())
            };
            (Some(host.clone()), host_part)
        }
        Some(ChannelTarget::Env(var)) => (None, HostPart::Env(vec![var.clone()])),
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

    /// OpenShell's Python SDK keeps the stub on the instance
    /// (`self._stub = openshell_pb2_grpc.OpenShellStub(...)`) and
    /// calls through it (`self._stub.Health(...)`). The service
    /// name lives in the constructor, not the receiver.
    #[test]
    fn python_member_receiver_stub_call_is_detected() {
        let src = "\
import grpc
import openshell_pb2_grpc

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)

    def health(self):
        return self._stub.Health(openshell_pb2.HealthRequest())
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].service, "OpenShell");
        assert_eq!(calls[0].method, "Health");
    }

    /// The lowercase receiver shape used by OpenShell's e2e tests.
    #[test]
    fn python_lowercase_stub_receiver_is_detected() {
        let src = "\
import grpc
import openshell_pb2_grpc

channel = grpc.insecure_channel(\"orders:50051\")
stub = openshell_pb2_grpc.OpenShellStub(channel)
resp = stub.GetCurrentUser(request)
";
        let calls = detect_stub_calls(src, "py", "client.py");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].service, "OpenShell");
        assert_eq!(calls[0].method, "GetCurrentUser");
        assert_eq!(calls[0].channel_target.as_deref(), Some("orders"));
    }

    /// A stub-shaped receiver with no constructor in the file has no
    /// service name; emitting a nameless consumer would put a
    /// phantom into the unresolved ledger.
    #[test]
    fn python_stub_receiver_without_constructor_emits_nothing() {
        let src = "\
class Client:
    def health(self):
        return self._stub.Health(request)
";
        let calls = detect_stub_calls(src, "py", "client.py");
        assert!(calls.is_empty());
    }

    /// OpenShell's Python SDK takes the gateway address as a
    /// constructor parameter and reads the gateway name from the
    /// environment elsewhere in the file. With one env read in the
    /// file, the channel resolves to that variable.
    #[test]
    fn python_stub_call_resolves_channel_from_file_env() {
        let src = "\
import os
import grpc
import openshell_pb2_grpc

def _resolve_active_cluster() -> str:
    env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")
    return env_gateway

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)

    def health(self):
        return self._stub.Health(request)
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0].channel_host_part,
            HostPart::Env(ref vars) if vars == &["OPENSHELL_GATEWAY".to_string()]));
    }

    /// The real OpenShell `sandbox.py` reads two env vars, one of them
    /// `XDG_CONFIG_HOME`. The gateway-shaped one is the channel's source.
    #[test]
    fn python_stub_call_prefers_the_gateway_env_among_two_reads() {
        let src = "\
import os
import grpc
import openshell_pb2_grpc

def _xdg_config_home() -> str:
    configured = os.environ.get(\"XDG_CONFIG_HOME\")
    return configured

def _resolve_active_cluster() -> str:
    env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")
    return env_gateway

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)

    def health(self):
        return self._stub.Health(request)
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0].channel_host_part,
            HostPart::Env(ref vars) if vars == &["OPENSHELL_GATEWAY".to_string()]));
    }

    /// Two env reads that both look like addresses: the channel must not guess.
    #[test]
    fn python_stub_call_with_two_address_env_reads_stays_unknown() {
        let src = "\
import os
import grpc
import openshell_pb2_grpc

a = os.environ.get(\"OPENSHELL_GATEWAY\")
b = os.environ.get(\"OPENSHELL_ENDPOINT\")

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)

    def health(self):
        return self._stub.Health(request)
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0].channel_host_part, HostPart::None));
    }

    /// Two env reads, neither address-shaped: the channel must not guess.
    #[test]
    fn python_stub_call_with_two_non_address_env_reads_stays_unknown() {
        let src = "\
import os
import grpc
import openshell_pb2_grpc

a = os.environ.get(\"XDG_CONFIG_HOME\")
b = os.environ.get(\"HOME\")

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)

    def health(self):
        return self._stub.Health(request)
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0].channel_host_part, HostPart::None));
    }

    /// Two locals reading the same variable are still one env read.
    #[test]
    fn python_stub_call_with_repeated_env_var_resolves() {
        let src = "\
import os
import grpc
import openshell_pb2_grpc

a = os.environ.get(\"OPENSHELL_GATEWAY\")
b = os.getenv(\"OPENSHELL_GATEWAY\")

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)

    def health(self):
        return self._stub.Health(request)
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert!(matches!(&calls[0].channel_host_part,
            HostPart::Env(ref vars) if vars == &["OPENSHELL_GATEWAY".to_string()]));
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

    /// Go gRPC whose dial target comes from `os.Getenv` — the
    /// enterprise-mesh logistics-svc shape. The channel host must
    /// surface as `HostPart::Env([var])` so the joiner can bind it
    /// through `services[].env` (or land `EnvUnmapped` with the var
    /// visible) instead of `RpcStubUnknown`.
    #[test]
    fn go_stub_call_resolves_env_channel() {
        let src = "\
package logistics

import (
\t\"context\"
\t\"os\"

\t\"google.golang.org/grpc\"
\t\"logistics-svc/pb\"
)

func fetchOrder(ctx context.Context) (*pb.Order, error) {
\ttarget := os.Getenv(\"ORDERS_GRPC_ADDR\")
\tconn, err := grpc.NewClient(target, grpc.WithTransportCredentials(insecure.NewCredentials()))
\tif err != nil {
\t\treturn nil, err
\t}
\tordersClient := pb.NewOrdersClient(conn)
\treturn ordersClient.GetOrder(ctx, &pb.GetOrderRequest{Id: \"1\"})
}
";
        let calls = detect_stub_calls(src, "go", "handler.go");
        assert_eq!(calls.len(), 1, "expected one stub call: {calls:?}");
        let c = &calls[0];
        assert_eq!(c.service, "orders");
        assert_eq!(c.method, "GetOrder");
        assert!(
            matches!(&c.channel_host_part, HostPart::Env(names) if names == &vec!["ORDERS_GRPC_ADDR".to_string()]),
            "dial target from os.Getenv must surface as HostPart::Env; got {:?}",
            c.channel_host_part
        );
        assert!(
            c.channel_target.is_none(),
            "an env dial target is not a host:port literal; got {:?}",
            c.channel_target
        );
    }

    /// A literal channel target keeps winning even when the file
    /// also has env-get assignments — the literal pass runs first.
    #[test]
    fn go_stub_literal_channel_wins_over_env_assignment() {
        let src = "\
package main
func main() {
    addr := os.Getenv(\"ORDERS_GRPC_ADDR\")
    client := grpc.NewClient(\"orders:50051\")
    ordersClient := pb.NewOrdersClient(client)
    resp, err := ordersClient.Get(ctx, &pb.GetRequest{Id: id})
    _ = resp
}
";
        let calls = detect_stub_calls(src, "go", "main.go");
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            calls[0].channel_host_part,
            HostPart::Literal(ref h) if h == "orders"
        ));
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
