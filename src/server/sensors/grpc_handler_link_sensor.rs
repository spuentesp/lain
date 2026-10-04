//! Phase E — gRPC server-registration sensor (spec §8.2).
//!
//! Recognises the per-language call patterns that link a generated
//! proto service to its user-supplied implementing handler. The
//! sensor is regex-first (the patterns are stable enough that
//! tree-sitter buys nothing extra) and emits one
//! `ContractFact::RpcHandler { rpc_service, handler_function,
//! origin }` per detected registration.
//!
//! Recognised patterns (spec §8.2):
//!
//! - **C++ / gRPC C++:** `RegisterFooServer(...)` and
//!   `add_FooServicer_to_server(...)` — the gRPC C++ codegen
//!   emits both for every service. We match `RegisterFooServer`
//!   (the more common case) and recognise `add_FooServicer_to_server`
//!   as a synonym.
//! - **Java @GrpcService:** `@GrpcService(impl = FooServiceImpl.class)`
//!   — the Spring Boot gRPC starter annotation. The handler is
//!   the `FooServiceImpl` class declared on the next non-comment
//!   line.
//! - **Go:** `pb.RegisterFooServer(s, &fooImpl{})` — the gRPC-Go
//!   codegen pattern. The handler is the second argument.
//! - **Python:** `add_FooServicer_to_server(foo_impl(), server)` —
//!   the gRPC Python codegen pattern. The handler is the first
//!   argument.
//!
//! The emitted `RpcHandler` is what `ContractKey::Rpc` providers
//! get their `handler` field from. The joiner links the handler
//! `SymbolKey` back to the provider on the per-repo graph so
//! typed traversal `handler → function → rpc` is reachable.

use crate::error::LainError;
use crate::federation::contracts::model::{
    ContractFact, ContractKey, RpcHandlerFact, RpcHandlerOrigin, RpcSystem, SymbolKey,
};
use crate::federation::repo_id::RepoId;
use crate::graph::{GraphDatabase, SensorOwner};
use crate::schema::{GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::path::Path;

// ─── Public sensor shape ───────────────────────────────────────────────

/// One server-registration link detected in a source file. The
/// `handler_function` is a `SymbolKey` the sensor resolved from
/// the call's textual pattern (the second arg in Go, the first
/// arg in Python, the `impl = X.class` value in Java Spring, the
/// argument to `RegisterFooServer` in C++).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcHandlerLink {
    pub rpc_service: String,
    pub rpc_method_scope: String, // currently the bare service name
    pub handler_function: SymbolKey,
    pub origin: RpcHandlerOrigin,
    pub site_line: u32,
}

pub struct GrpcHandlerLinkSensor;

crate::server::sensors::register_sensor!(
    GrpcHandlerLinkSensor,
    "grpc_handler_link",
    Proto,
    1,
    scan_workspace_handler_link
);

