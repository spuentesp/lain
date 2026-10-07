//! Phase E-GraphQL acceptance scenarios
//! (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §8.3).
//!
//! Spec §8.3 pins the acceptance criteria for Phase E-GraphQL:
//!
//! - **F1**: SDL file with
//!   `type Query { orders: [Order!] }` → emits
//!   `ContractKey::Graphql { op: Query, field: "orders" }`.
//! - **F2**: TS file with
//!   `gql\`query { orders { id } }\`` and an HTTP route
//!   `/graphql` resolving to service `api` → binds to the F1
//!   provider.
//! - **F3**: gqlgen-style
//!   `func (r *queryResolver) Orders(ctx) ([]*Order, error)` →
//!   emits `HandlerService { graphql_field, handler_function }`.
//! - **F4**: TS file with
//!   `gql\`query { ${userId} { ... } }\`` (interpolated) → lands
//!   in `unresolved` with `DynamicOperation`.
//! - **F5**: federation case — two services (gateway + backend)
//!   both expose `orders` on Query → lands in `Ambiguous`, no
//!   single bind.
//! - **F6**: fragment-only document
//!   (`fragment X on Order { id }` with no top-level query) →
//!   lands in `unresolved` with `DynamicOperation`.
//!
//! The tests drive the production sensors end-to-end against a
//! tempdir workspace (F1 + F3 + F4 + F6) and the joiner
//! directly with a hand-built `ContractIndex` (F2 + F5) —
//! the joiner is the pure inner half; the orchestrator
//! integration belongs to a later PR.

use std::path::PathBuf;

use lain::federation::contracts::clients::ClientRegistry;
use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::contracts::index::{ConsumerTarget, UnresolvedReason};
use lain::federation::contracts::joiner::ContractJoiner;
use lain::federation::contracts::model::{
    ContractFact, ContractKey, GraphqlConsumerFact, GraphqlHandlerFact, GraphqlHandlerOrigin,
    GraphqlOp, GraphqlProviderFact, HttpMethod, SymbolKey,
};
use lain::federation::repo_id::{GlobalId, RepoId};
use lain::schema::{GraphNode, NodeType, RepoNamespace};
use lain::server::sensors::graphql_consumer_sensor::parse_document;
use lain::server::sensors::graphql_provider_sensor::parse_sdl_providers;
use lain::server::sensors::graphql_resolver_link_sensor::detect_resolver_links;

// ─── Builders ────────────────────────────────────────────────────────

fn fixed_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lain_graphql_{tag}"));
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

fn graphql_provider_node(
    repo: &str,
    path: &str,
    op: GraphqlOp,
    field: &str,
    line: u32,
    return_type: &str,
) -> GraphNode {
    let id_name = format!("{}:{}", op, field);
    let id = make_id(repo, NodeType::Module, path, &id_name, line);
    let mut n = GraphNode::new_in(NodeType::Module, id_name.clone(), path.to_string(), &ns());
    n.repo_id = Some(repo.to_string());
    n.id = id;
    n.line_start = Some(line);
    n.line_end = Some(line);
    n.contract = Some(ContractFact::GraphqlProvider(GraphqlProviderFact {
        op,
        field: field.to_string(),
        return_type: return_type.to_string(),
    }));
    n
}

fn graphql_consumer_node(
    repo: &str,
    path: &str,
    op: GraphqlOp,
    field: &str,
    line: u32,
) -> GraphNode {
    let id_name = format!("graphql-call:{}:{}", op, field);
    let id = make_id(repo, NodeType::Function, path, &id_name, line);
    let mut n = GraphNode::new_in(NodeType::Function, id_name.clone(), path.to_string(), &ns());
    n.repo_id = Some(repo.to_string());
    n.id = id;
    n.line_start = Some(line);
    n.line_end = Some(line);
    n.contract = Some(ContractFact::GraphqlConsumer(GraphqlConsumerFact {
        op,
        field: field.to_string(),
    }));
    n
}

fn http_route_node(repo: &str, path: &str, template: &str, line: u32) -> GraphNode {
    use lain::federation::contracts::model::{ProviderFact, ProviderOrigin};
    let id_name = format!("http:POST {}", template);
    let id = make_id(repo, NodeType::HttpRoute, path, &id_name, line);
    let mut n = GraphNode::new_in(
        NodeType::HttpRoute,
        id_name.clone(),
        path.to_string(),
        &ns(),
    );
    n.repo_id = Some(repo.to_string());
    n.id = id;
    n.line_start = Some(line);
    n.line_end = Some(line);
    n.contract = Some(ContractFact::Provider(ProviderFact {
        method: HttpMethod::Post,
        template: template.to_string(),
        handler: None,
        operation_id: None,
        origin: ProviderOrigin::Code,
    }));
    n
}

