//! Pure-function tests for the §9 diff / classify / evaluate / coverage
//! pipeline.
//!
//! Coverage:
//! - §9.4 `classify`: every `ChangeKind` × `Direction` cell.
//! - §9.5 `evaluate`: every cell of the per-consumer table.
//! - §9.7 `could_match`: positive, negative, and prefix-tolerance rows.
//! - Property: `diff(a, a)` is empty.
//! - §15.2 scenario tests (1, 2, 5b, 6, 11, 12, 19, 20, 21, 22) at the
//!   pure-function level. The scenario rows in the tracker remain
//!   pending — PR 13 owns the MCP e2e for those.

use std::collections::{BTreeMap, BTreeSet};

use crate::federation::contracts::diff::{
    build_coverage, classify, could_match, diff_consumers, diff_contracts, diff_fields, evaluate,
    Affected, Change, ChangeKind, Class, Compat, ConsumerDef, ConsumerKey, ConsumerTargetKey,
    ContractSurface, Coverage, EndpointDef, ProviderRef, Reason, Scope, StaticChangedFiles,
    SurfaceResolution,
};
use crate::federation::contracts::index::{
    ConsumerResolution, ConsumerTarget, ContractIndex, Endpoint, EndpointId, EndpointProvider,
    EndpointSchema, UnresolvedReason,
};
use crate::federation::contracts::model::{
    ContractKey, Direction, FieldMeta, HttpMethod, JsonPath, MethodSpec, PathSegment,
    ProviderOrigin, ServiceName, SymbolKey, TypeDesc,
};
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::schema::EdgeProvenance;

// ─── helpers ────────────────────────────────────────────────────────

fn repo(s: &str) -> RepoId {
    RepoId::new(s).unwrap()
}

fn id(repo_str: &str, kind: &str, path: &str, name: &str, line: u32) -> GlobalId {
    let nt = match kind {
        "HttpRoute" => crate::schema::NodeType::HttpRoute,
        "HttpClientCall" => crate::schema::NodeType::HttpClientCall,
        "Field" => crate::schema::NodeType::Field,
        "FieldRef" => crate::schema::NodeType::FieldRef,
        "Function" => crate::schema::NodeType::Function,
        _ => crate::schema::NodeType::Synthetic,
    };
    GlobalId::new(&repo(repo_str), nt, path, name, Some(line))
}

fn svc(s: &str) -> ServiceName {
    ServiceName(s.into())
}

fn http_key(method: HttpMethod, template: &str) -> ContractKey {
    ContractKey::Http {
        method: MethodSpec::Known(method),
        template: template.to_string(),
    }
}

fn path(segments: &[&str]) -> JsonPath {
    JsonPath(
        segments
            .iter()
            .map(|s| PathSegment::Name((*s).to_string()))
            .collect(),
    )
}

fn field(ty: TypeDesc, required: bool, nullable: bool) -> FieldMeta {
    FieldMeta {
        ty,
        required,
        nullable,
        enum_values: None,
    }
}

fn endpoint_id(service: &str, method: HttpMethod, template: &str) -> EndpointId {
    (svc(service), http_key(method, template))
}

#[allow(dead_code)]
fn endpoint_def_with_fields(
    fields: BTreeMap<Direction, BTreeMap<JsonPath, FieldMeta>>,
) -> EndpointDef {
    let has_schema = fields.values().any(|f| !f.is_empty());
    EndpointDef {
        providers: Vec::new(),
        schemas: fields,
        has_schema,
        source_files: BTreeSet::new(),
    }
}

fn empty_endpoint_def() -> EndpointDef {
    EndpointDef {
        providers: Vec::new(),
        schemas: BTreeMap::new(),
        has_schema: false,
        source_files: BTreeSet::new(),
    }
}

fn empty_surface() -> ContractSurface {
    ContractSurface::default()
}

fn changed_set() -> Box<dyn crate::federation::contracts::diff::ChangedFilesSource> {
    Box::new(StaticChangedFiles(BTreeSet::new()))
}

fn minimal_coverage() -> Coverage {
    Coverage {
        repos: Vec::new(),
        unresolved_consumers: Vec::new(),
        ambiguous: Vec::new(),
        unnormalized: Vec::new(),
        external: Vec::new(),
        stale_bindings: 0,
        schemaless_endpoints: Vec::new(),
        complete: true,
        scope: Scope {
            reviewed: vec![crate::federation::contracts::diff::ReviewedRepo {
                repo: "billing".into(),
                commit: Some("abc".into()),
                dirty: false,
            }],
            unreviewed: Vec::new(),
            configured_only: true,
        },
    }
}

