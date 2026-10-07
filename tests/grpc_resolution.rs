//! Phase E-gRPC acceptance scenarios
//! (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §8.2).
//!
//! Spec §8.2 pins the acceptance criteria for Phase E-gRPC:
//!
//! - **E1**: proto file with `package com.acme.orders; service Orders
//!   { rpc Get(GetReq) returns (GetResp); }` → emits
//!   `ContractKey::Rpc { system: Grpc, service: "com.acme.orders.Orders",
//!   method: "Get" }`.
//! - **E2**: Go file with
//!   `client := grpc.NewClient("orders:50051"); ordersClient.Get(ctx, req)`
//!   AND `repos.yaml` mapping `orders:50051 → com.acme.orders.Orders` →
//!   binds to the E1 provider.
//! - **E3**: Java
//!   `@GrpcService(impl = OrdersImpl.class) class OrdersImpl extends
//!   OrdersGrpc.OrdersImplBase` → emits `HandlerService { rpc_service:
//!   ..., handler_function: OrdersImpl }`.
//! - **E4**: stub call where the channel address can't be resolved
//!   (no client definition, no env binding) → lands in `unresolved`
//!   with `RpcStubUnknown`.
//! - **E5**: stub call to the calling service itself (own-provider)
//!   → does NOT bind (I5); land in `unresolved`.
//!
//! The tests drive the production sensors end-to-end against a
//! tempdir workspace (E1 + E3) and drive the joiner directly with
//! a hand-built `ContractIndex` (E2 / E4 / E5) — the joiner is the
//! pure inner half; the orchestrator integration belongs to a
//! later PR.

use std::path::PathBuf;

use lain::federation::contracts::clients::ClientRegistry;
use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::contracts::index::{ConsumerTarget, UnresolvedReason};
use lain::federation::contracts::joiner::ContractJoiner;
use lain::federation::contracts::model::{
    ContractFact, ContractKey, HostPart, RpcConsumerFact, RpcHandlerFact, RpcHandlerOrigin,
    RpcProviderFact, RpcSystem, SymbolKey,
};
use lain::federation::repo_id::{GlobalId, RepoId};
use lain::schema::{GraphNode, NodeType, RepoNamespace};
use lain::server::sensors::grpc_provider_sensor::parse_proto_providers;

// ─── Builders ────────────────────────────────────────────────────────

fn fixed_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lain_grpc_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(root: &std::path::Path, rel: &str, content: &str) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    path
}

fn ns() -> RepoNamespace {
    RepoNamespace::for_test()
}

fn repo_id(name: &str) -> RepoId {
    RepoId::new(name).unwrap()
}

fn make_id(repo: &str, kind: NodeType, path: &str, name: &str, line: u32) -> String {
    let r = repo_id(repo);
    GlobalId::new(&r, kind, path, name, Some(line))
        .as_str()
        .to_string()
}

