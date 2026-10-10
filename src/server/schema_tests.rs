//! Tests for schema types

use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType};

#[test]
fn test_node_type_display() {
    assert_eq!(format!("{}", NodeType::Function), "Function");
    assert_eq!(format!("{}", NodeType::Struct), "Struct");
    assert_eq!(format!("{}", NodeType::File), "File");
}

#[test]
fn test_edge_type_display() {
    assert_eq!(format!("{}", EdgeType::Calls), "Calls");
    assert_eq!(format!("{}", EdgeType::Contains), "Contains");
    assert_eq!(format!("{}", EdgeType::Imports), "Imports");
}

#[test]
fn test_graph_node_new() {
    let node = GraphNode::new(
        NodeType::Function,
        "test_func".to_string(),
        "/src/lib.rs".to_string(),
    );
    assert_eq!(node.name, "test_func");
    assert_eq!(node.path, "/src/lib.rs");
    assert_eq!(node.node_type, NodeType::Function);
    assert!(!node.id.is_empty());
}

#[test]
fn test_graph_node_id_deterministic() {
    let node1 = GraphNode::new(
        NodeType::Function,
        "foo".to_string(),
        "/src/lib.rs".to_string(),
    );
    let node2 = GraphNode::new(
        NodeType::Function,
        "foo".to_string(),
        "/src/lib.rs".to_string(),
    );
    assert_eq!(node1.id, node2.id);
}

#[test]
fn test_graph_node_id_differs_by_name() {
    let node1 = GraphNode::new(
        NodeType::Function,
        "foo".to_string(),
        "/src/lib.rs".to_string(),
    );
    let node2 = GraphNode::new(
        NodeType::Function,
        "bar".to_string(),
        "/src/lib.rs".to_string(),
    );
    assert_ne!(node1.id, node2.id);
}

#[test]
fn test_graph_node_id_differs_by_path() {
    let node1 = GraphNode::new(
        NodeType::Function,
        "foo".to_string(),
        "/src/a.rs".to_string(),
    );
    let node2 = GraphNode::new(
        NodeType::Function,
        "foo".to_string(),
        "/src/b.rs".to_string(),
    );
    assert_ne!(node1.id, node2.id);
}

#[test]
fn test_graph_node_id_differs_by_type() {
    let node1 = GraphNode::new(
        NodeType::Function,
        "foo".to_string(),
        "/src/lib.rs".to_string(),
    );
    let node2 = GraphNode::new(
        NodeType::Struct,
        "foo".to_string(),
        "/src/lib.rs".to_string(),
    );
    assert_ne!(node1.id, node2.id);
}

#[test]
fn test_graph_node_all_node_types() {
    // Every variant from `NodeType::all()` round-trips through
    // `GraphNode::new`. Keeping this list in sync with the enum is
    // what `describe_schema_covers_every_node_type` checks for the
    // query side; this checks that each variant is also a usable
    // `GraphNode.node_type` value (no field-construction panic).
    let types = [
        NodeType::File,
        NodeType::Namespace,
        NodeType::Module,
        NodeType::Package,
        NodeType::Class,
        NodeType::Interface,
        NodeType::Struct,
        NodeType::Enum,
        NodeType::Trait,
        NodeType::Function,
        NodeType::Method,
        NodeType::Property,
        NodeType::Variable,
        NodeType::Constant,
        NodeType::HttpRoute,
        NodeType::Topic,
        NodeType::Resource,
        NodeType::Schema,
        NodeType::HttpClientCall,
        NodeType::Field,
        NodeType::FieldRef,
        NodeType::Synthetic,
    ];

    for ntype in types {
        let node = GraphNode::new(ntype.clone(), "test".to_string(), "/test.rs".to_string());
        assert_eq!(node.node_type, ntype);
    }
}