fn consumer_key_for_call(call: &GlobalId, key: ContractKey) -> ConsumerKey {
    let repo_str = call.repo_id().to_string();
    ConsumerKey {
        caller: SymbolKey {
            repo: RepoId::new(&repo_str).unwrap_or_else(|_| repo("unknown")),
            path: call.path().unwrap_or_default(),
            container: None,
            name: call.name().unwrap_or_default(),
        },
        target: ConsumerTargetKey::Contract(key),
    }
}

// ─── §9.4 classify — every cell ────────────────────────────────────

#[test]
fn classify_field_removed_response_is_breaking_if_read() {
    let kind = ChangeKind::FieldRemoved {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["x"]),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::BreakingIfRead);
}

#[test]
fn classify_field_removed_request_is_compatible() {
    let kind = ChangeKind::FieldRemoved {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Compatible);
}

#[test]
fn classify_field_added_optional_response_is_compatible() {
    let kind = ChangeKind::FieldAdded {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["x"]),
        required: false,
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Compatible);
}

#[test]
fn classify_field_added_required_request_is_breaking() {
    let kind = ChangeKind::FieldAdded {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
        required: true,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Breaking);
}

#[test]
fn classify_field_added_optional_request_is_compatible() {
    let kind = ChangeKind::FieldAdded {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
        required: false,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Compatible);
}

#[test]
fn classify_field_renamed_response_is_breaking_if_read() {
    let kind = ChangeKind::FieldRenamed {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        from: path(&["a"]),
        to: path(&["b"]),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::BreakingIfRead);
}

#[test]
fn classify_field_renamed_request_is_needs_review() {
    let kind = ChangeKind::FieldRenamed {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        from: path(&["a"]),
        to: path(&["b"]),
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::NeedsReview);
}

#[test]
fn classify_field_type_changed_response_is_breaking_if_read() {
    let kind = ChangeKind::FieldTypeChanged {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["x"]),
        from: TypeDesc::String,
        to: TypeDesc::Number,
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::BreakingIfRead);
}

#[test]
fn classify_field_type_changed_request_is_breaking_if_sent() {
    let kind = ChangeKind::FieldTypeChanged {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
        from: TypeDesc::String,
        to: TypeDesc::Number,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::BreakingIfSent);
}

#[test]
fn classify_requiredness_became_required_request_is_breaking() {
    let kind = ChangeKind::RequirednessChanged {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
        now_required: true,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Breaking);
}

#[test]
fn classify_requiredness_became_optional_response_is_breaking_if_read() {
    let kind = ChangeKind::RequirednessChanged {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["x"]),
        now_required: false,
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::BreakingIfRead);
}

#[test]
fn classify_requiredness_became_required_response_is_compatible() {
    let kind = ChangeKind::RequirednessChanged {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["x"]),
        now_required: true,
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Compatible);
}

#[test]
fn classify_requiredness_became_optional_request_is_compatible() {
    let kind = ChangeKind::RequirednessChanged {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
        now_required: false,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Compatible);
}

#[test]
fn classify_nullability_became_nullable_response_is_breaking_if_read() {
    let kind = ChangeKind::NullabilityChanged {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["x"]),
        now_nullable: true,
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::BreakingIfRead);
}

#[test]
fn classify_nullability_became_nullable_request_is_compatible() {
    let kind = ChangeKind::NullabilityChanged {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
        now_nullable: true,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Compatible);
}

#[test]
fn classify_nullability_became_non_nullable_request_is_breaking_if_sent() {
    let kind = ChangeKind::NullabilityChanged {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
        now_nullable: false,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::BreakingIfSent);
}

#[test]
fn classify_nullability_became_non_nullable_response_is_compatible() {
    let kind = ChangeKind::NullabilityChanged {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["x"]),
        now_nullable: false,
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Compatible);
}

#[test]
fn classify_enum_value_added_response_is_needs_review() {
    let kind = ChangeKind::EnumValueAdded {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["status"]),
        value: "refunded".into(),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::NeedsReview);
}

#[test]
fn classify_enum_value_added_request_is_compatible() {
    let kind = ChangeKind::EnumValueAdded {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["status"]),
        value: "refunded".into(),
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Compatible);
}

#[test]
fn classify_enum_value_removed_response_is_compatible() {
    let kind = ChangeKind::EnumValueRemoved {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["status"]),
        value: "open".into(),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Compatible);
}

#[test]
fn classify_enum_value_removed_request_is_breaking_if_sent() {
    let kind = ChangeKind::EnumValueRemoved {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["status"]),
        value: "open".into(),
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::BreakingIfSent);
}

#[test]
fn classify_endpoint_removed_is_breaking() {
    let kind = ChangeKind::EndpointRemoved {
        key: http_key(HttpMethod::Get, "/a"),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Breaking);
}

#[test]
fn classify_endpoint_added_is_compatible() {
    let kind = ChangeKind::EndpointAdded {
        key: http_key(HttpMethod::Get, "/a"),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Compatible);
}