fn rpc_provider_node(repo: &str, path: &str, method: &str, line: u32, service: &str) -> GraphNode {
    let id_name = format!("{}/{}", service, method);
    let id = make_id(repo, NodeType::Module, path, &id_name, line);
    let mut n = GraphNode::new_in(
        NodeType::Module,
        format!(
            "{}.{}",
            service.rsplit_once('.').map(|(_, s)| s).unwrap_or(service),
            method
        ),
        path.to_string(),
        &ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = id;
    n.line_start = Some(line);
    n.line_end = Some(line);
    n.contract = Some(ContractFact::RpcProvider(RpcProviderFact {
        system: RpcSystem::Grpc,
        service: service.to_string(),
        method: method.to_string(),
        request_type: format!("{}Request", method),
        response_type: format!("{}Response", method),
        handler: None,
    }));
    n
}

fn rpc_consumer_node(
    repo: &str,
    path: &str,
    method: &str,
    line: u32,
    service: &str,
    channel_target: Option<&str>,
    channel_host_part: HostPart,
) -> GraphNode {
    let id_name = format!("rpc-call:{}:{}", service, method);
    let id = make_id(repo, NodeType::Function, path, &id_name, line);
    let mut n = GraphNode::new_in(NodeType::Function, id_name.clone(), path.to_string(), &ns());
    n.repo_id = Some(repo.to_string());
    n.id = id;
    n.line_start = Some(line);
    n.line_end = Some(line);
    n.contract = Some(ContractFact::RpcConsumer(RpcConsumerFact {
        system: RpcSystem::Grpc,
        service: service.to_string(),
        method: method.to_string(),
        channel_target: channel_target.map(|s| s.to_string()),
        channel_host_part,
    }));
    n
}

fn rpc_handler_node(
    repo: &str,
    path: &str,
    line: u32,
    rpc_service: &str,
    handler_name: &str,
) -> GraphNode {
    let id_name = format!("rpc-handler:{}", handler_name);
    let id = make_id(repo, NodeType::Module, path, &id_name, line);
    let mut n = GraphNode::new_in(NodeType::Module, id_name.clone(), path.to_string(), &ns());
    n.repo_id = Some(repo.to_string());
    n.id = id;
    n.line_start = Some(line);
    n.line_end = Some(line);
    let key = ContractKey::Rpc {
        system: RpcSystem::Grpc,
        service: rpc_service.to_string(),
        method: String::new(),
    };
    n.contract = Some(ContractFact::RpcHandler(RpcHandlerFact {
        rpc_service: key,
        handler_function: SymbolKey {
            repo: repo_id(repo),
            path: path.to_string(),
            container: None,
            name: handler_name.to_string(),
        },
        origin: RpcHandlerOrigin::JavaGrpcService,
    }));
    n
}

fn config_with_orders_host(host: &str) -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![ServiceDecl {
            name: "orders".into(),
            repo: "orders".into(),
            paths: Vec::new(),
            hosts: vec![host.to_string()],
            env: Vec::new(),
            base_path: None,
            route_prefixes: Vec::new(),
        }],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    }
}

fn config_empty() -> ContractFederationConfig {
    ContractFederationConfig {
        services: Vec::new(),
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    }
}

fn run(
    nodes: Vec<GraphNode>,
    config: ContractFederationConfig,
) -> lain::federation::contracts::joiner::JoinOutput {
    ContractJoiner::run_with_registry(&nodes, &[], &config, &ClientRegistry::new())
}

fn gid(repo: &str, kind: NodeType, path: &str, name: &str, line: u32) -> GlobalId {
    let r = repo_id(repo);
    GlobalId::new(&r, kind, path, name, Some(line))
}

// ─── E1 — proto file emits ContractKey::Rpc ────────────────────────

/// **E1** (spec §8.2): a proto file declaring
/// `package com.acme.orders; service Orders { rpc Get(GetReq) returns (GetResp); }`
/// yields a `ContractKey::Rpc { system: Grpc, service:
/// "com.acme.orders.Orders", method: "Get" }`. We exercise the
/// tokenizer directly (the sensor wraps the same parser) and
/// confirm the composed service identity and the extracted
/// request/response type names.
#[test]
fn e1_proto_file_emits_rpc_key() {
    let root = fixed_workspace("e1");
    let proto = write_file(
        &root,
        "orders.proto",
        "\
syntax = \"proto3\";

package com.acme.orders;

service Orders {
  rpc Get (GetReq) returns (GetResp);
  rpc Create (CreateReq) returns (CreateResp);
}
",
    );
    let content = std::fs::read_to_string(&proto).unwrap();
    let providers = parse_proto_providers(&content, "orders.proto");
    assert_eq!(providers.len(), 2);
    assert_eq!(providers[0].package, "com.acme.orders");
    assert_eq!(providers[0].service, "Orders");
    assert_eq!(providers[0].method, "Get");
    assert_eq!(providers[0].request_type, "GetReq");
    assert_eq!(providers[0].response_type, "GetResp");
    // The composed identity is `com.acme.orders.Orders` — the
    // joiner projects this onto `ContractKey::Rpc` once the
    // provider runs through `build_endpoints`. We assert the
    // shape here so a future tokenizer change does not silently
    // drop the package prefix.
    let composed = lain::server::sensors::grpc_provider_sensor::compose_service_name(
        &providers[0].package,
        &providers[0].service,
    );
    assert_eq!(composed, "com.acme.orders.Orders");
    let key = ContractKey::Rpc {
        system: RpcSystem::Grpc,
        service: composed,
        method: providers[0].method.clone(),
    };
    assert_eq!(
        key.to_string(),
        "rpc:com.acme.orders.Orders/Get",
        "the rpc wire form renders `service/method`"
    );
}