/// Walk `root`, find every recognised server-registration call,
/// resolve the handler symbol, and emit one `Module` node carrying
/// `ContractFact::RpcHandler` per match. Returns the count emitted.
pub fn scan_workspace_handler_link(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let repo_id = RepoId::new(root.to_string_lossy().as_ref())
        .unwrap_or_else(|_| RepoId::new("unknown").expect("valid repo id"));
    let code_ext = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .filter(|e| matches!(*e, "go" | "py" | "java" | "cc" | "cpp" | "h" | "hpp"))
            .map(|e| e.to_string())
    };
    let mut total = 0usize;
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    for (path, content, ext) in crate::server::sensors::util::scan_files(root, code_ext) {
        let graph_path_str = crate::graph::graph_path(root, &path);
        let links = detect_handler_links(&content, &ext, &graph_path_str, &repo_id);
        for link in links {
            let id_name = format!("rpc-handler:{}", link.handler_function.name);
            let id = GraphNode::generate_id(
                &NodeType::Module,
                &graph_path_str,
                &id_name,
                Some(link.site_line),
                namespace,
            );
            let mut node =
                GraphNode::new(NodeType::Module, id_name.clone(), graph_path_str.clone());
            node.id = id;
            node.line_start = Some(link.site_line);
            node.line_end = Some(link.site_line);
            let key = ContractKey::Rpc {
                system: RpcSystem::Grpc,
                service: link.rpc_service.clone(),
                method: link.rpc_method_scope.clone(),
            };
            node.contract = Some(ContractFact::RpcHandler(RpcHandlerFact {
                rpc_service: key,
                handler_function: link.handler_function.clone(),
                origin: link.origin,
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

/// Detect every recognised server-registration call in `content`.
/// Public so the acceptance tests can exercise the detector
/// without going through the graph emission path.
pub fn detect_handler_links(
    content: &str,
    ext: &str,
    graph_path: &str,
    repo_id: &RepoId,
) -> Vec<RpcHandlerLink> {
    let mut out: Vec<RpcHandlerLink> = Vec::new();
    match ext {
        "go" => detect_go(content, graph_path, repo_id, &mut out),
        "py" => detect_python(content, graph_path, repo_id, &mut out),
        "java" => detect_java(content, graph_path, repo_id, &mut out),
        "cc" | "cpp" | "h" | "hpp" => detect_cpp(content, graph_path, repo_id, &mut out),
        _ => {}
    }
    out
}

fn detect_go(
    content: &str,
    graph_path: &str,
    repo_id: &crate::federation::repo_id::RepoId,
    out: &mut Vec<RpcHandlerLink>,
) {
    // `pb.RegisterFooServer(s, &fooImpl{})` — extract the service
    // name from `Register<Name>Server` and the handler name from
    // the second argument (`&fooImpl{}`). The codegen call is
    // usually prefixed with the package (`pb.RegisterOrdersServer`)
    // so we look for `Register` anywhere on the line.
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        let Some(reg_pos) = trimmed.find("Register") else {
            continue;
        };
        let rest = &trimmed[reg_pos + "Register".len()..];
        // The call ends with `Server(...)`; we extract the service
        // name from `Register<Name>Server`.
        let Some(server_pos) = rest.find("Server(") else {
            continue;
        };
        let service = rest[..server_pos].trim().to_string();
        if service.is_empty() {
            continue;
        }
        // The handler is the second argument; pull it out of the
        // full call.
        let Some(handler_name) = extract_go_handler_arg(rest) else {
            continue;
        };
        if handler_name.is_empty() {
            continue;
        }
        out.push(RpcHandlerLink {
            rpc_service: service.clone(),
            rpc_method_scope: service,
            handler_function: SymbolKey {
                repo: repo_id.clone(),
                path: graph_path.to_string(),
                container: None,
                name: handler_name,
            },
            origin: RpcHandlerOrigin::GoRegister,
            site_line: line_no,
        });
    }
}

fn extract_go_handler_arg(rest: &str) -> Option<String> {
    // The handler is the second argument, prefixed with `&` (e.g.
    // `s, &fooImpl{}`). Find `, &` and take the identifier up to
    // `{` or end-of-token.
    let comma_pos = rest.find(',')?;
    let after = rest[comma_pos + 1..].trim();
    let after = after.strip_prefix('&')?;
    let name: String = after
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn detect_python(
    content: &str,
    graph_path: &str,
    repo_id: &crate::federation::repo_id::RepoId,
    out: &mut Vec<RpcHandlerLink>,
) {
    // `add_FooServicer_to_server(foo_impl(), server)` — extract
    // the service name from `add_<Name>Servicer_to_server` and
    // the handler name from the first argument. The codegen
    // call site is typically `pkg.add_OrdersServicer_to_server(...)`
    // so we search for the pattern rather than requiring it
    // to be at the start of the line.
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        let Some(add_pos) = trimmed.find("add_") else {
            continue;
        };
        let after_add = &trimmed[add_pos + "add_".len()..];
        let Some(suffix_pos) = after_add.find("Servicer_to_server(") else {
            continue;
        };
        let service = after_add[..suffix_pos].trim().to_string();
        // The handler is the first argument inside the parens
        // following the `Servicer_to_server(` call.
        let after_paren = &trimmed[trimmed.find('(').unwrap_or(0) + 1..];
        let handler_name = after_paren
            .split(',')
            .next()
            .map(|s| s.trim().trim_end_matches("()").to_string())
            .unwrap_or_default();
        if !service.is_empty() && !handler_name.is_empty() {
            out.push(RpcHandlerLink {
                rpc_service: service.clone(),
                rpc_method_scope: service,
                handler_function: SymbolKey {
                    repo: repo_id.clone(),
                    path: graph_path.to_string(),
                    container: None,
                    name: handler_name,
                },
                origin: RpcHandlerOrigin::PythonServicer,
                site_line: line_no,
            });
        }
    }
}

fn detect_java(
    content: &str,
    graph_path: &str,
    repo_id: &crate::federation::repo_id::RepoId,
    out: &mut Vec<RpcHandlerLink>,
) {
    // `@GrpcService(impl = FooServiceImpl.class) class FooServiceImpl
    // extends OrdersGrpc.OrdersImplBase` — record the link with the
    // service name extracted from the `extends` clause.
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        if !trimmed.contains("@GrpcService") {
            continue;
        }
        // Extract `impl = X.class` value.
        let impl_name = trimmed
            .split("impl =")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.split('.').next())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if impl_name.is_empty() {
            continue;
        }
        // Extract the service name from the next non-comment
        // `extends XGrpc.YImplBase` clause. The class declaration
        // is typically `public class FooServiceImpl extends ...`
        // (the `public` modifier may be absent), so we look for
        // the `class` keyword anywhere on the line.
        let mut service_name: Option<String> = None;
        for look in lines.iter().skip(idx + 1).take(4) {
            let look_trim = look.trim();
            if look_trim.starts_with("//") || look_trim.is_empty() {
                continue;
            }
            // The line must contain a `class` keyword followed by
            // an `extends` clause. Strip modifiers and find the
            // class declaration.
            if let Some(class_pos) = look_trim.find("class ") {
                let after_class = &look_trim[class_pos + "class".len()..];
                if let Some(extends_start) = after_class.find("extends") {
                    let after = &after_class[extends_start + "extends".len()..];
                    if let Some(grpc_part) = after.split("Grpc.").nth(1) {
                        let name: String = grpc_part
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if !name.is_empty() {
                            // Strip the `ImplBase` suffix the
                            // codegen appends.
                            let stripped = name
                                .strip_suffix("ImplBase")
                                .map(|s| s.to_string())
                                .unwrap_or(name);
                            service_name = Some(stripped);
                            break;
                        }
                    }
                }
            }
            break;
        }
        let Some(service) = service_name else {
            continue;
        };
        out.push(RpcHandlerLink {
            rpc_service: service.clone(),
            rpc_method_scope: service,
            handler_function: SymbolKey {
                repo: repo_id.clone(),
                path: graph_path.to_string(),
                container: None,
                name: impl_name,
            },
            origin: RpcHandlerOrigin::JavaGrpcService,
            site_line: line_no,
        });
    }
}

fn detect_cpp(
    content: &str,
    graph_path: &str,
    repo_id: &crate::federation::repo_id::RepoId,
    out: &mut Vec<RpcHandlerLink>,
) {
    // `RegisterFooServer(...)` — the second arg is the
    // `FooService::Service` (we capture the service name from the
    // `Register<Name>Server` form).
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Register") {
            if let Some(suffix_pos) = rest.find("Server(") {
                let service = rest[..suffix_pos].trim().to_string();
                if !service.is_empty() {
                    out.push(RpcHandlerLink {
                        rpc_service: service.clone(),
                        rpc_method_scope: service.clone(),
                        handler_function: SymbolKey {
                            repo: repo_id.clone(),
                            path: graph_path.to_string(),
                            container: None,
                            name: format!("{}Service", service),
                        },
                        origin: RpcHandlerOrigin::CppRegister,
                        site_line: line_no,
                    });
                }
            }
        }
        if let Some(rest) = trimmed.strip_prefix("add_") {
            if let Some(suffix_pos) = rest.find("Servicer_to_server(") {
                let service = rest[..suffix_pos].trim().to_string();
                if !service.is_empty() {
                    out.push(RpcHandlerLink {
                        rpc_service: service.clone(),
                        rpc_method_scope: service.clone(),
                        handler_function: SymbolKey {
                            repo: repo_id.clone(),
                            path: graph_path.to_string(),
                            container: None,
                            name: format!("{}Service", service),
                        },
                        origin: RpcHandlerOrigin::CppRegister,
                        site_line: line_no,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::repo_id::RepoId;

    fn repo() -> RepoId {
        RepoId::new("test").unwrap()
    }

    #[test]
    fn go_register_links_handler() {
        let src = "\
package main
import pb \"orders\"
func main() {
    s := grpc.NewServer()
    pb.RegisterOrdersServer(s, &ordersImpl{})
}
";
        let links = detect_handler_links(src, "go", "main.go", &repo());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].rpc_service, "Orders");
        assert_eq!(links[0].handler_function.name, "ordersImpl");
        assert_eq!(links[0].origin, RpcHandlerOrigin::GoRegister);
    }

    #[test]
    fn python_servicer_links_handler() {
        let src = "\
from concurrent import futures
import orders_pb2_grpc
server = grpc.server(futures.ThreadPoolExecutor(max_workers=10))
orders_pb2_grpc.add_OrdersServicer_to_server(ordersImpl(), server)
";
        let links = detect_handler_links(src, "py", "server.py", &repo());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].rpc_service, "Orders");
        assert_eq!(links[0].handler_function.name, "ordersImpl");
        assert_eq!(links[0].origin, RpcHandlerOrigin::PythonServicer);
    }

    #[test]
    fn java_grpc_service_links_handler() {
        let src = "\
import net.devh.boot.grpc.server.service.GrpcService;

@GrpcService(impl = OrdersServiceImpl.class)
public class OrdersServiceImpl extends OrdersGrpc.OrdersImplBase {
}
";
        let links = detect_handler_links(src, "java", "OrdersServiceImpl.java", &repo());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].rpc_service, "Orders");
        assert_eq!(links[0].handler_function.name, "OrdersServiceImpl");
        assert_eq!(links[0].origin, RpcHandlerOrigin::JavaGrpcService);
    }

    #[test]
    fn cpp_register_links_handler() {
        let src = "\
int main() {
  RegisterOrdersServer(builder.AddListeningPort(\"...\", ...), &service);
}
";
        let links = detect_handler_links(src, "cc", "main.cc", &repo());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].rpc_service, "Orders");
        assert_eq!(links[0].handler_function.name, "OrdersService");
        assert_eq!(links[0].origin, RpcHandlerOrigin::CppRegister);
    }
}
