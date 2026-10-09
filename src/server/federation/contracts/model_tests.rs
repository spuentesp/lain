//! Round-trip and shape tests for the contract-federation model.
//!
//! The model is a wire contract: every sensor (PR 5–9) writes one of
//! these types, and every later tool (PR 10+) reads them back. The
//! bincode serializer is positional, so a layout regression here
//! would surface as a load failure on a freshly-built federation
//! graph. The tests in this module exist so that regression fails at
//! `cargo test` time, not at the operator's `lain server` start.
//!
//! All enums are externally tagged (the only form bincode 2.x's
//! legacy codec can decode). Internal/untagged tagging would still
//! serialize correctly but would not round-trip back through
//! `decode_from_slice`.

use crate::federation::contracts::model::{
    CallVia, ContractFact, ContractKey, Direction, EntryKind, FieldMeta, FieldReadFact,
    FieldReadOrigin, HostPart, HttpMethod, JsonPath, MethodSpec, NormalizedUrl, PathSegment,
    ProviderFact, ProviderOrigin, ServiceName, SourceSite, SymbolKey, TypeDesc,
};
use crate::federation::repo_id::RepoId;

/// Bincode round-trip for `ContractFact::Provider` carrying the full
/// `ProviderFact`. Mirrors the JSON variants elsewhere — a
/// regression in the externally tagged `ContractFact` enum would
/// surface here first.
#[test]
fn contract_fact_provider_roundtrips_through_bincode() {
    let repo = RepoId::new("orders").unwrap();
    let original = ContractFact::Provider(ProviderFact {
        method: HttpMethod::Get,
        template: "/api/orders/{}".to_string(),
        handler: Some(SymbolKey {
            repo: repo.clone(),
            path: "src/orders.py".to_string(),
            container: Some("OrdersAPI".to_string()),
            name: "get_order".to_string(),
        }),
        operation_id: Some("getOrder".to_string()),
        origin: ProviderOrigin::OpenApi,
    });
    let bytes =
        bincode::serde::encode_to_vec(&original, bincode::config::legacy()).expect("encode");
    let (decoded, _consumed): (ContractFact, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).expect("decode");
    assert_eq!(decoded, original);
}

/// `ContractFact::FieldRead` carries a `JsonPath`, which is itself a
/// `Vec<PathSegment>` newtype. Round-trip through bincode catches a
/// layout drift in either.
#[test]
fn field_read_fact_roundtrips_through_bincode() {
    let original = ContractFact::FieldRead(FieldReadFact {
        chain: JsonPath(vec![
            PathSegment::Name("customer".to_string()),
            PathSegment::Name("address".to_string()),
            PathSegment::Name("city".to_string()),
        ]),
        exact: true,
        origin: FieldReadOrigin::GraphqlConsumer,
    });
    let bytes =
        bincode::serde::encode_to_vec(&original, bincode::config::legacy()).expect("encode");
    let (decoded, _consumed): (ContractFact, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).expect("decode");
    assert_eq!(decoded, original);
}

/// `origin` is `#[serde(default)]` so a JSON payload without the field
/// still deserialises, as `FieldAccess`. (Bincode is not
/// self-describing; this covers the serde-JSON path an external client
/// or a hand-written fixture uses.)
#[test]
fn field_read_origin_defaults_to_field_access() {
    let without_origin = serde_json::json!({
        "FieldRead": { "chain": [{"Name": "x"}], "exact": true }
    });
    let fact: ContractFact =
        serde_json::from_value(without_origin).expect("legacy payload deserialises");
    match fact {
        ContractFact::FieldRead(r) => assert_eq!(r.origin, FieldReadOrigin::FieldAccess),
        other => panic!("expected FieldRead, got {other:?}"),
    }

    // And an explicit origin round-trips.
    let with_origin = serde_json::json!({
        "FieldRead": { "chain": [{"Name": "x"}], "exact": true, "origin": "graphql_consumer" }
    });
    let fact: ContractFact = serde_json::from_value(with_origin).expect("deserialises");
    match fact {
        ContractFact::FieldRead(r) => assert_eq!(r.origin, FieldReadOrigin::GraphqlConsumer),
        other => panic!("expected FieldRead, got {other:?}"),
    }
}