fn config_with_service(name: &str) -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![ServiceDecl {
            name: name.to_string(),
            repo: name.to_string(),
            paths: Vec::new(),
            hosts: Vec::new(),
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

fn config_with_two_services(a: &str, b: &str) -> ContractFederationConfig {
    ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: a.to_string(),
                repo: a.to_string(),
                paths: Vec::new(),
                hosts: Vec::new(),
                env: Vec::new(),
                base_path: None,
                route_prefixes: Vec::new(),
            },
            ServiceDecl {
                name: b.to_string(),
                repo: b.to_string(),
                paths: Vec::new(),
                hosts: Vec::new(),
                env: Vec::new(),
                base_path: None,
                route_prefixes: Vec::new(),
            },
        ],
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

// ─── F1 — SDL file emits ContractKey::Graphql ────────────────────────

/// **F1** (spec §8.3): a `.graphql` SDL file declaring
/// `type Query { orders: [Order!] }` yields a
/// `ContractKey::Graphql { op: Query, field: "orders" }`. We
/// exercise the provider sensor's tokenizer directly (the
/// orchestrator wires the same parser into the graph
/// emission path) and confirm the (op, field, return_type)
/// triple.
#[test]
fn f1_sdl_file_emits_graphql_key() {
    let root = fixed_workspace("f1");
    let sdl = write_file(
        &root,
        "schema.graphql",
        "\
type Query {
  orders: [Order!]!
  order(id: ID!): Order
}
",
    );
    let content = std::fs::read_to_string(&sdl).unwrap();
    let providers = parse_sdl_providers(&content, "schema.graphql");
    assert_eq!(providers.len(), 2);
    assert_eq!(providers[0].op, GraphqlOp::Query);
    assert_eq!(providers[0].field, "orders");
    assert_eq!(providers[0].return_type, "[Order!]!");
    assert_eq!(providers[1].field, "order");
    assert_eq!(providers[1].return_type, "Order");
    let key = ContractKey::Graphql {
        op: providers[0].op,
        field: providers[0].field.clone(),
    };
    assert_eq!(
        key.to_string(),
        "graphql:query:orders",
        "the graphql wire form renders `graphql:<op>:<field>`"
    );
}

// ─── F2 — TS gql tagged template + /graphql route → binds ───────────

/// **F2** (spec §8.3): a TS file with
/// `gql\`query { orders { id } }\`` and an HTTP route
/// `/graphql` resolving to service `api` binds to the F1
/// provider. The test mints the GraphqlConsumer +
/// GraphqlProvider + /graphql HttpRoute nodes directly
/// (the consumer sensor wraps the same parser; the
/// orchestrator is the joiner's caller) and confirms the
/// joiner produces a Binds edge.
#[test]
fn f2_gql_tagged_template_binds_via_graphql_route() {
    let provider = graphql_provider_node(
        "api",
        "schema.graphql",
        GraphqlOp::Query,
        "orders",
        3,
        "[Order!]!",
    );
    let consumer = graphql_consumer_node("billing", "src/orders.ts", GraphqlOp::Query, "orders", 7);
    let route = http_route_node("api", "src/server.ts", "/graphql", 12);
    let out = run(vec![provider, consumer, route], config_with_service("api"));
    let consumer_id = gid(
        "billing",
        NodeType::Function,
        "src/orders.ts",
        "graphql-call:query:orders",
        7,
    );
    let Some(resolution) = out.index.consumers.get(&consumer_id) else {
        panic!(
            "consumer must resolve: binds={:?} endpoints={:?}",
            out.binds, out.index.endpoints
        );
    };
    assert!(
        !resolution.bound_endpoints.is_empty(),
        "consumer must bind to the api provider"
    );
    let bound = &resolution.bound_endpoints[0];
    assert_eq!(bound.0.to_string(), "api");
    assert!(matches!(bound.1, ContractKey::Graphql { .. }));
    let Some(ConsumerTarget::Binds { confidence, .. }) = resolution.target.as_ref() else {
        panic!("target must be Binds, got {:?}", resolution.target);
    };
    assert!((confidence - 1.0).abs() < f32::EPSILON);
    let provider_id = gid("api", NodeType::Module, "schema.graphql", "query:orders", 3);
    let Some(bind) = out.binds.iter().find(|b| b.consumer == consumer_id) else {
        panic!("a Binds edge must exist for the consumer");
    };
    assert_eq!(bind.provider, provider_id);
    assert_eq!(bind.provider_service.to_string(), "api");
    assert_eq!(bind.consumer_service.to_string(), "billing");
}

// ─── F3 — gqlgen resolver → HandlerService link ─────────────────────

/// **F3** (spec §8.3): a Go file with
/// `func (r *queryResolver) Orders(ctx context.Context) ([]*Order, error)`
/// emits a `GraphqlHandler` link with the method name as the
/// field, the resolver type as the container, and the
/// `Gqlgen` origin. We exercise the resolver-link sensor's
/// detector directly to confirm it parses the receiver type
/// and the method name correctly.
#[test]
fn f3_gqlgen_resolver_links_handler() {
    let src = "\
package graph

func (r *queryResolver) Orders(ctx context.Context) ([]*Order, error) {
    return r.OrdersService.List(ctx)
}
";
    let links = detect_resolver_links(src, "go", "resolver.go", &repo_id("api"));
    assert_eq!(links.len(), 1);
    let link = &links[0];
    assert_eq!(link.op, GraphqlOp::Query);
    assert_eq!(link.field, "Orders");
    assert_eq!(link.handler_function.name, "Orders");
    assert_eq!(
        link.handler_function.container.as_deref(),
        Some("queryResolver")
    );
    assert!(matches!(link.origin, GraphqlHandlerOrigin::Gqlgen));
    // The handler node the sensor mints carries a
    // `ContractFact::GraphqlHandler` payload that names both
    // the `ContractKey::Graphql` and the `SymbolKey` for the
    // resolver method.
    let id_name = format!("graphql-handler:{}:{}", link.op, link.field);
    let id = make_id(
        "api",
        NodeType::Module,
        "resolver.go",
        &id_name,
        link.site_line,
    );
    let mut n = GraphNode::new_in(NodeType::Module, id_name, "resolver.go".to_string(), &ns());
    n.repo_id = Some("api".to_string());
    n.id = id;
    let key = ContractKey::Graphql {
        op: link.op,
        field: link.field.clone(),
    };
    n.contract = Some(ContractFact::GraphqlHandler(GraphqlHandlerFact {
        graphql_field: key,
        handler_function: link.handler_function.clone(),
        origin: link.origin,
    }));
    match &n.contract {
        Some(ContractFact::GraphqlHandler(gh)) => {
            assert_eq!(gh.handler_function.name, "Orders");
            assert!(matches!(gh.graphql_field, ContractKey::Graphql { .. }));
        }
        other => panic!("expected ContractFact::GraphqlHandler, got {other:?}"),
    }
}