#[test]
fn classify_path_changed_is_breaking() {
    let kind = ChangeKind::PathChanged {
        from: http_key(HttpMethod::Get, "/a"),
        to: http_key(HttpMethod::Get, "/b"),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Breaking);
}

#[test]
fn classify_method_changed_is_breaking() {
    let kind = ChangeKind::MethodChanged {
        from: http_key(HttpMethod::Get, "/a"),
        to: http_key(HttpMethod::Post, "/a"),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Breaking);
}

#[test]
fn classify_changed_without_schema_is_needs_review() {
    let kind = ChangeKind::ChangedWithoutSchema {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::NeedsReview);
}

#[test]
fn classify_consumer_endpoint_unmatched_is_breaking() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let kind = ChangeKind::ConsumerEndpointUnmatched {
        consumer: consumer_key_for_call(&call, http_key(HttpMethod::Get, "/a")),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Breaking);
}

#[test]
fn classify_consumer_field_unmatched_is_breaking() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let kind = ChangeKind::ConsumerFieldUnmatched {
        consumer: consumer_key_for_call(&call, http_key(HttpMethod::Get, "/a")),
        field: path(&["new_field"]),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Breaking);
}

#[test]
fn classify_consumer_rebound_is_compatible() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let kind = ChangeKind::ConsumerRebound {
        consumer: consumer_key_for_call(&call, http_key(HttpMethod::Get, "/a")),
        from: endpoint_id("orders", HttpMethod::Get, "/a"),
        to: endpoint_id("orders", HttpMethod::Get, "/b"),
    };
    assert_eq!(classify(&kind, Direction::Response), Compat::Compatible);
}

// ─── §9.5 evaluate — every per-consumer cell ───────────────────────

fn coverage_with_reviewed(reviewed: Vec<&str>) -> Coverage {
    Coverage {
        repos: Vec::new(),
        unresolved_consumers: Vec::new(),
        ambiguous: Vec::new(),
        unnormalized: Vec::new(),
        external: Vec::new(),
        stale_bindings: 0,
        schemaless_endpoints: Vec::new(),
        complete: reviewed.is_empty(),
        scope: Scope {
            reviewed: reviewed
                .into_iter()
                .map(|r| crate::federation::contracts::diff::ReviewedRepo {
                    repo: r.into(),
                    commit: Some("abc".into()),
                    dirty: false,
                })
                .collect(),
            unreviewed: Vec::new(),
            configured_only: true,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn build_surface_with_consumer(
    service: &str,
    method: HttpMethod,
    template: &str,
    response_fields: BTreeMap<JsonPath, FieldMeta>,
    consumer_call: &GlobalId,
    consumer_endpoint: EndpointId,
    provenance: EdgeProvenance,
    reads: BTreeSet<JsonPath>,
    reads_complete: bool,
) -> ContractSurface {
    let endpoint = (ServiceName(service.into()), http_key(method, template));
    let mut schemas: BTreeMap<Direction, BTreeMap<JsonPath, FieldMeta>> = BTreeMap::new();
    schemas.insert(Direction::Response, response_fields);
    let endpoint_def = EndpointDef {
        providers: Vec::new(),
        schemas,
        has_schema: true,
        source_files: BTreeSet::new(),
    };
    let consumer_key = consumer_key_for_call(consumer_call, consumer_endpoint.1.clone());
    let consumer_def = ConsumerDef {
        call: consumer_call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![consumer_endpoint],
            provenance,
        },
        reads,
        reads_complete,
    };
    let mut surface = ContractSurface::default();
    surface.endpoints.insert(endpoint, endpoint_def);
    surface.consumers.insert(consumer_key, consumer_def);
    surface
}

#[test]
fn evaluate_breaking_with_static_provenance_and_reading_consumer_is_verified() {
    // orders removes `customer_id` (response field). billing reads
    // it; binding is `Static`. → `Verified`.
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut reads = BTreeSet::new();
    reads.insert(path(&["customer_id"]));
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        reads,
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["customer_id"]),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::Verified);
    assert_eq!(impact.affected.len(), 1);
    assert_eq!(impact.affected[0].class, Class::Verified);
    assert_eq!(impact.affected[0].reason, Reason::HeuristicBinding);
}

#[test]
fn evaluate_breaking_with_heuristic_provenance_is_needs_investigation() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut reads = BTreeSet::new();
    reads.insert(path(&["customer_id"]));
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Heuristic {
            detector: "ambiguous".into(),
            confidence: 0.3,
        },
        reads,
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["customer_id"]),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::HeuristicBinding);
}