/// Every `HttpMethod` variant round-trips. The literal `MethodSpec`
/// the joiner sees depends on this — a wrong serialization for
/// `Any` would silently break the go-std HTTP route matching (§6.2).
#[test]
fn every_http_method_variant_roundtrips() {
    for m in [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
        HttpMethod::Head,
        HttpMethod::Options,
        HttpMethod::Any,
    ] {
        let bytes = bincode::serde::encode_to_vec(m, bincode::config::legacy()).unwrap();
        let (decoded, _): (HttpMethod, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
        assert_eq!(decoded, m);
    }
}

/// `ContractKey::Http` carries a `MethodSpec`, and the externally
/// tagged enum must round-trip both `Known(HttpMethod::Get)` and
/// `MethodSpec::Unknown` (the consumer-side wildcard form).
#[test]
fn contract_key_http_roundtrips_for_known_and_unknown_method() {
    let known = ContractKey::Http {
        method: MethodSpec::Known(HttpMethod::Get),
        template: "/api/orders/{}".to_string(),
    };
    let bytes = bincode::serde::encode_to_vec(&known, bincode::config::legacy()).unwrap();
    let (decoded, _): (ContractKey, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, known);

    let unknown = ContractKey::Http {
        method: MethodSpec::Unknown,
        template: "/api/webhooks".to_string(),
    };
    let bytes = bincode::serde::encode_to_vec(&unknown, bincode::config::legacy()).unwrap();
    let (decoded, _): (ContractKey, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, unknown);
}

/// `FieldMeta.enum_values` is `Option<Vec<String>>` — the joiner
/// uses it to flag fields whose enum is empty / mismatch. A `None`
/// and a populated `Vec` must both round-trip cleanly.
#[test]
fn field_meta_with_and_without_enum_values_roundtrips() {
    let without = ContractFact::Field(FieldMeta {
        ty: TypeDesc::String,
        required: true,
        nullable: false,
        enum_values: None,
    });
    let bytes = bincode::serde::encode_to_vec(&without, bincode::config::legacy()).unwrap();
    let (decoded, _): (ContractFact, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, without);

    let with = ContractFact::Field(FieldMeta {
        ty: TypeDesc::String,
        required: false,
        nullable: true,
        enum_values: Some(vec!["a".to_string(), "b".to_string(), "c".to_string()]),
    });
    let bytes = bincode::serde::encode_to_vec(&with, bincode::config::legacy()).unwrap();
    let (decoded, _): (ContractFact, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, with);
}

/// `NormalizedUrl.host` covers every `HostPart` variant. `Env` carries
/// a `Vec<String>` of env-var names, and `Expr` carries the raw
/// unresolved expression. Both must round-trip.
#[test]
fn normalized_url_host_variants_roundtrip() {
    let cases = [
        NormalizedUrl {
            host: HostPart::None,
            template: Some("/api/x".to_string()),
        },
        NormalizedUrl {
            host: HostPart::Literal("orders.svc.cluster.local".to_string()),
            template: Some("/api/orders".to_string()),
        },
        NormalizedUrl {
            host: HostPart::Env(vec![
                "ORDERS_URL".to_string(),
                "ORDERS_BASE_URL".to_string(),
            ]),
            template: Some("/api/orders".to_string()),
        },
        NormalizedUrl {
            host: HostPart::Expr("settings.base_url".to_string()),
            template: None, // dynamic path
        },
    ];
    for url in cases {
        let bytes = bincode::serde::encode_to_vec(&url, bincode::config::legacy()).unwrap();
        let (decoded, _): (NormalizedUrl, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
        assert_eq!(decoded, url);
    }
}

/// `CallVia` has two variants — `Library { name }` and
/// `Receiver { expr, fn_name }` — both round-trip. The joiner keeps
/// only the `Receiver` variants that match `http_clients` (§7.3);
/// the bincode form is what reaches the joiner in the federated
/// graph.
#[test]
fn call_via_variants_roundtrip() {
    let lib = CallVia::Library {
        name: "requests".to_string(),
    };
    let bytes = bincode::serde::encode_to_vec(&lib, bincode::config::legacy()).unwrap();
    let (decoded, _): (CallVia, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, lib);

    let recv = CallVia::Receiver {
        expr: "ordersClient".to_string(),
        fn_name: "get".to_string(),
        base: None,
    };
    let bytes = bincode::serde::encode_to_vec(&recv, bincode::config::legacy()).unwrap();
    let (decoded, _): (CallVia, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, recv);
}

/// `TypeDesc::Array(Box<TypeDesc>)` is a recursive variant — the box
/// indirection must not fool the bincode positional layout. Two
/// levels of nesting (Array(Array(Integer))) is enough to catch the
/// regression if the recursion is dropped.
#[test]
fn type_desc_recursive_array_roundtrips() {
    let nested = TypeDesc::Array(Box::new(TypeDesc::Array(Box::new(TypeDesc::Integer))));
    let bytes = bincode::serde::encode_to_vec(&nested, bincode::config::legacy()).unwrap();
    let (decoded, _): (TypeDesc, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, nested);
}

/// `EntryKind`, `Direction`, `SourceSite`, and `ServiceName` are
/// carried on `GraphNode.entry` / `GraphEdge.site` / the
/// `EndpointId` tuple. Round-tripping them together is the contract
/// the joiner and the tools rely on.
#[test]
fn small_types_roundtrip() {
    for entry in [
        EntryKind::HttpHandler,
        EntryKind::Scheduled,
        EntryKind::Cli,
        EntryKind::Main,
    ] {
        let bytes = bincode::serde::encode_to_vec(entry, bincode::config::legacy()).unwrap();
        let (decoded, _): (EntryKind, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
        assert_eq!(decoded, entry);
    }
    for dir in [Direction::Request, Direction::Response, Direction::Payload] {
        let bytes = bincode::serde::encode_to_vec(dir, bincode::config::legacy()).unwrap();
        let (decoded, _): (Direction, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
        assert_eq!(decoded, dir);
    }
    let site = SourceSite {
        path: "src/orders_api.py".to_string(),
        line: 42,
    };
    let bytes = bincode::serde::encode_to_vec(&site, bincode::config::legacy()).unwrap();
    let (decoded, _): (SourceSite, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, site);

    let svc = ServiceName("orders".to_string());
    let bytes = bincode::serde::encode_to_vec(&svc, bincode::config::legacy()).unwrap();
    let (decoded, _): (ServiceName, usize) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
    assert_eq!(decoded, svc);
}