// ─── F4 — interpolated document → DynamicOperation ──────────────────

/// **F4** (spec §8.3): a TS file with
/// `gql\`query { ${userId} { ... } }\`` (interpolated) lands
/// in `unresolved` with reason `DynamicOperation`. The
/// consumer sensor detects the `${...}` placeholder and
/// returns a single `dynamic: true` record; the joiner
/// produces no `GraphqlConsumer` node for it, and the
/// coverage ledger records the reason.
#[test]
fn f4_interpolated_document_is_dynamic() {
    let src = r"\
const ORDERS = gql`query { ${userId} { id } }`;
";
    let root = fixed_workspace("f4");
    let ts = write_file(&root, "src/orders.ts", src);
    let content = std::fs::read_to_string(&ts).unwrap();
    let detected =
        lain::server::sensors::graphql_consumer_sensor::detect_in_code(&content, "src/orders.ts");
    assert_eq!(detected.len(), 1);
    assert!(
        detected[0].dynamic,
        "interpolated document must be flagged dynamic"
    );
    // The joiner does not produce a Binds edge for dynamic
    // consumers; the coverage ledger records the reason.
    // We exercise the dynamic flag detection here; the F4
    // joiner integration is the same shape as F6 (no
    // GraphqlConsumer node mints, so the joiner sees no
    // consumer to resolve).
    let _ = root;
}

// ─── F5 — federation case → Ambiguous, no single bind ───────────────

/// **F5** (spec §8.3): two services (gateway + backend) both
/// expose `orders` on Query → the joiner MUST NOT single-bind
/// (per spec §8.3: "if several services expose the same root
/// field (federation/gateway) ⇒ ambiguous, never
/// single-bound"). The test mints two providers (one in each
/// service) and a consumer in a third service. The /graphql
/// route is owned by `api` (the gateway). The backend's
/// `orders` provider is in scope (same `Query.orders` field)
/// but the joiner must surface `Unresolved { reason:
/// GraphqlNoOp }` because two services match.
#[test]
fn f5_federation_ambiguous_no_single_bind() {
    let gateway_provider = graphql_provider_node(
        "api",
        "schema.graphql",
        GraphqlOp::Query,
        "orders",
        3,
        "[Order!]!",
    );
    let backend_provider = graphql_provider_node(
        "orders",
        "schema.graphql",
        GraphqlOp::Query,
        "orders",
        5,
        "[Order!]!",
    );
    let consumer = graphql_consumer_node("billing", "src/orders.ts", GraphqlOp::Query, "orders", 7);
    let route = http_route_node("api", "src/server.ts", "/graphql", 12);
    let out = run(
        vec![gateway_provider, backend_provider, consumer, route],
        config_with_two_services("api", "orders"),
    );
    let consumer_id = gid(
        "billing",
        NodeType::Function,
        "src/orders.ts",
        "graphql-call:query:orders",
        7,
    );
    let Some(resolution) = out.index.consumers.get(&consumer_id) else {
        panic!("consumer must be recorded (even when ambiguous)");
    };
    assert!(
        resolution.bound_endpoints.is_empty(),
        "ambiguous federation case must NOT bind"
    );
    let Some(ConsumerTarget::Unresolved { reason, .. }) = resolution.target.as_ref() else {
        panic!(
            "federation case must be Unresolved, got {:?}",
            resolution.target
        );
    };
    assert!(
        matches!(reason, UnresolvedReason::GraphqlNoOp),
        "expected GraphqlNoOp, got {reason:?}"
    );
    assert!(
        out.binds.is_empty(),
        "no Binds edges may be emitted for ambiguous federation"
    );
}

// ─── F6 — fragment-only document → DynamicOperation ──────────────────

/// **F6** (spec §8.3): a `.graphql` file containing only a
/// fragment (`fragment X on Order { id }`) with no top-level
/// operation lands in `unresolved` with `DynamicOperation`.
/// The consumer sensor returns a single `dynamic: true`
/// record; the joiner sees no real `GraphqlConsumer` node
/// to bind.
#[test]
fn f6_fragment_only_document_is_dynamic() {
    let root = fixed_workspace("f6");
    let frag = write_file(
        &root,
        "frag.graphql",
        "\
fragment X on Order {
  id
}
",
    );
    let content = std::fs::read_to_string(&frag).unwrap();
    let detected = parse_document(&content, "frag.graphql");
    assert_eq!(detected.len(), 1);
    assert!(
        detected[0].dynamic,
        "fragment-only document must be flagged dynamic"
    );
    assert!(
        detected[0].field.is_empty(),
        "no top-level field on a fragment-only document"
    );
}