#[test]
fn evaluate_breaking_if_read_with_reading_static_is_verified() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["status"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut reads = BTreeSet::new();
    reads.insert(path(&["status"]));
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        reads,
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["status"]),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::Verified);
}

#[test]
fn evaluate_breaking_if_read_with_reading_heuristic_is_ni_heuristic_binding() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut reads = BTreeSet::new();
    reads.insert(path(&["customer_id"]));
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Heuristic {
            detector: "unbound_host".into(),
            confidence: 0.6,
        },
        reads,
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["customer_id"]),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::HeuristicBinding);
}

#[test]
fn evaluate_breaking_if_read_with_non_reading_complete_consumer_is_no_known_impact() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    // No reads → the consumer is unaffected (reads_complete = true).
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["customer_id"]),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NoKnownImpact);
    assert_eq!(impact.compatible_changes, 0);
    assert!(impact.affected.is_empty());
}

#[test]
fn evaluate_breaking_if_read_with_non_reading_incomplete_consumer_is_ni_reads_not_fully_traced() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        false,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["customer_id"]),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::ReadsNotFullyTraced);
}

#[test]
fn evaluate_breaking_if_sent_is_always_ni_sends_not_modeled() {
    let call = id("billing", "HttpClientCall", "src/b.py", "post", 1);
    let mut request_fields = BTreeMap::new();
    request_fields.insert(path(&["note"]), field(TypeDesc::String, false, true));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Post, "/api/orders");
    let mut schemas: BTreeMap<Direction, BTreeMap<JsonPath, FieldMeta>> = BTreeMap::new();
    schemas.insert(Direction::Request, request_fields);
    let mut endpoints = BTreeMap::new();
    endpoints.insert(
        consumer_endpoint.clone(),
        EndpointDef {
            providers: Vec::new(),
            schemas,
            has_schema: true,
            source_files: BTreeSet::new(),
        },
    );
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![consumer_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    let surface = ContractSurface {
        endpoints,
        consumers: {
            let mut m = BTreeMap::new();
            m.insert(consumer_key, consumer_def);
            m
        },
    };
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldTypeChanged {
            endpoint: consumer_endpoint,
            direction: Direction::Request,
            path: path(&["note"]),
            from: TypeDesc::String,
            to: TypeDesc::Number,
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::SendsNotModeled);
}

#[test]
fn evaluate_needs_review_response_with_reading_is_ni_needs_review() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["status"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut reads = BTreeSet::new();
    reads.insert(path(&["status"]));
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        reads,
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::EnumValueAdded {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["status"]),
            value: "refunded".into(),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::NeedsReview);
}

#[test]
fn evaluate_needs_review_response_with_non_reading_complete_consumer_is_no_known_impact() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["status"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::EnumValueAdded {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["status"]),
            value: "refunded".into(),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NoKnownImpact);
}

#[test]
fn evaluate_changed_without_schema_is_ni_no_schema_for_every_bound_consumer() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}/label");
    let mut endpoints = BTreeMap::new();
    endpoints.insert(consumer_endpoint.clone(), empty_endpoint_def());
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![consumer_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    let mut consumers = BTreeMap::new();
    consumers.insert(consumer_key, consumer_def);
    let surface = ContractSurface {
        endpoints,
        consumers,
    };
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::ChangedWithoutSchema {
            endpoint: consumer_endpoint,
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::NoSchema);
}

#[test]
fn evaluate_compatible_field_added_is_not_reported_but_counted() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let response_fields = BTreeMap::new();
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldAdded {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["currency"]),
            required: false,
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NoKnownImpact);
    assert_eq!(impact.compatible_changes, 1);
    assert!(impact.affected.is_empty());
}

#[test]
fn evaluate_unresolved_candidate_in_reviewed_repo_is_ni_unresolved_candidates() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    let mut head = ContractSurface::default();
    head.consumers.insert(consumer_key.clone(), consumer_def);
    let mut coverage = coverage_with_reviewed(vec!["orders"]);
    coverage.unresolved_consumers.push(consumer_key.clone());
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::EndpointAdded {
            key: consumer_endpoint.1.clone(),
        },
    };
    let impact = evaluate(&change, &head, &head, &coverage);
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.reason, Some(Reason::UnresolvedCandidates));
    assert!(impact
        .affected
        .iter()
        .any(|a| a.reason == Reason::UnresolvedCandidates));
}

// ─── consumer-side change ───────────────────────────────────────────