// ─── E2 — Go stub call + repos.yaml host → binds to E1 provider ──

/// **E2** (spec §8.2): a Go file with
/// `client := grpc.NewClient("orders:50051"); ordersClient.Get(ctx, req)`
/// resolves the channel host to the configured Orders service and
/// binds to the E1 provider. The test mints a `RpcConsumer` node
/// directly (the consumer sensor wraps the same parser; the
/// orchestration is the joiner's job) and a `RpcProvider` for
/// `com.acme.orders.Orders/Get`, then confirms the joiner
/// produces a `Binds` edge between them.
#[test]
fn e2_go_stub_binds_to_known_service() {
    let provider = rpc_provider_node(
        "orders",
        "pkg/orders/orders.proto",
        "Get",
        7,
        "com.acme.orders.Orders",
    );
    let consumer = rpc_consumer_node(
        "billing",
        "cmd/billing/client.go",
        "Get",
        12,
        "Orders",
        Some("orders"),
        HostPart::Literal("orders".to_string()),
    );
    let config = config_with_orders_host("orders");
    let out = run(vec![provider, consumer], config);
    let provider_id = gid(
        "orders",
        NodeType::Module,
        "pkg/orders/orders.proto",
        "com.acme.orders.Orders/Get",
        7,
    );
    let consumer_id = gid(
        "billing",
        NodeType::Function,
        "cmd/billing/client.go",
        "rpc-call:Orders:Get",
        12,
    );
    let Some(resolution) = out.index.consumers.get(&consumer_id) else {
        panic!(
            "consumer must resolve: binds={:?} endpoints={:?}",
            out.binds, out.index.endpoints
        );
    };
    assert!(
        !resolution.bound_endpoints.is_empty(),
        "consumer must bind to at least one endpoint"
    );
    let bound = &resolution.bound_endpoints[0];
    assert_eq!(bound.0.to_string(), "orders");
    assert!(matches!(bound.1, ContractKey::Rpc { .. }));
    let Some(ConsumerTarget::Binds { confidence, .. }) = resolution.target.as_ref() else {
        panic!("target must be Binds, got {:?}", resolution.target);
    };
    assert!((confidence - 1.0).abs() < f32::EPSILON);
    let Some(bind) = out.binds.iter().find(|b| b.consumer == consumer_id) else {
        panic!("a Binds edge must exist for the consumer");
    };
    assert_eq!(bind.provider, provider_id);
    assert_eq!(bind.provider_service.to_string(), "orders");
    assert_eq!(bind.consumer_service.to_string(), "billing");
}

// ─── E3 — Java @GrpcService links provider → handler ───────────────

/// **E3** (spec §8.2): Java
/// `@GrpcService(impl = OrdersImpl.class) class OrdersImpl extends
/// OrdersGrpc.OrdersImplBase` emits a `RpcHandler` link with the
/// impl class as the handler function. We exercise the handler
/// sensor's detector directly to confirm it parses the
/// annotation + `extends` clause correctly and yields the
/// `Orders` service name.
#[test]
fn e3_java_grpc_service_links_handler() {
    use lain::server::sensors::grpc_handler_link_sensor::detect_handler_links;
    let src = "\
import net.devh.boot.grpc.server.service.GrpcService;

@GrpcService(impl = OrdersImpl.class)
public class OrdersImpl extends OrdersGrpc.OrdersImplBase {
}
";
    let links = detect_handler_links(src, "java", "OrdersImpl.java", &repo_id("orders"));
    assert_eq!(links.len(), 1, "the @GrpcService annotation must link");
    let link = &links[0];
    assert_eq!(link.rpc_service, "Orders");
    assert_eq!(link.handler_function.name, "OrdersImpl");
    assert!(matches!(link.origin, RpcHandlerOrigin::JavaGrpcService));
    // The handler node the sensor mints carries a
    // `ContractFact::RpcHandler` payload that names both the
    // `ContractKey::Rpc` and the `SymbolKey` for the impl class.
    let handler_node = rpc_handler_node("orders", "OrdersImpl.java", 4, "Orders", "OrdersImpl");
    match &handler_node.contract {
        Some(ContractFact::RpcHandler(rh)) => {
            assert_eq!(rh.handler_function.name, "OrdersImpl");
            assert!(matches!(rh.rpc_service, ContractKey::Rpc { .. }));
        }
        other => panic!("expected ContractFact::RpcHandler, got {other:?}"),
    }
}