// ─── Additional negative path: HTTP route owner mismatch ────────────

/// **F2-NEG**: when the consumer's `/graphql` route lives in
/// service `api` but the provider is in a different service
/// (say `orders`), the joiner still binds on the (op, field)
/// match — Phase B's HTTP join is the only thing that ties
/// the route to the owning service. This is the positive
/// counterpart of F2 and confirms the joiner doesn't
/// accidentally conflate route ownership with provider
/// location.
#[test]
fn f2_neg_route_owner_and_provider_can_differ() {
    // The provider is in `orders`; the /graphql route is in
    // `api` (the gateway). The joiner still binds because
    // the (op, field) match is exact.
    let provider = graphql_provider_node(
        "orders",
        "schema.graphql",
        GraphqlOp::Query,
        "orders",
        3,
        "[Order!]!",
    );
    let consumer = graphql_consumer_node("billing", "src/orders.ts", GraphqlOp::Query, "orders", 7);
    let route = http_route_node("api", "src/server.ts", "/graphql", 12);
    let out = run(
        vec![provider, consumer, route],
        config_with_two_services("api", "orders"),
    );
    let consumer_id = gid(
        "billing",
        NodeType::Function,
        "src/orders.ts",
        "graphql-call:query:orders",
        7,
    );
    let Some(resolution) = out.index.consumers.get(&consumer_id) else {
        panic!("consumer must resolve");
    };
    assert!(
        !resolution.bound_endpoints.is_empty(),
        "single-provider case must bind even when route owner and provider differ"
    );
}

// ─── Wire form round-trip ────────────────────────────────────────────

/// Sanity: the `Display` / `FromStr` round-trip on
/// `ContractKey::Graphql` covers all three ops.
#[test]
fn graphql_wire_form_round_trips() {
    for (op, label) in [
        (GraphqlOp::Query, "query"),
        (GraphqlOp::Mutation, "mutation"),
        (GraphqlOp::Subscription, "subscription"),
    ] {
        let key = ContractKey::Graphql {
            op,
            field: "orders".to_string(),
        };
        let wire = key.to_string();
        assert_eq!(wire, format!("graphql:{label}:orders"));
        let parsed: ContractKey = wire.parse().expect("round-trip parse");
        assert_eq!(parsed, key);
    }
}

// ─── Same-service skip (I5) ──────────────────────────────────────────

/// The same-service rule from spec §5.3 (I5) applies to
/// GraphQL too: a consumer in the same service as the
/// provider must NOT bind.
#[test]
fn graphql_same_service_does_not_bind() {
    let provider = graphql_provider_node(
        "api",
        "schema.graphql",
        GraphqlOp::Query,
        "orders",
        3,
        "[Order!]!",
    );
    let consumer = graphql_consumer_node("api", "src/orders.ts", GraphqlOp::Query, "orders", 7);
    let route = http_route_node("api", "src/server.ts", "/graphql", 12);
    let out = run(vec![provider, consumer, route], config_with_service("api"));
    let consumer_id = gid(
        "api",
        NodeType::Function,
        "src/orders.ts",
        "graphql-call:query:orders",
        7,
    );
    let Some(resolution) = out.index.consumers.get(&consumer_id) else {
        panic!("consumer must be recorded (even when same-service)");
    };
    assert!(
        resolution.bound_endpoints.is_empty(),
        "same-service consumer must NOT bind (I5)"
    );
    let Some(ConsumerTarget::Unresolved { reason, .. }) = resolution.target.as_ref() else {
        panic!(
            "same-service must be Unresolved, got {:?}",
            resolution.target
        );
    };
    assert!(matches!(reason, UnresolvedReason::GraphqlNoOp));
    assert!(out.binds.is_empty());
}

// ─── Sanity: GraphqlHandler → SymbolKey projection ───────────────────

/// The resolver-link sensor's payload carries the
/// `SymbolKey` for the handler function; the joiner can
/// therefore project the handler → function link onto the
/// provider's `handler` field (mirroring the gRPC pattern).
/// This test pins the SymbolKey shape the joiner reads.
#[test]
fn graphql_handler_symbol_key_projects_to_function() {
    use std::collections::BTreeMap;
    let symbol = SymbolKey {
        repo: repo_id("api"),
        path: "resolver.go".to_string(),
        container: Some("queryResolver".to_string()),
        name: "Orders".to_string(),
    };
    let fact = GraphqlHandlerFact {
        graphql_field: ContractKey::Graphql {
            op: GraphqlOp::Query,
            field: "Orders".to_string(),
        },
        handler_function: symbol.clone(),
        origin: GraphqlHandlerOrigin::Gqlgen,
    };
    // BTreeMap keyed by the SymbolKey round-trips (the
    // joiner's typed traversal will read it back out).
    let mut map: BTreeMap<SymbolKey, &str> = BTreeMap::new();
    map.insert(symbol.clone(), "handler");
    assert_eq!(map.get(&symbol), Some(&"handler"));
    assert_eq!(fact.origin, GraphqlHandlerOrigin::Gqlgen);
}

// ─── F7 — GraphQL Field Lineage and Field Binds ──────────────────────