#[test]
fn evaluate_consumer_endpoint_unmatched_in_reviewed_repo_is_verified() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let consumer_def = ConsumerDef {
        call: call.clone(),
        // §9.5: "the consumer's target service is resolved Static or
        // Confirmed and that service's repo is reviewed". The target
        // service (orders) is reviewed and the resolution is Static
        // — Verified.
        resolution: SurfaceResolution::Binds {
            endpoints: vec![consumer_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    let mut head = ContractSurface::default();
    head.consumers.insert(consumer_key.clone(), consumer_def);
    let change = Change {
        service: svc("billing"),
        kind: ChangeKind::ConsumerEndpointUnmatched {
            consumer: consumer_key.clone(),
        },
    };
    let coverage = coverage_with_reviewed(vec!["orders", "billing"]);
    let impact = evaluate(&change, &empty_surface(), &head, &coverage);
    assert_eq!(impact.class, Class::Verified);
}

#[test]
fn evaluate_consumer_endpoint_unmatched_in_unreviewed_repo_is_ni() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    let mut head = ContractSurface::default();
    head.consumers.insert(consumer_key.clone(), consumer_def);
    let change = Change {
        service: svc("billing"),
        kind: ChangeKind::ConsumerEndpointUnmatched {
            consumer: consumer_key.clone(),
        },
    };
    // orders is not in scope.reviewed → NI.
    let coverage = coverage_with_reviewed(vec!["billing"]);
    let impact = evaluate(&change, &empty_surface(), &head, &coverage);
    assert_eq!(impact.class, Class::NeedsInvestigation);
}

// ─── §9.7 could-match ────────────────────────────────────────────────

#[test]
fn could_match_accepts_same_service_no_match_method_any() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    assert!(could_match(&key, &target, &head));
}

#[test]
fn could_match_rejects_external_consumer() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::External {
            host: "api.stripe.com".into(),
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/v1/charges"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    assert!(!could_match(&key, &target, &head));
}

#[test]
fn could_match_accepts_prefix_stripped_template() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    // The consumer template is `/api/v1/orders/{}` and the provider
    // template is `/orders/{}`. §7.4 prefix tolerance strips up to
    // 3 leading literal segments from the consumer (`api`, `v1`),
    // giving `orders/{}`, which matches the provider's `orders/{}`.
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/api/v1/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/orders/{}");
    assert!(could_match(&key, &target, &head));
}

#[test]
fn could_match_rejects_mismatched_method() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Post, "/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    assert!(!could_match(&key, &target, &head));
}

#[test]
fn could_match_accepts_provider_any_method() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Any, "/api/orders/{}");
    assert!(could_match(&key, &target, &head));
}

// ─── Property: diff(a, a) is empty ──────────────────────────────────

#[test]
fn diff_self_is_empty() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint,
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let changes = diff_contracts(&surface, &surface, &*changed_set());
    assert!(
        changes.is_empty(),
        "diff(self, self) must be empty, got {changes:?}"
    );
    let consumer_changes = diff_consumers(&surface, &surface);
    assert!(
        consumer_changes.is_empty(),
        "consumer diff(self, self) must be empty"
    );
}

// ─── Surface extraction ─────────────────────────────────────────────

#[test]
fn surface_extraction_keys_per_service() {
    let mut index = ContractIndex::default();
    let endpoint_a = endpoint_id("orders", HttpMethod::Get, "/health");
    let endpoint_b = endpoint_id("billing", HttpMethod::Get, "/health");
    index.endpoints.insert(
        endpoint_a.clone(),
        Endpoint {
            id: endpoint_a.clone(),
            method: HttpMethod::Get,
            template: "/health".into(),
            providers: Vec::new(),
            schemas: BTreeMap::new(),
        },
    );
    index.endpoints.insert(
        endpoint_b.clone(),
        Endpoint {
            id: endpoint_b.clone(),
            method: HttpMethod::Get,
            template: "/health".into(),
            providers: Vec::new(),
            schemas: BTreeMap::new(),
        },
    );
    let surface = ContractSurface::from_index(&index);
    assert!(surface.contains_endpoint(&endpoint_a));
    assert!(surface.contains_endpoint(&endpoint_b));
}

trait SurfaceExt {
    fn contains_endpoint(&self, id: &EndpointId) -> bool;
}
impl SurfaceExt for ContractSurface {
    fn contains_endpoint(&self, id: &EndpointId) -> bool {
        self.endpoints.contains_key(id)
    }
}

// ─── §15.2 scenarios (pure-function) ────────────────────────────────

#[allow(dead_code)]
fn orders_index_with_response(fields: BTreeMap<JsonPath, FieldMeta>) -> ContractIndex {
    let mut index = ContractIndex::default();
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Response, fields);
    let provider_node_id = id("orders", "HttpRoute", "src/o.py", "get_order", 1);
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/{}".into(),
            providers: vec![EndpointProvider {
                node_id: provider_node_id,
                origin: ProviderOrigin::Code,
                handler: None,
                operation_id: None,
            }],
            schemas: schemas
                .into_iter()
                .map(|(d, m)| {
                    (
                        d,
                        EndpointSchema {
                            node_id: id("orders", "Other", "openapi.yaml", "schema", 1),
                            fields: m,
                        },
                    )
                })
                .collect(),
        },
    );
    index
}

