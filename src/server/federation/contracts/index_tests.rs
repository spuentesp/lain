//! Additional tests for the `ContractIndex` shapes that don't fit
//! in the in-file `#[cfg(test)]` block.

use std::collections::BTreeMap;

use crate::federation::contracts::index::{
    BoundField, ConfirmedBindingKey, ConfirmedBindingProviderKey, ConsumerResolution,
    ConsumerTarget, ContractIndex, Endpoint, EndpointId, EndpointProvider, EndpointSchema,
    FieldRefResolution, ServiceInfo, StaleBinding, StaleReason, UnresolvedReason,
};
use crate::federation::contracts::model::{
    ContractKey, Direction, FieldMeta, HttpMethod, JsonPath, MethodSpec, PathSegment,
    ProviderOrigin, ServiceName, TypeDesc,
};
use crate::federation::repo_id::{GlobalId, RepoId};

fn make_index() -> ContractIndex {
    let svc = ServiceName("orders".into());
    let key = ContractKey::Http {
        method: MethodSpec::Known(HttpMethod::Get),
        template: "/api/orders".into(),
    };
    let mut idx = ContractIndex::default();
    let repo = RepoId::new("orders").unwrap();
    let consumer_id = GlobalId::new(
        &repo,
        crate::schema::NodeType::HttpClientCall,
        "src/c.py",
        "fetch",
        Some(1),
    );
    let provider_node_id = GlobalId::new(
        &repo,
        crate::schema::NodeType::HttpRoute,
        "src/o.py",
        "get_orders",
        Some(10),
    );
    idx.services.insert(
        svc.clone(),
        ServiceInfo {
            name: svc.clone(),
            repo: repo.clone(),
            paths: vec![],
            hosts: vec!["orders.svc".into()],
            env: vec!["ORDERS_URL".into()],
            base_path: None,
            route_prefixes: vec![],
            endpoint_ids: vec![(svc.clone(), key.clone())],
        },
    );
    idx.endpoints.insert(
        (svc.clone(), key.clone()),
        Endpoint {
            id: (svc.clone(), key.clone()),
            method: HttpMethod::Get,
            template: "/api/orders".into(),
            providers: vec![EndpointProvider {
                node_id: provider_node_id,
                origin: ProviderOrigin::Code,
                handler: None,
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );
    idx.consumers.insert(
        consumer_id.clone(),
        ConsumerResolution {
            call_id: consumer_id,
            service: svc.clone(),
            target: Some(ConsumerTarget::Binds {
                provenance: crate::schema::EdgeProvenance::Heuristic {
                    detector: "static".into(),
                    confidence: 1.0,
                },
                confidence: 1.0,
                route_match: crate::schema::RouteMatch::Exact,
                stripped_prefix: None,
            }),
            bound_endpoints: vec![(svc, key)],
            reads_complete: true,
        },
    );
    idx
}

#[test]
fn contract_index_default_is_empty() {
    let idx = ContractIndex::default();
    assert!(idx.services.is_empty());
    assert!(idx.endpoints.is_empty());
    assert!(idx.consumers.is_empty());
    assert!(idx.field_refs.is_empty());
    assert!(idx.stale_bindings.is_empty());
    assert!(idx.external.is_empty());
    assert!(idx.unnormalized.is_empty());
}

#[test]
fn round_trip_constructed_index_through_partial_eq() {
    let a = make_index();
    let b = make_index();
    assert_eq!(a, b);
}

#[test]
fn stale_binding_constructs_with_reason() {
    let key = ConfirmedBindingKey {
        repo: RepoId::new("billing").unwrap(),
        path: "src/x.py".into(),
        symbol: "create_order".into(),
        key: "POST /api/orders".into(),
    };
    let prov = ConfirmedBindingProviderKey {
        service: ServiceName("orders".into()),
        key: "POST /api/orders".into(),
    };
    let stale = StaleBinding {
        consumer: key,
        provider: prov,
        reason: StaleReason::NoEndpoint,
    };
    assert!(matches!(stale.reason, StaleReason::NoEndpoint));
}

#[test]
fn unresolved_reason_variants_distinguish_themselves() {
    assert_ne!(
        UnresolvedReason::NoRouteInService,
        UnresolvedReason::NoMatch
    );
    assert_ne!(UnresolvedReason::NoMatch, UnresolvedReason::Unnormalized);
}

#[test]
fn field_ref_resolution_default_unknown() {
    let repo = RepoId::new("billing").unwrap();
    let fr = GlobalId::new(
        &repo,
        crate::schema::NodeType::FieldRef,
        "src/x.py",
        "r.customer.id",
        Some(1),
    );
    let r = FieldRefResolution {
        field_ref_id: fr,
        service: ServiceName("billing".into()),
        bound_fields: Vec::<BoundField>::new(),
        unknown: true,
    };
    assert!(r.unknown);
    assert!(r.bound_fields.is_empty());
}

#[test]
fn endpoint_schema_keyed_by_direction() {
    let mut schemas: std::collections::BTreeMap<Direction, EndpointSchema> =
        std::collections::BTreeMap::new();
    schemas.insert(
        Direction::Response,
        EndpointSchema {
            node_id: GlobalId::new(
                &RepoId::new("orders").unwrap(),
                crate::schema::NodeType::Schema,
                "openapi.yaml",
                "GET /api/orders Response",
                Some(1),
            ),
            fields: BTreeMap::new(),
        },
    );
    assert_eq!(schemas.len(), 1);
    assert!(schemas.contains_key(&Direction::Response));
    assert!(!schemas.contains_key(&Direction::Request));
}

#[test]
fn field_meta_serializes_to_json_properly() {
    let m = FieldMeta {
        ty: TypeDesc::String,
        required: true,
        nullable: false,
        enum_values: None,
    };
    let s = serde_json::to_string(&m).unwrap();
    let round: FieldMeta = serde_json::from_str(&s).unwrap();
    assert_eq!(round.ty, TypeDesc::String);
}

#[test]
fn jsonpath_round_trip_with_escapes() {
    let p = JsonPath(vec![
        PathSegment::Name("a.b".into()),
        PathSegment::ArrayItems,
        PathSegment::Name("c".into()),
        PathSegment::MapValues,
    ]);
    let s = p.to_string();
    let parsed: JsonPath = s.parse().unwrap();
    assert_eq!(parsed, p);
}

#[test]
fn jsonpath_dollar_property_escapes_with_backslash() {
    // §6.4 "a body property whose name starts with `$` is escaped as `\$`".
    let p = JsonPath(vec![PathSegment::Name("$ref".into())]);
    let s = p.to_string();
    assert_eq!(s, "\\$ref");
    let parsed: JsonPath = s.parse().unwrap();
    assert_eq!(parsed, p);
}

#[test]
fn jsonpath_dollar_query_sentinel_is_not_escaped() {
    // The reserved `$query` first segment (§6.4) is a literal
    // sentinel — its display form must not be backslash-escaped.
    let p = JsonPath(vec![
        PathSegment::Name("$query".into()),
        PathSegment::Name("limit".into()),
    ]);
    let s = p.to_string();
    assert_eq!(s, "$query.limit");
    let parsed: JsonPath = s.parse().unwrap();
    assert_eq!(parsed, p);
}

#[test]
fn endpoint_id_is_a_tuple_of_service_and_key() {
    let id: EndpointId = (
        ServiceName("orders".into()),
        ContractKey::Http {
            method: MethodSpec::Known(HttpMethod::Get),
            template: "/".into(),
        },
    );
    assert_eq!(id.0, ServiceName("orders".into()));
}