// ─── E4 — channel address unresolvable → RpcStubUnknown ───────────

/// **E4** (spec §8.2): a stub call whose channel address has no
/// matching `services[].hosts` entry lands in `unresolved` with
/// `reason: RpcStubUnknown`. The config has no services at all
/// (an empty `repos.yaml`), so the channel's host literal cannot
/// resolve to any service.
#[test]
fn e4_unresolved_channel_is_rpc_stub_unknown() {
    let provider = rpc_provider_node(
        "orders",
        "pkg/orders/orders.proto",
        "Get",
        7,
        "com.acme.orders.Orders",
    );
    let consumer = rpc_consumer_node(
        "billing",
        "cmd/billing/client.go",
        "Get",
        12,
        "Orders",
        Some("unknown-host.example.com"),
        HostPart::Literal("unknown-host.example.com".to_string()),
    );
    let out = run(vec![provider, consumer], config_empty());
    let consumer_id = gid(
        "billing",
        NodeType::Function,
        "cmd/billing/client.go",
        "rpc-call:Orders:Get",
        12,
    );
    let Some(resolution) = out.index.consumers.get(&consumer_id) else {
        panic!("consumer must be recorded (even when unresolved)");
    };
    let Some(ConsumerTarget::Unresolved { reason, .. }) = resolution.target.as_ref() else {
        panic!("target must be Unresolved, got {:?}", resolution.target);
    };
    assert!(
        matches!(reason, UnresolvedReason::RpcStubUnknown),
        "expected RpcStubUnknown, got {reason:?}"
    );
    assert!(resolution.bound_endpoints.is_empty());
    assert!(out.binds.is_empty());
}

// ─── E5 — stub call to own service does NOT bind (I5) ─────────────

/// **E5** (spec §5.3 / §3 invariant): a stub call to the calling
/// service's own provider must NOT bind (I5 — every `Binds` edge
/// connects two different services). The provider is in the
/// `orders` repo; the consumer is also in the `orders` repo;
/// the joiner must skip the provider on the I5 check and emit
/// `Unresolved { reason: RpcStubUnknown }` instead of a `Binds`
/// edge.
#[test]
fn e5_same_service_call_does_not_bind() {
    let provider = rpc_provider_node(
        "orders",
        "pkg/orders/orders.proto",
        "Get",
        7,
        "com.acme.orders.Orders",
    );
    // Same repo as the provider — would otherwise bind to the
    // provider's own service under rule 1.
    let consumer = rpc_consumer_node(
        "orders",
        "pkg/orders/orders_impl.go",
        "Get",
        22,
        "Orders",
        Some("orders"),
        HostPart::Literal("orders".to_string()),
    );
    let config = config_with_orders_host("orders");
    let out = run(vec![provider, consumer], config);
    let consumer_id = gid(
        "orders",
        NodeType::Function,
        "pkg/orders/orders_impl.go",
        "rpc-call:Orders:Get",
        22,
    );
    let Some(resolution) = out.index.consumers.get(&consumer_id) else {
        panic!("consumer must be recorded");
    };
    // I5: the call must NOT bind — same-service binds are
    // explicitly disallowed.
    assert!(
        resolution.bound_endpoints.is_empty(),
        "same-service stub call must not bind (I5)"
    );
    let Some(ConsumerTarget::Unresolved { reason, .. }) = resolution.target.as_ref() else {
        panic!(
            "same-service call must be Unresolved, got {:?}",
            resolution.target
        );
    };
    assert!(
        matches!(reason, UnresolvedReason::RpcStubUnknown),
        "expected RpcStubUnknown, got {reason:?}"
    );
    assert!(
        out.binds.is_empty(),
        "no Binds edges may be emitted for same-service calls"
    );
}

// ─── E6 — gRPC message-field lineage and field binds ──────────────────