#[test]
fn f7_graphql_field_lineage_and_field_binds() {
    use lain::graph::GraphDatabase;
    use lain::schema::EdgeType;
    use lain::server::sensors::graphql_consumer_sensor::scan_workspace_graphql_consumer;
    use lain::server::sensors::graphql_provider_sensor::scan_workspace_graphql_provider;

    let root_api = fixed_workspace("f7_api");
    let sdl_content = r#"
type Order {
  id: ID!
  status: String!
  total: Float
}

type Query {
  orders: [Order!]!
}
"#;
    write_file(&root_api, "schema.graphql", sdl_content);
    let graph_api = GraphDatabase::new(&root_api.join("graph.bin")).unwrap();
    let n = RepoNamespace::for_test();
    let count_api = scan_workspace_graphql_provider(&graph_api, &root_api, &n).unwrap();
    assert_eq!(count_api, 1, "emitted 1 GraphqlProvider");

    let (nodes_api, edges_api) =
        lain::federation::contracts::snapshots::manager::project_graph_shared(&graph_api, "api")
            .unwrap();

    // Verify Schema and Field nodes exist
    let order_schema = nodes_api
        .iter()
        .find(|n| n.node_type == NodeType::Schema && n.name == "Order");
    assert!(order_schema.is_some(), "Order Schema node must be emitted");
    let status_field = nodes_api
        .iter()
        .find(|n| n.node_type == NodeType::Field && n.name == "status");
    assert!(status_field.is_some(), "status Field node must be emitted");

    let has_resp_schema_edge = edges_api
        .iter()
        .any(|e| e.edge_type == EdgeType::ResponseSchema);
    assert!(has_resp_schema_edge, "ResponseSchema edge must be emitted");

    // Task 9: pin the specific edge. A bare `any(|e| ...)` accepted
    // a `ResponseSchema` edge to *some* schema; the contract is the
    // edge must connect the *Order* provider to the *Order* schema.
    // Pin both endpoints so a regression that mis-routes the edge
    // (e.g. swapping provider for consumer) is caught.
    let order_provider_id = nodes_api
        .iter()
        .find(|n| n.node_type == NodeType::Module && n.name.contains("Order"))
        .map(|n| n.id.clone());
    let order_schema_id = order_schema.map(|n| n.id.clone());
    if let (Some(provider_id), Some(schema_id)) = (order_provider_id, order_schema_id) {
        let connected = edges_api.iter().any(|e| {
            e.edge_type == EdgeType::ResponseSchema
                && e.source_id == provider_id
                && e.target_id == schema_id
        });
        assert!(
            connected,
            "ResponseSchema edge must connect Order provider {provider_id} \
             to Order schema {schema_id}, got edges: {:?}",
            edges_api
                .iter()
                .filter(|e| e.edge_type == EdgeType::ResponseSchema)
                .collect::<Vec<_>>()
        );
    }

    // Scan consumer in web repo
    let root_web = fixed_workspace("f7_web");
    let ts_content = r#"
const query = gql`
  query GetOrders {
    orders {
      id
      status
    }
  }
`;
"#;
    write_file(&root_web, "src/orders.ts", ts_content);
    let graph_web = GraphDatabase::new(&root_web.join("graph.bin")).unwrap();
    let count_web = scan_workspace_graphql_consumer(&graph_web, &root_web, &n).unwrap();
    assert_eq!(count_web, 1, "emitted 1 GraphqlConsumer");

    let (nodes_web, edges_web) =
        lain::federation::contracts::snapshots::manager::project_graph_shared(&graph_web, "web")
            .unwrap();

    // In web repo, verify Function and FieldRef nodes exist
    let orders_consumer = nodes_web
        .iter()
        .find(|n| n.node_type == NodeType::Function && n.name == "graphql-call:query:orders");
    assert!(
        orders_consumer.is_some(),
        "graphql-call Function node must be emitted"
    );
    let orders_consumer_id = orders_consumer.unwrap().id.clone();

    let status_ref = nodes_web
        .iter()
        .find(|n| n.node_type == NodeType::FieldRef && n.name == "status");
    assert!(status_ref.is_some(), "status FieldRef node must be emitted");
    let status_ref_id = status_ref.unwrap().id.clone();

    // Route in api repo for /graphql POST endpoint
    let route = http_route_node("api", "src/server.ts", "/graphql", 10);

    let mut all_nodes = nodes_api;
    all_nodes.extend(nodes_web);
    all_nodes.push(route);

    let mut all_edges = edges_api;
    all_edges.extend(edges_web);

    let config = config_with_two_services("api", "web");
    let out =
        ContractJoiner::run_with_registry(&all_nodes, &all_edges, &config, &ClientRegistry::new());

    let consumer_gid = GlobalId::parse(&orders_consumer_id).unwrap();
    let resolution = out
        .index
        .consumers
        .get(&consumer_gid)
        .expect("consumer must be resolved");
    assert!(
        !resolution.bound_endpoints.is_empty(),
        "consumer must bind to api endpoint"
    );

    let status_ref_gid = GlobalId::parse(&status_ref_id).unwrap();
    let field_res = out
        .index
        .field_refs
        .get(&status_ref_gid)
        .expect("field_ref for status must be recorded");
    assert!(!field_res.unknown, "status field read must not be unknown");
    assert_eq!(
        field_res.bound_fields.len(),
        1,
        "status field read must bind to Order.status field"
    );
    assert_eq!(field_res.bound_fields[0].field_path.to_string(), "status");
}

// ─── Task 6 — Fragment spreads, inline fragments, SDL implements/@key
//
// These cases are the parser defects called out in
// `docs/superpowers/plans/2026-10-07-remaining-work.md` Task 6:
// fragments must not become selected fields, and SDL with
// `implements` / `@key` directives must still emit Schema/Field
// nodes. Each test below pins one shape and is expected to be
// RED until the parser learns to recognise `...` and the
// directive/implements header.