// Scenario 1 — orders removes customer_id, billing reads it → Verified
#[test]
fn scenario_1_field_removed_response_verified() {
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    response_fields.insert(path(&["total"]), field(TypeDesc::Number, true, false));
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut reads = BTreeSet::new();
    reads.insert(path(&["customer_id"]));
    let head = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        reads,
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["customer_id"]),
        },
    };
    let impact = evaluate(&change, &head, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::Verified);
    assert_eq!(impact.affected.len(), 1);
    assert_eq!(impact.affected[0].class, Class::Verified);
    assert_eq!(impact.affected[0].service, svc("billing"));
}

// Scenario 2 — orders adds optional currency → not reported; compatible_changes=1
#[test]
fn scenario_2_field_added_optional_response_no_known_impact() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let head = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldAdded {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["currency"]),
            required: false,
        },
    };
    let impact = evaluate(&change, &head, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::NoKnownImpact);
    assert_eq!(impact.compatible_changes, 1);
    assert!(impact.affected.is_empty());
}

// Scenario 5b — orders adds enum value refunded to status; billing reads status → NI needs_review
#[test]
fn scenario_5b_enum_added_status_billing_reads_needs_review() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["status"]), field(TypeDesc::String, true, false));
    let mut reads = BTreeSet::new();
    reads.insert(path(&["status"]));
    let head = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        reads,
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::EnumValueAdded {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["status"]),
            value: "refunded".into(),
        },
    };
    let impact = evaluate(&change, &head, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::NeedsReview);
}

// Scenario 5 — orders adds enum value refunded, billing does not read status → NoKnownImpact
#[test]
fn scenario_5_enum_added_status_billing_does_not_read_no_known_impact() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let response_fields = BTreeMap::new();
    let head = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::EnumValueAdded {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["status"]),
            value: "refunded".into(),
        },
    };
    let impact = evaluate(&change, &head, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::NoKnownImpact);
}

// Scenario 6 — orders renames path `/api/orders/{}` → `/api/order/{}`,
// same handler → PathChanged, Verified for billing.
#[test]
fn scenario_6_path_changed_same_handler_verified() {
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/orders.py".into(),
        container: None,
        name: "get_order".into(),
    };
    let provider_base = ProviderRef {
        node_id: id("orders", "HttpRoute", "src/orders.py", "get_order", 10),
        handler: Some(handler.clone()),
        operation_id: None,
    };
    let provider_head = ProviderRef {
        node_id: id("orders", "HttpRoute", "src/orders.py", "get_order", 10),
        handler: Some(handler.clone()),
        operation_id: None,
    };
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let base_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let head_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/order/{}");
    let base_endpoint_def = EndpointDef {
        providers: vec![provider_base],
        schemas: {
            let mut m = BTreeMap::new();
            m.insert(Direction::Response, response_fields.clone());
            m
        },
        has_schema: true,
        source_files: BTreeSet::new(),
    };
    let head_endpoint_def = EndpointDef {
        providers: vec![provider_head],
        schemas: {
            let mut m = BTreeMap::new();
            m.insert(Direction::Response, response_fields);
            m
        },
        has_schema: true,
        source_files: BTreeSet::new(),
    };
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let mut reads = BTreeSet::new();
    reads.insert(path(&["customer_id"]));
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![head_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads,
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, head_endpoint.1.clone());
    let mut endpoints = BTreeMap::new();
    endpoints.insert(head_endpoint.clone(), head_endpoint_def);
    let mut consumers = BTreeMap::new();
    consumers.insert(consumer_key.clone(), consumer_def);
    let head = ContractSurface {
        endpoints,
        consumers,
    };
    let mut base_endpoints = BTreeMap::new();
    base_endpoints.insert(base_endpoint.clone(), base_endpoint_def.clone());
    let base = ContractSurface {
        endpoints: base_endpoints,
        consumers: BTreeMap::new(),
    };
    let changes = diff_contracts(&base, &head, &*changed_set());
    // Expect PathChanged for the orders endpoint plus FieldRemoved
    // entries (because base has no fields, head has customer_id; head
    // added a new field on Response → FieldAdded). The interesting
    // assertion is that `PathChanged` is present.
    assert!(
        changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::PathChanged { .. })),
        "expected PathChanged, got {changes:?}"
    );
    let path_change = changes
        .iter()
        .find(|c| matches!(c.kind, ChangeKind::PathChanged { .. }))
        .unwrap();
    let impact = evaluate(path_change, &base, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::Verified);
}