#[test]
fn test_graph_node_with_line_range() {
    let mut node = GraphNode::new(
        NodeType::Function,
        "range_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    node.line_start = Some(10);
    node.line_end = Some(25);
    assert_eq!(node.line_start, Some(10));
    assert_eq!(node.line_end, Some(25));
}

#[test]
fn test_graph_node_with_signature() {
    let mut node = GraphNode::new(
        NodeType::Function,
        "sig_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    node.signature = Some("(a: i32, b: String) -> bool".to_string());
    assert!(node.signature.is_some());
}

#[test]
fn test_graph_node_with_docstring() {
    let mut node = GraphNode::new(
        NodeType::Function,
        "doc_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    node.docstring = Some("This is a documentation string".to_string());
    assert!(node.docstring.is_some());
}

#[test]
fn test_graph_node_with_metadata() {
    let mut node = GraphNode::new(
        NodeType::Function,
        "meta_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    node.fan_in = Some(5);
    node.fan_out = Some(10);
    node.anchor_score = Some(0.75);
    node.depth_from_main = Some(3);
    node.co_change_count = Some(7);
    node.is_deprecated = true;
    assert_eq!(node.fan_in, Some(5));
    assert_eq!(node.fan_out, Some(10));
    assert_eq!(node.anchor_score, Some(0.75));
    assert_eq!(node.depth_from_main, Some(3));
    assert_eq!(node.co_change_count, Some(7));
    assert!(node.is_deprecated);
}

#[test]
fn test_graph_node_clone() {
    let node = GraphNode::new(
        NodeType::Function,
        "clone_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    let cloned = node.clone();
    assert_eq!(cloned.id, node.id);
    assert_eq!(cloned.name, node.name);
    assert_eq!(cloned.node_type, node.node_type);
}

#[test]
fn test_graph_edge_new() {
    let edge = GraphEdge::new(EdgeType::Calls, "node_a".to_string(), "node_b".to_string());
    assert_eq!(edge.source_id, "node_a");
    assert_eq!(edge.target_id, "node_b");
    assert_eq!(edge.edge_type, EdgeType::Calls);
}

#[test]
fn test_graph_edge_all_types() {
    // Every variant from `EdgeType::all()` round-trips through
    // `GraphEdge::new`. Mirrors the node-type counterpart above.
    let types = [
        EdgeType::Contains,
        EdgeType::Calls,
        EdgeType::Uses,
        EdgeType::Implements,
        EdgeType::Imports,
        EdgeType::CoChangedWith,
        EdgeType::Pattern,
        EdgeType::CallsHttp,
        EdgeType::Produces,
        EdgeType::Consumes,
        EdgeType::DeployedTo,
        EdgeType::CrossRepoSameSymbol,
        EdgeType::DynamicDispatch,
        EdgeType::BusTopic,
        EdgeType::RouteMatches,
        EdgeType::RuntimeCall,
        EdgeType::SendsHttp,
        EdgeType::RequestSchema,
        EdgeType::ResponseSchema,
        EdgeType::HasField,
        EdgeType::ReadsField,
        EdgeType::ReadsFrom,
        EdgeType::Binds,
    ];

    for edge_type in types {
        let edge = GraphEdge::new(edge_type.clone(), "src".to_string(), "tgt".to_string());
        assert_eq!(edge.edge_type, edge_type);
    }
}

#[test]
fn test_graph_edge_clone() {
    let edge = GraphEdge::new(EdgeType::Calls, "src".to_string(), "tgt".to_string());
    let cloned = edge.clone();
    assert_eq!(cloned.source_id, edge.source_id);
    assert_eq!(cloned.target_id, edge.target_id);
    assert_eq!(cloned.edge_type, edge.edge_type);
}

#[test]
fn test_graph_node_serialize() {
    let node = GraphNode::new(
        NodeType::Function,
        "ser_fn".to_string(),
        "/src/lib.rs".to_string(),
    );
    let json = serde_json::to_string(&node).unwrap();
    assert!(json.contains("ser_fn"));
    assert!(json.contains("Function"));
}

#[test]
fn test_graph_node_deserialize() {
    let json = r#"{"id":"test-id","node_type":"Function","name":"deser_fn","path":"/src/lib.rs","line_start":null,"line_end":null,"signature":null,"docstring":null,"embedding":null,"fan_in":null,"fan_out":null,"anchor_score":null,"depth_from_main":null,"co_change_count":null,"is_deprecated":false,"label":null,"last_lsp_sync":null,"last_git_sync":null,"commit_hash":null,"is_hydrated":true}"#;
    let node: GraphNode = serde_json::from_str(json).unwrap();
    assert_eq!(node.name, "deser_fn");
    assert_eq!(node.node_type, NodeType::Function);
}

/// Backwards compatibility: a GraphNode serialised with an older schema
/// (missing the `label` field) must still deserialize. This guards against
/// the bincode-persistence regression where adding any field would break
/// every existing `.lain/graph.bin` from prior releases.
#[test]
fn test_graph_node_deserialize_without_label_is_backward_compatible() {
    let old_json = r#"{"id":"old","node_type":"Function","name":"old_fn","path":"/src/lib.rs","line_start":null,"line_end":null,"signature":null,"docstring":null,"embedding":null,"fan_in":null,"fan_out":null,"anchor_score":null,"depth_from_main":null,"co_change_count":null,"is_deprecated":false,"last_lsp_sync":null,"last_git_sync":null,"commit_hash":null,"is_hydrated":true}"#;
    let node: GraphNode =
        serde_json::from_str(old_json).expect("old-format JSON must still deserialize");
    assert_eq!(node.name, "old_fn");
    assert!(node.label.is_none());
}

#[test]
fn test_graph_edge_serialize() {
    let edge = GraphEdge::new(EdgeType::Calls, "src_id".to_string(), "tgt_id".to_string());
    let json = serde_json::to_string(&edge).unwrap();
    assert!(json.contains("src_id"));
    assert!(json.contains("tgt_id"));
    assert!(json.contains("Calls"));
}

#[test]
fn test_graph_edge_deserialize() {
    let json = r#"{"source_id":"a","target_id":"b","edge_type":"Imports","metadata":{}}"#;
    let edge: GraphEdge = serde_json::from_str(json).unwrap();
    assert_eq!(edge.source_id, "a");
    assert_eq!(edge.target_id, "b");
    assert_eq!(edge.edge_type, EdgeType::Imports);
}

#[test]
fn test_node_type_equality() {
    assert_eq!(NodeType::Function, NodeType::Function);
    assert_ne!(NodeType::Function, NodeType::Struct);
}

#[test]
fn test_edge_type_equality() {
    assert_eq!(EdgeType::Calls, EdgeType::Calls);
    assert_ne!(EdgeType::Calls, EdgeType::Contains);
}

/// Two symbols with the same (type, path, name) but different line ranges —
/// e.g. a top-level `fn add` and an `impl` method `add` — must have distinct
/// IDs so both survive in the graph. Previously they collided because the ID
/// ignored line_start, causing the second insert to overwrite the first.
#[test]
fn test_graph_node_id_differs_by_line_range() {
    let top_level = GraphNode::new(
        NodeType::Function,
        "add".to_string(),
        "/src/lib.rs".to_string(),
    )
    .with_location(1, 1);
    let impl_method = GraphNode::new(
        NodeType::Function,
        "add".to_string(),
        "/src/lib.rs".to_string(),
    )
    .with_location(7, 7);
    assert_ne!(
        top_level.id, impl_method.id,
        "Functions named 'add' at different lines must have distinct IDs (got {} vs {})",
        top_level.id, impl_method.id
    );
}

// ─── PR 3 (Schema v3) tests ────────────────────────────────────────────
//
// §4.2: new node/edge variants added with `all()` updated.
// §4.3: new fields on `GraphNode` and `GraphEdge`.
// §5.4: version bumps — refusal tested separately in
// `graph_backend_tests.rs`.

/// `NodeType::all()` returns every declared variant. The contract
/// nodes (HttpClientCall, Field, FieldRef) are part of schema v3 and
/// must appear in `all()` so `describe_schema` and the joiner can
/// reach them. A drift between the enum and `all()` (the kind of
/// drift that lost `Method` in the v0.7 incident) is exactly the
/// regression this test guards against.
#[test]
fn test_node_type_all_includes_contract_variants() {
    use std::collections::HashSet;
    let all: HashSet<String> = NodeType::all().iter().map(|t| t.to_string()).collect();
    for v in ["HttpClientCall", "Field", "FieldRef"] {
        assert!(
            all.contains(v),
            "NodeType::all() must include `{v}` (schema v3 contract node)"
        );
    }
}

/// `EdgeType::all()` returns every declared variant. The §4.2 contract
/// edges (SendsHttp, RequestSchema, ResponseSchema, PayloadSchema,
/// HasField, ReadsField, ReadsFrom, Binds) must appear so the
/// impact-traversal table (§5.2) and the joiner can refer to them
/// by name.
#[test]
fn test_edge_type_all_includes_contract_variants() {
    use std::collections::HashSet;
    let all: HashSet<String> = EdgeType::all().iter().map(|t| t.to_string()).collect();
    for v in [
        "SendsHttp",
        "RequestSchema",
        "ResponseSchema",
        "PayloadSchema",
        "HasField",
        "ReadsField",
        "ReadsFrom",
        "Binds",
    ] {
        assert!(
            all.contains(v),
            "EdgeType::all() must include `{v}` (schema v3 contract edge)"
        );
    }
}

/// `is_indexed()` is the "does any sensor actually emit this today?"
/// flag. The contract-edge types that the wired pipeline now emits
/// (`ReadsField`, `ReadsFrom`, `Binds` — PR 13 owns this flip) must
/// report `true`. The node types and the schema-bearing edges still
/// report `false` because their producer graph is not wired into
/// the default ingest pipeline (the schema/edge information is
/// reconstructed at joiner time, not at sensor time).
#[test]
fn test_contract_nodes_and_edges_are_marked_indexed() {
    assert!(NodeType::HttpClientCall.is_indexed());
    assert!(NodeType::Field.is_indexed());
    assert!(NodeType::FieldRef.is_indexed());
    assert!(NodeType::Topic.is_indexed());
    assert!(NodeType::Schema.is_indexed());
    for e in [
        EdgeType::SendsHttp,
        EdgeType::RequestSchema,
        EdgeType::ResponseSchema,
        EdgeType::HasField,
        EdgeType::ReadsField,
        EdgeType::ReadsFrom,
        EdgeType::Binds,
        EdgeType::Produces,
        EdgeType::Consumes,
    ] {
        assert!(e.is_indexed(), "{e} is wired and must be marked indexed");
    }
    // `PayloadSchema` has no producer yet — `payload_schema.rs` parses
    // payload files but no sensor mints the Topic → Schema edge. It must
    // stay in the "known fiction" set or `describe_schema` lies.
    assert!(
        !EdgeType::PayloadSchema.is_indexed(),
        "PayloadSchema has no producer and must not be advertised as indexed"
    );
}

/// `source_types` / `target_types` for each new edge — `describe_schema`
/// answers "what can I attach this edge to?" from these lists. The
/// contract edges target the contract node types exclusively, and
/// `Binds` is the one with three target shapes (HttpRoute / Field /
/// Topic) — losing any of them silently drops a join arm.
#[test]
fn test_contract_edge_endpoints_match_node_type_definitions() {
    assert!(EdgeType::SendsHttp
        .source_types()
        .contains(&NodeType::Function));
    assert_eq!(
        EdgeType::SendsHttp.target_types(),
        &[NodeType::HttpClientCall]
    );
    assert_eq!(
        EdgeType::RequestSchema.source_types(),
        &[NodeType::HttpRoute, NodeType::Module]
    );
    assert_eq!(
        EdgeType::ResponseSchema.source_types(),
        &[NodeType::HttpRoute, NodeType::Module]
    );
    assert_eq!(EdgeType::ResponseSchema.target_types(), &[NodeType::Schema]);
    assert_eq!(EdgeType::PayloadSchema.source_types(), &[NodeType::Topic]);
    assert_eq!(EdgeType::HasField.source_types(), &[NodeType::Schema]);
    assert_eq!(EdgeType::HasField.target_types(), &[NodeType::Field]);
    assert_eq!(
        EdgeType::ReadsField.source_types(),
        &[NodeType::Function, NodeType::Method]
    );
    assert_eq!(EdgeType::ReadsField.target_types(), &[NodeType::FieldRef]);
    assert_eq!(EdgeType::ReadsFrom.source_types(), &[NodeType::FieldRef]);
    assert_eq!(
        EdgeType::ReadsFrom.target_types(),
        &[
            NodeType::HttpClientCall,
            NodeType::Function,
            NodeType::Method
        ]
    );
    // `Binds`: consumer → provider. Three source shapes (HttpClientCall,
    // FieldRef, Topic) and three target shapes (HttpRoute, Field,
    // Topic). Order-preserving check so the wire form stays stable.
    assert_eq!(
        EdgeType::Binds.source_types(),
        &[
            NodeType::HttpClientCall,
            NodeType::FieldRef,
            NodeType::Topic
        ]
    );
    assert_eq!(
        EdgeType::Binds.target_types(),
        &[NodeType::HttpRoute, NodeType::Field, NodeType::Topic]
    );
}

/// `GraphNode::new` initializes `contract` and `entry` to `None` so a
/// freshly-constructed node survives the schema-v3 wire form. The
/// bincode round-trip on the next test will reuse this shape, so a
/// regression here also breaks that test.
#[test]
fn test_graph_node_new_initializes_contract_and_entry_to_none() {
    let node = GraphNode::new(NodeType::Function, "f".into(), "src/lib.rs".into());
    assert!(node.contract.is_empty());
    assert!(node.entry.is_none());
}

/// `GraphEdge::new` initializes `site` and `detail` to `None` for
/// the same reason as the node counterpart above.
#[test]
fn test_graph_edge_new_initializes_site_and_detail_to_none() {
    let edge = GraphEdge::new(EdgeType::Calls, "a".into(), "b".into());
    assert!(edge.site.is_none());
    assert!(edge.detail.is_none());
}

/// `EdgeProvenance::Confirmed` carries a `source` string of the form
/// `repos.yaml#bindings[<i>]` (§4.6). Round-tripping it through the
/// externally tagged wire form is the contract the joiner and the
/// audit log both rely on.
#[test]
fn test_edge_provenance_confirmed_roundtrips() {
    let original = EdgeProvenance::Confirmed {
        source: "repos.yaml#bindings[3]".to_string(),
    };
    let json = serde_json::to_string(&original).unwrap();
    assert!(
        json.contains("Confirmed"),
        "Confirmed must serialize as an externally tagged variant; got {json}"
    );
    assert!(json.contains("repos.yaml#bindings[3]"));
    let parsed: EdgeProvenance = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, original);
}

/// `GraphEdge::detail` is `Option<EdgeDetail>` whose fields are each
/// `Option` so a `Binds` edge can carry `RouteMatch::Exact` without
/// a `stripped_prefix`. The default-`None` shape must round-trip
/// cleanly through both serde forms.
#[test]
fn test_edge_detail_roundtrips() {
    use crate::schema::{EdgeDetail, RouteMatch};
    let exact = EdgeDetail {
        route_match: Some(RouteMatch::Exact),
        stripped_prefix: None,
    };
    let json = serde_json::to_string(&exact).unwrap();
    let parsed: EdgeDetail = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, exact);

    let prefix = EdgeDetail {
        route_match: Some(RouteMatch::PrefixStripped),
        stripped_prefix: Some("/api/v1".to_string()),
    };
    let json = serde_json::to_string(&prefix).unwrap();
    let parsed: EdgeDetail = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, prefix);
}

/// Bincode round-trip for the new `GraphEdge` shape. The on-disk
/// graph uses bincode 2.x's legacy codec; a positional layout
/// regression here would surface as a load failure on a freshly-
/// built backend, and would not be caught by the JSON tests above.
///
/// `GraphEdge` does not derive `PartialEq` (the embedded
/// `federation::contracts::model::SourceSite` adds no useful
/// semantics, and existing callers don't need it), so the assertion
/// is field-by-field.
#[test]
fn test_graph_edge_bincode_roundtrips_with_new_fields() {
    use crate::schema::EdgeDetail;
    let original = GraphEdge {
        edge_type: EdgeType::Binds,
        source_id: "orders:HttpClientCall:src/orders_api.py:get_order:42".to_string(),
        target_id: "billing:HttpRoute:src/billing.py:GET /orders/{}:7".to_string(),
        weight: Some(1.0),
        cross_repo: true,
        provenance: Some(EdgeProvenance::Confirmed {
            source: "repos.yaml#bindings[0]".to_string(),
        }),
        site: Some(crate::federation::contracts::model::SourceSite {
            path: "src/orders_api.py".to_string(),
            line: 42,
        }),
        detail: Some(EdgeDetail {
            route_match: Some(crate::schema::RouteMatch::Exact),
            stripped_prefix: None,
        }),
    };
    let bytes =
        bincode::serde::encode_to_vec(&original, bincode::config::legacy()).expect("encode");
    let (decoded, _consumed): (GraphEdge, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).expect("decode");
    assert_eq!(decoded.edge_type, original.edge_type);
    assert_eq!(decoded.source_id, original.source_id);
    assert_eq!(decoded.target_id, original.target_id);
    assert_eq!(decoded.weight, original.weight);
    assert_eq!(decoded.cross_repo, original.cross_repo);
    assert_eq!(decoded.provenance, original.provenance);
    assert_eq!(decoded.site, original.site);
    assert_eq!(decoded.detail, original.detail);
}

/// Bincode round-trip for `GraphNode` with the new `contract` /
/// `entry` fields populated. Mirrors the edge counterpart above.
/// `GraphNode` does not derive `PartialEq`, so the assertion is
/// field-by-field on the schema-v3 additions plus a name check.
#[test]
fn test_graph_node_bincode_roundtrips_with_contract_field() {
    use crate::federation::contracts::model::{
        ContractFact, EntryKind, HttpMethod, ProviderFact, ProviderOrigin,
    };
    let mut original = GraphNode::new(
        NodeType::HttpRoute,
        "GET /orders/{}".to_string(),
        "src/orders.py".to_string(),
    );
    original.line_start = Some(7);
    original.contract = vec![ContractFact::Provider(ProviderFact {
        method: HttpMethod::Get,
        template: "/orders/{}".to_string(),
        handler: None,
        operation_id: Some("getOrder".to_string()),
        origin: ProviderOrigin::OpenApi,
    })];
    original.entry = Some(EntryKind::HttpHandler);

    let bytes =
        bincode::serde::encode_to_vec(&original, bincode::config::legacy()).expect("encode");
    let (decoded, _consumed): (GraphNode, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).expect("decode");
    assert_eq!(decoded.node_type, original.node_type);
    assert_eq!(decoded.name, original.name);
    assert_eq!(decoded.path, original.path);
    assert_eq!(decoded.line_start, original.line_start);
    assert_eq!(decoded.contract, original.contract);
    assert_eq!(decoded.entry, original.entry);
}