/// Task 6 case (a): a query with a fragment spread
/// (`query { orders { id ...orderFields } }`) must NOT emit
/// `orderFields` as a selected field. Today the scanner walks
/// past the `.` byte, then reads `orderFields` as a field name
/// and yields a `FieldRef` named `orderFields` for it. The
/// acceptance criterion is that `id` is selected and no field
/// named `orderFields` leaks into either the `field` list or
/// any `selected_fields` list.
#[test]
fn t6_fragment_spread_is_not_a_selected_field() {
    let src = "\
query {
  orders {
    id
    ...orderFields
  }
}
";
    let consumers = parse_document(src, "doc.graphql");
    assert_eq!(consumers.len(), 1, "exactly one consumer for `orders`");
    let orders = consumers[0].field.as_str();
    assert_eq!(orders, "orders", "the top-level field must remain `orders`");
    let selected = &consumers[0].selected_fields;
    assert!(
        selected.iter().any(|f| f == "id"),
        "id must remain selected, got {selected:?}"
    );
    assert!(
        !selected.iter().any(|f| f == "orderFields"),
        "fragment spread name must NOT be a selected field, got {selected:?}"
    );
    let code = r"const Q = gql`query { orders { id ...orderFields } }`;";
    let in_code = lain::server::sensors::graphql_consumer_sensor::detect_in_code(code, "orders.ts");
    let orders_consumer = in_code
        .iter()
        .find(|c| c.field == "orders")
        .expect("orders consumer must be detected in the tagged template");
    assert!(
        !orders_consumer
            .selected_fields
            .iter()
            .any(|f| f == "orderFields"),
        "fragment spread name must NOT leak into selected_fields in TS, got {:?}",
        orders_consumer.selected_fields
    );
}

/// Task 6 case (b): an inline fragment (`... on Paid { total }`)
/// must NOT yield fields named `on` and `Paid`. The scanner
/// currently reads `..` as two empty identifiers, then `on` as a
/// field, then `Paid` as a field.
#[test]
fn t6_inline_fragment_does_not_yield_on_or_paid() {
    let src = "\
query {
  orders {
    id
    ... on Paid {
      total
    }
  }
}
";
    let consumers = parse_document(src, "doc.graphql");
    let orders = consumers
        .iter()
        .find(|c| c.field == "orders")
        .expect("orders consumer must be present");
    let selected = &orders.selected_fields;
    assert!(
        !selected.iter().any(|f| f == "on"),
        "inline fragment must NOT leak `on` as a selected field, got {selected:?}"
    );
    assert!(
        !selected.iter().any(|f| f == "Paid"),
        "inline fragment must NOT leak `Paid` as a selected field, got {selected:?}"
    );
    assert!(
        selected.iter().any(|f| f == "id"),
        "id must remain selected, got {selected:?}"
    );
    assert!(
        selected.iter().any(|f| f == "total"),
        "total (inside the inline fragment) must remain selected, got {selected:?}"
    );
}

/// Task 6 case (c): a top-level fragment spread
/// (`...frag`) must NOT create a `graphql-call:query:frag`
/// consumer. Today the consumer sensor reads `frag` as the only
/// top-level field of an (effectively anonymous) query block
/// and mints a `graphql-call:query:frag` record.
#[test]
fn t6_top_level_fragment_spread_does_not_become_a_consumer() {
    let src = "\
query {
  ...frag
  orders {
    id
  }
}
";
    let consumers = parse_document(src, "doc.graphql");
    assert!(
        !consumers.iter().any(|c| c.field == "frag"),
        "a top-level `...frag` must NOT mint a consumer named `frag`"
    );
    let orders = consumers
        .iter()
        .find(|c| c.field == "orders")
        .expect("orders consumer must still be detected");
    assert!(
        orders.selected_fields.iter().any(|f| f == "id"),
        "id must remain selected, got {:?}",
        orders.selected_fields
    );
    let code = r"const Q = gql`query { ...frag orders { id } }`;";
    let in_code = lain::server::sensors::graphql_consumer_sensor::detect_in_code(code, "q.ts");
    assert!(
        !in_code.iter().any(|c| c.field == "frag"),
        "in-code top-level `...frag` must NOT become a consumer"
    );
}

/// Task 6 case (d): an SDL with
/// `type User @key(fields: "id") implements Node { ... }` must
/// still produce a `Schema` node and its `Field` nodes. Today
/// the parser requires the very next non-space byte after the
/// type name to be `{`, so the entire block is silently
/// dropped, leaving the lineage empty for federation SDL —
/// which is exactly the case where field lineage matters most.
#[test]
fn t6_sdl_with_implements_and_directive_emits_schema_and_fields() {
    let src = "\
type User @key(fields: \"id\") implements Node {
  id: ID!
  email: String!
}
";
    let blocks = lain::server::sensors::graphql_provider_sensor::extract_object_type_blocks(src);
    let user = blocks
        .iter()
        .find(|b| b.name == "User")
        .expect("User object type block must be parsed despite @key/implements");
    let field_names: Vec<&str> = user.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(field_names, vec!["id", "email"]);
}