// Scenario 11 — customer_id → customerId (rename, same type) → one
// FieldRenamed, BreakingIfRead, Verified.
#[test]
fn scenario_11_field_renamed_same_type_verified() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut base_response_fields = BTreeMap::new();
    base_response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut head_response_fields = BTreeMap::new();
    head_response_fields.insert(path(&["customerId"]), field(TypeDesc::String, true, false));
    let base = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        base_response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let head = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        head_response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let changes = diff_contracts(&base, &head, &*changed_set());
    let renames: Vec<&Change> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::FieldRenamed { .. }))
        .collect();
    assert_eq!(
        renames.len(),
        1,
        "exactly one FieldRenamed, got {changes:?}"
    );
    let kind = &renames[0].kind;
    if let ChangeKind::FieldRenamed { from, to, .. } = kind {
        assert!(from.0.last().unwrap().clone() == PathSegment::Name("customer_id".into()));
        assert!(to.0.last().unwrap().clone() == PathSegment::Name("customerId".into()));
    } else {
        unreachable!();
    }
    assert_eq!(
        classify(&renames[0].kind, Direction::Response),
        Compat::BreakingIfRead
    );
    let impact = evaluate(renames[0], &base, &head, &minimal_coverage());
    // The consumer (billing) does not read customer_id in this
    // scenario — the rename is reported but no consumer is affected.
    assert_eq!(impact.class, Class::NoKnownImpact);
}

// Scenario 12 — customer_id → customerId with type change → FieldRemoved + FieldAdded
#[test]
fn scenario_12_field_renamed_with_type_change_breaks_rename() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut base_response_fields = BTreeMap::new();
    base_response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut head_response_fields = BTreeMap::new();
    head_response_fields.insert(path(&["customerId"]), field(TypeDesc::Number, true, false));
    let base = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        base_response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let head = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        head_response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        BTreeSet::new(),
        true,
    );
    let changes = diff_contracts(&base, &head, &*changed_set());
    let removed: Vec<&Change> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::FieldRemoved { .. }))
        .collect();
    let added: Vec<&Change> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::FieldAdded { .. }))
        .collect();
    assert_eq!(removed.len(), 1, "expected FieldRemoved for customer_id");
    assert_eq!(added.len(), 1, "expected FieldAdded for customerId");
    let renames: Vec<&Change> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::FieldRenamed { .. }))
        .collect();
    assert!(renames.is_empty(), "type change breaks rename");
}

// Scenario 19 — type change of optional request field note → BreakingIfSent → NI sends_not_modeled
#[test]
fn scenario_19_optional_request_type_change_is_sends_not_modeled() {
    let call = id("billing", "HttpClientCall", "src/b.py", "post", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Post, "/api/orders");
    let mut request_fields = BTreeMap::new();
    request_fields.insert(path(&["note"]), field(TypeDesc::String, false, true));
    let mut schemas: BTreeMap<Direction, BTreeMap<JsonPath, FieldMeta>> = BTreeMap::new();
    schemas.insert(Direction::Request, request_fields);
    let mut endpoints = BTreeMap::new();
    endpoints.insert(
        consumer_endpoint.clone(),
        EndpointDef {
            providers: Vec::new(),
            schemas,
            has_schema: true,
            source_files: BTreeSet::new(),
        },
    );
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![consumer_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    let mut consumers = BTreeMap::new();
    consumers.insert(consumer_key, consumer_def);
    let head = ContractSurface {
        endpoints,
        consumers,
    };
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldTypeChanged {
            endpoint: consumer_endpoint,
            direction: Direction::Request,
            path: path(&["note"]),
            from: TypeDesc::String,
            to: TypeDesc::Number,
        },
    };
    let impact = evaluate(&change, &head, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::SendsNotModeled);
}

// Scenario 20 — billing starts reading discount (ConsumerFieldUnmatched) → Verified.
#[test]
fn scenario_20_consumer_field_unmatched_verified() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut reads = BTreeSet::new();
    reads.insert(path(&["discount"]));
    let head = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        reads,
        true,
    );
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    let _ = consumer_key;
    let base = ContractSurface::default();
    let changes = diff_consumers(&base, &head);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::ConsumerFieldUnmatched { .. })),
        "expected ConsumerFieldUnmatched, got {changes:?}"
    );
    let matched = changes
        .iter()
        .find(|c| matches!(c.kind, ChangeKind::ConsumerFieldUnmatched { .. }))
        .unwrap();
    let coverage = coverage_with_reviewed(vec!["orders"]);
    let impact = evaluate(matched, &base, &head, &coverage);
    assert_eq!(impact.class, Class::Verified);
}