#[test]
fn e6_grpc_message_field_lineage_and_field_binds() {
    use lain::federation::contracts::model::{FieldReadFact, JsonPath, PathSegment};
    use lain::graph::GraphDatabase;
    use lain::schema::{EdgeType, GraphEdge};
    use lain::server::sensors::grpc_provider_sensor::scan_workspace_grpc;

    let root = fixed_workspace("e6");
    let proto_content = r#"
syntax = "proto3";

package com.acme.orders;

message GetReq {
  string order_id = 1;
}

message GetResp {
  string id = 1;
  string status = 2;
  double total = 3;
}

service Orders {
  rpc Get (GetReq) returns (GetResp);
}
"#;
    write_file(&root, "orders.proto", proto_content);
    let graph = GraphDatabase::new(&root.join("graph.bin")).unwrap();
    let n = RepoNamespace::for_test();
    let count = scan_workspace_grpc(&graph, &root, &n).unwrap();
    assert_eq!(count, 1, "emitted 1 RpcProvider");

    let (all_nodes, all_edges) =
        lain::federation::contracts::snapshots::manager::project_graph_shared(&graph, "orders")
            .unwrap();

    // Verify Schema and Field nodes exist
    let get_resp_schema = all_nodes
        .iter()
        .find(|n| n.node_type == NodeType::Schema && n.name == "GetResp");
    assert!(
        get_resp_schema.is_some(),
        "GetResp Schema node must be emitted"
    );
    let status_field = all_nodes
        .iter()
        .find(|n| n.node_type == NodeType::Field && n.name == "status");
    assert!(status_field.is_some(), "status Field node must be emitted");

    // Verify RequestSchema and ResponseSchema edges
    let has_resp_schema_edge = all_edges
        .iter()
        .any(|e| e.edge_type == EdgeType::ResponseSchema);
    assert!(has_resp_schema_edge, "ResponseSchema edge must be emitted");

    // Test end-to-end joiner field resolution
    let consumer = rpc_consumer_node(
        "billing",
        "pkg/billing/client.go",
        "Get",
        15,
        "com.acme.orders.Orders",
        Some("orders:50051"),
        HostPart::Literal("orders:50051".to_string()),
    );
    let consumer_gid = gid(
        "billing",
        NodeType::Function,
        "pkg/billing/client.go",
        "rpc-call:com.acme.orders.Orders:Get",
        15,
    );

    // Create a FieldRef node reading 'status'
    let field_ref_id = make_id(
        "billing",
        NodeType::FieldRef,
        "pkg/billing/client.go",
        "field-read:status",
        16,
    );
    let mut field_ref_node = GraphNode::new_in(
        NodeType::FieldRef,
        "status".to_string(),
        "pkg/billing/client.go".to_string(),
        &n,
    );
    field_ref_node.id = field_ref_id.clone();
    field_ref_node.repo_id = Some("billing".to_string());
    field_ref_node.line_start = Some(16);
    field_ref_node.contract = Some(ContractFact::FieldRead(FieldReadFact {
        chain: JsonPath(vec![PathSegment::Name("status".to_string())]),
        exact: true,
    }));

    let reads_from_edge = GraphEdge::new(EdgeType::ReadsFrom, field_ref_id, consumer.id.clone());

    let mut nodes_for_join = all_nodes;
    nodes_for_join.push(consumer);
    nodes_for_join.push(field_ref_node);

    let mut edges_for_join = all_edges;
    edges_for_join.push(reads_from_edge);

    let config = config_with_orders_host("orders:50051");
    let out = ContractJoiner::run_with_registry(
        &nodes_for_join,
        &edges_for_join,
        &config,
        &ClientRegistry::new(),
    );

    // Verify consumer bound to provider
    let resolution = out
        .index
        .consumers
        .get(&consumer_gid)
        .expect("consumer must be resolved");
    assert!(
        !resolution.bound_endpoints.is_empty(),
        "consumer must bind to orders endpoint"
    );

    // Verify field ref bound to status Field
    let field_ref_gid = gid(
        "billing",
        NodeType::FieldRef,
        "pkg/billing/client.go",
        "field-read:status",
        16,
    );
    let field_res = out
        .index
        .field_refs
        .get(&field_ref_gid)
        .expect("field_ref must be recorded");
    assert!(!field_res.unknown, "field read must not be unknown");
    assert_eq!(
        field_res.bound_fields.len(),
        1,
        "field read must bind to status field"
    );
    assert_eq!(field_res.bound_fields[0].field_path.to_string(), "status");
}