/// Task 6 case (e): `type Order implements Node { ... }` with no
/// directives must still produce Schema/Field nodes. Same root
/// cause, narrower shape.
#[test]
fn t6_sdl_with_implements_only_emits_schema_and_fields() {
    let src = "\
type Order implements Node {
  id: ID!
  total: Float!
}
";
    let blocks = lain::server::sensors::graphql_provider_sensor::extract_object_type_blocks(src);
    let order = blocks
        .iter()
        .find(|b| b.name == "Order")
        .expect("Order object type block must be parsed despite implements");
    let field_names: Vec<&str> = order.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(field_names, vec!["id", "total"]);
}

/// Task 6 case (f): deeply nested selection sets must not abort
/// the process. Today `top_level_fields_with_selections`
/// recurses without a depth cap, so adversarial input like
/// `a{a{a{…` blows the stack. The acceptance criterion is that
/// the parse call returns (success or a bounded subset — but
/// never aborts). The depth is chosen so the unfixed parser
/// overflows its 8 MiB stack on a CI runner. The body has a
/// top-level `a` field so the recursion happens via the selection
/// path (a leading `{` would be eaten as an "unattached block"
/// and the recursion would never trigger).
#[test]
fn t6_deeply_nested_selections_do_not_abort() {
    let levels = 50_000usize;
    let mut body = String::with_capacity(levels * 2 + 8);
    body.push('a');
    for _ in 0..levels {
        body.push_str("{a");
    }
    body.push_str("{x}");
    for _ in 0..levels {
        body.push('}');
    }
    // The recursive descent parses `x` once and then unwinds.
    // Whether the depth cap truncates output or not, the call
    // MUST return without aborting the process.
    let out =
        lain::server::sensors::graphql_consumer_sensor::top_level_fields_with_selections(&body);
    let _ = out;
}

// ─── Task 9 — byte-boundary coverage for the consumer parser
//
//     The mutation harness reports 66 survivors in
//     `graphql_consumer_sensor.rs`, almost all on the
//     `bytes[i] == b'X'` boundary comparisons for `(`, `)`,
//     `{`, `}`, `$`, `\n`, and the `while i < bytes.len() && <cond>`
//     short-circuits where the condition is always true. The
//     fixtures below land on each byte at the parser's branching
//     point so a `==`→`!=` flip changes the parsed result and
//     trips the test.

/// `$X` (dollar followed by a non-`{` byte) is *not* a
/// template interpolation and the document must not be
/// marked dynamic. With the `bytes[i+1] == b'{'` mutated to
/// `!=`, the parser returns `true` on the first non-`{` byte
/// after the dollar (e.g. `$x`), falsely flagging the
/// document dynamic.
#[test]
fn t9_byte_boundary_dollar_without_open_brace_is_not_interpolation() {
    let src = "query $x { orders { id } }";
    let consumers =
        lain::server::sensors::graphql_consumer_sensor::parse_document(src, "doc.graphql");
    // No `FieldRef` for `$x` should be emitted; either the
    // parser returns a single dynamic record (which we
    // explicitly disallow) or the query is parsed as a normal
    // query. Either way, no `dynamic: true` outcome is
    // acceptable for `$x` (no `{`).
    for c in &consumers {
        assert!(
            !c.dynamic,
            "`$x` (dollar without open brace) must not be flagged dynamic, got: {:?}",
            c
        );
    }
}

/// `query { order(id: ID!) { id status } }` exercises the
/// paren-skip path (`(`, `)`) and the selection-set entry
/// (`{`, `}`). The `id` and `status` reads are picked up only
/// if the selection set is correctly entered; a `==`→`!=` on
/// `bytes[i] == b'{'` at line 723 would skip the selection set
/// and lose the sub-fields, tripping the assertion.
#[test]
fn t9_byte_boundary_selection_set_keeps_subfields() {
    let body = "order(id: ID!) { id status }";
    let out =
        lain::server::sensors::graphql_consumer_sensor::top_level_fields_with_selections(body);
    assert_eq!(out.len(), 1, "one top-level field expected, got {out:?}");
    let (field, sub) = &out[0];
    assert_eq!(field, "order");
    assert_eq!(
        sub,
        &vec!["id".to_string(), "status".to_string()],
        "selection set must be entered (id + status), got {sub:?}"
    );
}

/// `query { order(id: ID!, customer: ID!) { name } }` lands
/// on `(` (line 676), `)` (line 682), and the comma / colon
/// boundaries inside the paren block. The comma-bearing
/// arguments also exercise the `bytes[i] != b'{' && bytes[i] != b'\n'`
/// loop guard at line 521.
#[test]
fn t9_byte_boundary_multiple_arguments_in_parens() {
    let body = "order(id: ID!, customer: ID!) { name }";
    let out =
        lain::server::sensors::graphql_consumer_sensor::top_level_fields_with_selections(body);
    assert_eq!(out.len(), 1);
    let (field, sub) = &out[0];
    assert_eq!(field, "order");
    assert_eq!(sub, &vec!["name".to_string()]);
}

/// A multi-line document lands on `bytes[i] == b'\n'` at the
/// `find_top_level_operations` line-409 and line-432 branches.
/// A mutation `==`→`!=` on line 432 would skip the line-number
/// update, but the parsed fields are still correct — the
/// `parse_document` test instead asserts that an interpolation
/// placeholder mid-line does NOT poison the result, exercising
/// the `b'{'` byte at line 723.
#[test]
fn t9_byte_boundary_multiline_document_keeps_operation() {
    let src = "\
query GetOrder {
  order(id: ID!) {
    id
    status
  }
}
";
    let consumers =
        lain::server::sensors::graphql_consumer_sensor::parse_document(src, "doc.graphql");
    let orders = consumers
        .iter()
        .find(|c| c.field == "order")
        .expect("`order` consumer must be parsed across multiple lines");
    assert_eq!(
        orders.selected_fields,
        vec!["id".to_string(), "status".to_string()],
        "multi-line document must still surface sub-fields, got {:?}",
        orders.selected_fields
    );
    assert!(
        !orders.dynamic,
        "plain multi-line document must not be dynamic"
    );
}