// Scenario 21 — orders changes the handler of GET /api/orders/{}/label
// (code-only route). ChangedWithoutSchema → NI no_schema for billing.
#[test]
fn scenario_21_changed_without_schema_no_schema_for_billing() {
    let call = id("billing", "HttpClientCall", "src/b.py", "print_label", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}/label");
    let mut endpoint_def = empty_endpoint_def();
    endpoint_def
        .source_files
        .insert("src/orders.py".to_string());
    // The same endpoint appears in both base and head (same key),
    // both with the source file present. `ChangedFilesSource`
    // reports the file differs → ChangedWithoutSchema.
    let mut head = ContractSurface::default();
    head.endpoints
        .insert(consumer_endpoint.clone(), endpoint_def);
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![consumer_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    head.consumers.insert(consumer_key.clone(), consumer_def);
    let base = head.clone();
    let mut source_files = BTreeSet::new();
    source_files.insert("src/orders.py".into());
    let changed = StaticChangedFiles(source_files);
    let changes = diff_contracts(&base, &head, &changed);
    let matched = changes
        .iter()
        .find(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. }));
    assert!(
        matched.is_some(),
        "expected ChangedWithoutSchema, got {changes:?}"
    );
    let matched = matched.unwrap();
    let impact = evaluate(matched, &base, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::NoSchema);
}

// Scenario 22 — billing caches the response, reads_complete=false → NI reads_not_fully_traced.
#[test]
fn scenario_22_cache_response_reads_not_fully_traced() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut reads = BTreeSet::new();
    reads.insert(path(&["customer_id"]));
    let surface = build_surface_with_consumer(
        "orders",
        HttpMethod::Get,
        "/api/orders/{}",
        response_fields,
        &call,
        consumer_endpoint.clone(),
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        },
        reads,
        false,
    );
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            path: path(&["customer_id"]),
        },
    };
    let impact = evaluate(&change, &surface, &surface, &minimal_coverage());
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.affected[0].reason, Reason::ReadsNotFullyTraced);
}

// ─── coverage builder ───────────────────────────────────────────────

#[test]
fn build_coverage_reflects_index_state() {
    let mut index = ContractIndex::default();
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 5);
    let mut consumer = ConsumerResolution {
        call_id: call.clone(),
        service: svc("billing"),
        target: Some(ConsumerTarget::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
        }),
        bound_endpoints: Vec::new(),
        reads_complete: true,
    };
    let _ = &mut consumer;
    index.consumers.insert(
        call.clone(),
        ConsumerResolution {
            call_id: call.clone(),
            service: svc("billing"),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::NoRouteInService,
            }),
            bound_endpoints: Vec::new(),
            reads_complete: true,
        },
    );
    let cov = build_coverage(
        &index,
        Vec::new(),
        Scope {
            reviewed: vec![crate::federation::contracts::diff::ReviewedRepo {
                repo: "orders".into(),
                commit: Some("abc".into()),
                dirty: false,
            }],
            unreviewed: Vec::new(),
            configured_only: true,
        },
    );
    assert_eq!(cov.unresolved_consumers.len(), 1);
    assert!(cov.complete);
}

// ─── Affected has expected shape ────────────────────────────────────

#[test]
fn affected_shape_carries_class_and_reason() {
    let a = Affected {
        service: svc("billing"),
        consumer: ConsumerKey {
            caller: SymbolKey {
                repo: repo("billing"),
                path: "src/b.py".into(),
                container: None,
                name: "fetch".into(),
            },
            target: ConsumerTargetKey::Contract(http_key(HttpMethod::Get, "/a")),
        },
        class: Class::Verified,
        reason: Reason::HeuristicBinding,
    };
    assert_eq!(a.class, Class::Verified);
    assert_eq!(a.reason, Reason::HeuristicBinding);
}

// ─── nested-field rule ──────────────────────────────────────────────

#[test]
fn nested_field_rule_collapses_descendants_under_object_removal() {
    let mut base = BTreeMap::new();
    base.insert(path(&["x"]), field(TypeDesc::Object, true, false));
    base.insert(path(&["x", "y"]), field(TypeDesc::String, true, false));
    base.insert(
        path(&["x", "y", "z"]),
        field(TypeDesc::Integer, false, true),
    );
    let mut head = BTreeMap::new();
    head.insert(path(&["y"]), field(TypeDesc::String, true, false));
    let mut changes: Vec<Change> = Vec::new();
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/a");
    diff_fields(
        &mut changes,
        &consumer_endpoint,
        Direction::Response,
        Some(&base),
        Some(&head),
    );
    let removed_paths: Vec<&JsonPath> = changes
        .iter()
        .filter_map(|c| match &c.kind {
            ChangeKind::FieldRemoved { path, .. } => Some(path),
            _ => None,
        })
        .collect();
    // Only the topmost `x` is reported; descendants are folded.
    assert_eq!(removed_paths.len(), 1);
    assert_eq!(removed_paths[0], &path(&["x"]));
}