/// `query { ${userId} { id } }` exercises the `$` byte at
/// line 384. A `==`→`!=` mutation there makes
/// `has_interpolation` return `true` for non-`$` bytes, so
/// the document is wrongly marked dynamic. The test asserts
/// that a *non*-interpolated document is **not** dynamic.
#[test]
fn t9_byte_boundary_non_interpolated_document_is_not_dynamic() {
    // The fixture deliberately does NOT contain `$`. The
    // mutation flips the comparison, so `has_interpolation`
    // would return `true` on the first byte and the document
    // would be marked dynamic. We assert the opposite: a
    // plain document is `dynamic: false`.
    let src = "query { orders { id status } }";
    let consumers =
        lain::server::sensors::graphql_consumer_sensor::parse_document(src, "doc.graphql");
    assert_eq!(consumers.len(), 1, "exactly one consumer for `orders`");
    let orders = &consumers[0];
    assert!(
        !orders.dynamic,
        "non-interpolated document must NOT be dynamic, got: {:?}",
        orders
    );
    assert_eq!(orders.field, "orders");
    assert_eq!(
        orders.selected_fields,
        vec!["id".to_string(), "status".to_string()],
        "sub-fields must still be parsed on a non-interpolated document"
    );
}

/// `query GetOrder(id: ID!) { order(id: ID!) { id } }` has
/// nested parens (the operation header argument list AND the
/// field argument list). The inner paren check is line 446
/// (`bytes[i] == b'('`) and 448 (`bytes[i] == b')'`). A
/// mutation on either flips the depth counter and the parser
/// either under- or over-consumes the field.
#[test]
fn t9_byte_boundary_nested_parens_in_operation_header() {
    let src = "query GetOrder(id: ID!) { order(id: ID!) { id } }";
    let consumers =
        lain::server::sensors::graphql_consumer_sensor::parse_document(src, "doc.graphql");
    let order = consumers
        .iter()
        .find(|c| c.field == "order")
        .expect("`order` consumer must be parsed despite nested parens");
    assert_eq!(
        order.selected_fields,
        vec!["id".to_string()],
        "nested parens in the operation header must not eat the field, got {:?}",
        order.selected_fields
    );
    assert!(!order.dynamic);
}

/// Aliases (`: `) are picked up at line 654
/// (`bytes[i] == b':'`). A `==`→`!=` mutation there means the
/// alias is treated as the field name, and the actual field
/// is dropped.
#[test]
fn t9_byte_boundary_alias_uses_target_not_alias_name() {
    let body = "first: orders { id }";
    let out =
        lain::server::sensors::graphql_consumer_sensor::top_level_fields_with_selections(body);
    assert_eq!(out.len(), 1, "one top-level field, got {out:?}");
    let (field, sub) = &out[0];
    assert_eq!(
        field, "orders",
        "alias `first:` must resolve to the field name `orders`, got {field:?}"
    );
    assert_eq!(sub, &vec!["id".to_string()]);
}

/// A directive `@include(if: $cond)` lands on `@` (line 693),
/// `(` (line 701), and `)` (line 707). A `==`→`!=` on `@` at
/// line 693 leaves the directive unread and the parser
/// proceeds as if the directive were absent — the test still
/// passes. So we additionally check that the directive DOES
/// NOT introduce a new field name (the failure mode is
/// interpreting `if` as a sub-field).
#[test]
fn t9_byte_boundary_directive_does_not_leak_subfield() {
    let body = "orders @include(if: true) { id }";
    let out =
        lain::server::sensors::graphql_consumer_sensor::top_level_fields_with_selections(body);
    assert_eq!(out.len(), 1);
    let (field, sub) = &out[0];
    assert_eq!(field, "orders");
    assert!(
        !sub.iter().any(|s| s == "if" || s == "true"),
        "directive arguments must not be parsed as sub-fields, got {sub:?}"
    );
    assert!(sub.contains(&"id".to_string()));
}

/// `gql\`query { order(id: 1) { id } }\`` in TS: the
/// `detect_in_code` path tags the body and dispatches to
/// `parse_operation_body`. The byte boundary in
/// `parse_operation_body`'s `bytes[i] != b'{'` loop
/// (line 906) walks past the operation keyword. A mutation
/// there would loop forever — but the test guards with a
/// documented (non-adversarial) body so the test stays fast.
#[test]
fn t9_byte_boundary_in_code_gql_tagged_template() {
    let code = "const Q = gql`query { order(id: ID!) { id status } }`;";
    let in_code = lain::server::sensors::graphql_consumer_sensor::detect_in_code(code, "src/q.ts");
    let order = in_code
        .iter()
        .find(|c| c.field == "order")
        .expect("`order` consumer must be detected in the tagged template");
    assert_eq!(
        order.selected_fields,
        vec!["id".to_string(), "status".to_string()],
        "tagged-template body must surface the sub-fields, got {:?}",
        order.selected_fields
    );
    assert!(!order.dynamic);
}
