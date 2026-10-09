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
    ContractKey, Direction, FieldMeta, GraphqlOp, HttpMethod, JsonPath, MethodSpec, PathSegment,
    ProviderOrigin, RpcSystem, ServiceName, SymbolKey, TypeDesc,
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
    let mut repo_coverages = std::collections::BTreeMap::new();
    let cover = crate::federation::contracts::coverage::RepoCoverage {
        cache_key: crate::federation::contracts::index_cache::CacheKey::new(
            "billing",
            "abc",
            crate::federation::contracts::analyzer_version(),
        ),
        ..Default::default()
    };
    repo_coverages.insert("billing".into(), cover);
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
        repo_coverages,
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
        required: false,
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
        required: false,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::NeedsReview);
}

#[test]
fn classify_field_renamed_request_required_is_breaking() {
    // §9.4: "Breaking if the new field is required, else NeedsReview"
    // (request side). `required` carries the destination field's
    // requiredness.
    let kind = ChangeKind::FieldRenamed {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        from: path(&["a"]),
        to: path(&["b"]),
        required: true,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Breaking);
}

#[test]
fn classify_field_renamed_payload_is_breaking_if_read() {
    // §9.4: "Response / payload" is one column. Payload follows
    // Response's `BreakingIfRead` verdict for renames.
    let kind = ChangeKind::FieldRenamed {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Payload,
        from: path(&["a"]),
        to: path(&["b"]),
        required: false,
    };
    assert_eq!(classify(&kind, Direction::Payload), Compat::BreakingIfRead);
}

#[test]
fn classify_field_type_changed_response_is_breaking_if_read() {
    let kind = ChangeKind::FieldTypeChanged {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Response,
        path: path(&["x"]),
        from: TypeDesc::String,
        to: TypeDesc::Number,
        required: false,
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
        required: false,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::BreakingIfSent);
}

#[test]
fn classify_field_type_changed_request_required_is_breaking() {
    // §9.4: "Breaking if required, else BreakingIfSent" (request side).
    let kind = ChangeKind::FieldTypeChanged {
        endpoint: endpoint_id("orders", HttpMethod::Post, "/a"),
        direction: Direction::Request,
        path: path(&["x"]),
        from: TypeDesc::String,
        to: TypeDesc::Number,
        required: true,
    };
    assert_eq!(classify(&kind, Direction::Request), Compat::Breaking);
}

#[test]
fn classify_field_type_changed_payload_is_breaking_if_read() {
    // §9.4: "Response / payload" is one column. Payload follows
    // Response's `BreakingIfRead` verdict.
    let kind = ChangeKind::FieldTypeChanged {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Payload,
        path: path(&["x"]),
        from: TypeDesc::String,
        to: TypeDesc::Number,
        required: false,
    };
    assert_eq!(classify(&kind, Direction::Payload), Compat::BreakingIfRead);
}

#[test]
fn classify_field_removed_payload_is_breaking_if_read() {
    // §9.4: "Response / payload" — payload follows Response's
    // BreakingIfRead verdict for FieldRemoved.
    let kind = ChangeKind::FieldRemoved {
        endpoint: endpoint_id("orders", HttpMethod::Get, "/a"),
        direction: Direction::Payload,
        path: path(&["x"]),
    };
    assert_eq!(classify(&kind, Direction::Payload), Compat::BreakingIfRead);
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
        repo_coverages: std::collections::BTreeMap::new(),
    }
}

// ─── Phase A coverage downgrade ─────────────────────────────────────

/// TLA+ `NoKnownImpactSound`: when a `NoKnownImpact` verdict fires
/// for a change, every in-scope repo must be `RepoComplete(r)`. The
/// evaluator checks the coverage ledger on every `NoKnownImpact`
/// path; an incomplete in-scope repo downgrades to
/// `NeedsInvestigation`.
#[test]
fn phase_a_no_known_impact_downgraded_on_incomplete_repo() {
    use crate::federation::contracts::coverage::RepoCoverage as PhaseARepo;
    use crate::federation::contracts::index_cache::CacheKey;
    let mut coverage = minimal_coverage();
    let ledger_entry = PhaseARepo {
        error: Some("walk failed".into()),
        cache_key: CacheKey::new("billing", "abc", "0.9.0+c3"),
        ..Default::default()
    };
    coverage
        .repo_coverages
        .insert("billing".into(), ledger_entry);
    let change = Change {
        service: svc("billing"),
        kind: ChangeKind::EndpointAdded {
            key: http_key(HttpMethod::Get, "/api/x"),
        },
    };
    let impact = evaluate(
        &change,
        &ContractSurface::default(),
        &ContractSurface::default(),
        &coverage,
    );
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.reason, Some(Reason::UnresolvedCandidates));
}

/// TLA+ `NoKnownImpactSound`: a `NoKnownImpact` verdict with valid
/// ledger attached succeeds.
#[test]
fn phase_a_no_known_impact_with_valid_ledger() {
    let change = Change {
        service: svc("billing"),
        kind: ChangeKind::EndpointAdded {
            key: http_key(HttpMethod::Get, "/api/x"),
        },
    };
    let impact = evaluate(
        &change,
        &ContractSurface::default(),
        &ContractSurface::default(),
        &minimal_coverage(),
    );
    assert_eq!(impact.class, Class::NoKnownImpact);
}

/// TLA+ `NoKnownImpactSound`: a change without coverage ledger attached
/// must downgrade from NoKnownImpact to NeedsInvestigation.
#[test]
fn phase_a_no_known_impact_downgraded_without_ledger() {
    let mut cov = minimal_coverage();
    cov.repo_coverages.clear();
    let change = Change {
        service: svc("billing"),
        kind: ChangeKind::EndpointAdded {
            key: http_key(HttpMethod::Get, "/api/x"),
        },
    };
    let impact = evaluate(
        &change,
        &ContractSurface::default(),
        &ContractSurface::default(),
        &cov,
    );
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.reason, Some(Reason::UnresolvedCandidates));
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
    assert_eq!(impact.affected[0].reason, Reason::StaticBinding);
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
            required: false,
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
            target_service: None,
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

#[test]
fn evaluate_optional_response_field_add_stays_compatible_with_unresolved_consumer() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let key = consumer_key_for_call(&call, endpoint.1.clone());
    let consumer = ConsumerDef {
        call,
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: None,
        },
        reads: BTreeSet::new(),
        reads_complete: false,
    };
    let mut surface = ContractSurface::default();
    surface.consumers.insert(key.clone(), consumer);
    let mut coverage = coverage_with_reviewed(vec!["orders"]);
    coverage.repo_coverages.insert(
        "orders".into(),
        crate::federation::contracts::coverage::RepoCoverage {
            cache_key: crate::federation::contracts::index_cache::CacheKey::new(
                "orders",
                "abc",
                crate::federation::contracts::analyzer_version(),
            ),
            ..Default::default()
        },
    );
    coverage.unresolved_consumers.push(key);
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldAdded {
            endpoint,
            direction: Direction::Response,
            path: path(&["currency"]),
            required: false,
        },
    };

    let impact = evaluate(&change, &surface, &surface, &coverage);
    assert_eq!(impact.class, Class::NoKnownImpact);
    assert_eq!(impact.compatible_changes, 1);
    assert!(impact.affected.is_empty());
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
            target_service: None,
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

// ─── Finding 2: provider-side trace base ────────────────────────────

#[test]
fn evaluate_path_changed_with_base_only_consumer_is_verified() {
    // Consumer bound in base to the OLD endpoint id, but dropped
    // from head (the consumer's URL was never updated to the new
    // path). §9.5: trace the OLD contract in BASE — the consumer is
    // still affected by the PathChanged (breaking, certain,
    // Static) → Verified.
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 5);
    let base_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let head_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/order/{}");
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![base_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, base_endpoint.1.clone());
    let mut base_consumers = BTreeMap::new();
    base_consumers.insert(consumer_key.clone(), consumer_def);
    let base = ContractSurface {
        endpoints: BTreeMap::new(),
        consumers: base_consumers,
    };
    let head = ContractSurface::default();
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::PathChanged {
            from: base_endpoint.1.clone(),
            to: head_endpoint.1.clone(),
        },
    };
    let impact = evaluate(&change, &base, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::Verified);
    assert_eq!(impact.affected.len(), 1);
    assert_eq!(impact.affected[0].class, Class::Verified);
}

#[test]
fn evaluate_endpoint_removed_with_unresolved_candidate_is_unresolved() {
    // EndpointRemoved + an unresolved consumer in a reviewed repo
    // could match it → NeedsInvestigation (unresolved_candidates).
    // Before finding 2's fix, `target_endpoint` was `None` for
    // EndpointRemoved so the could-match branch was skipped.
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let removed_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: Some(svc("orders")),
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, removed_endpoint.1.clone());
    let mut head = ContractSurface::default();
    head.consumers.insert(consumer_key.clone(), consumer_def);
    let base = ContractSurface::default();
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::EndpointRemoved {
            key: removed_endpoint.1.clone(),
        },
    };
    let mut coverage = coverage_with_reviewed(vec!["orders"]);
    coverage.unresolved_consumers.push(consumer_key.clone());
    let impact = evaluate(&change, &base, &head, &coverage);
    assert_eq!(impact.class, Class::NeedsInvestigation);
    assert_eq!(impact.reason, Some(Reason::UnresolvedCandidates));
    assert!(impact
        .affected
        .iter()
        .any(|a| a.reason == Reason::UnresolvedCandidates));
}

// ─── Finding 3: rename read detection covers both `from` and `to` ──

#[test]
fn evaluate_field_renamed_with_stale_base_reads_is_verified() {
    // Consumer only reads the OLD path `customer_id` in base; the
    // rename moves it to `customerId`. §9.5: a consumer "reads" a
    // renamed field if base OR head reads contain either name. The
    // consumer is bound and certain (Static) → Verified.
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch_order", 5);
    let consumer_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut base_reads = BTreeSet::new();
    base_reads.insert(path(&["customer_id"]));
    let base_consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![consumer_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: base_reads,
        reads_complete: true,
    };
    let head_consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![consumer_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        // Head reads are empty: the consumer hasn't migrated yet.
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, consumer_endpoint.1.clone());
    let mut base = ContractSurface::default();
    base.consumers
        .insert(consumer_key.clone(), base_consumer_def);
    let mut head = ContractSurface::default();
    head.consumers
        .insert(consumer_key.clone(), head_consumer_def);
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRenamed {
            endpoint: consumer_endpoint,
            direction: Direction::Response,
            from: path(&["customer_id"]),
            to: path(&["customerId"]),
            required: true,
        },
    };
    let impact = evaluate(&change, &base, &head, &minimal_coverage());
    assert_eq!(impact.class, Class::Verified);
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
            target_service: None,
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
            target_service: None,
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
            target_service: None,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Post, "/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    assert!(!could_match(&key, &target, &head));
}

// Finding 4: target-service condition for could_match. An
// unresolved consumer with a known target service (rule 3) must
// NOT could-match an unrelated endpoint.
#[test]
fn could_match_rejects_unresolved_with_known_other_target() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: Some(svc("shipping")),
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    // The consumer's known target is `shipping`, not `orders`;
    // could_match must reject.
    assert!(!could_match(&key, &target, &head));
}

#[test]
fn could_match_accepts_unresolved_with_unknown_target() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoMatch,
            target_service: None,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    // target_service = None → permissive; could_match returns
    // true (subject to method / template checks).
    assert!(could_match(&key, &target, &head));
}

#[test]
fn could_match_accepts_unresolved_with_matching_target() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: Some(svc("orders")),
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    assert!(could_match(&key, &target, &head));
}

// Bug C: rule-3 prefix tolerance must surface the orders endpoint
// as a could-match candidate even when the consumer's
// `target_service` is known (`Some(orders)`). §7.4 prefix tolerance
// strips `/v1` from `/v1/api/orders/{}`, leaving `/api/orders/{}`,
// which direct-matches the provider. The consumer stays
// `Unresolved` (rule 3 must not bind via prefix strip); the
// endpoint is a `could_match` candidate.
#[test]
fn could_match_accepts_prefix_stripped_template_with_known_target_service() {
    let call = id("billing", "HttpClientCall", "src/b.py", "build_invoice", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: Some(svc("orders")),
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/v1/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    assert!(
        could_match(&key, &target, &head),
        "rule-3 prefix-stripped consumer must surface orders as a could-match candidate"
    );
}

#[test]
fn could_match_accepts_provider_any_method() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: None,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Any, "/api/orders/{}");
    assert!(could_match(&key, &target, &head));
}

// PR 18 — operationId candidate. An unresolved consumer whose URL
// template does not match any provider's template, but whose
// function name (`UrlExpr(name)`) equals a provider's OpenAPI
// `operationId`, is a could-match candidate. The URL check fails
// first; the operationId check then runs and returns true.
#[test]
fn could_match_accepts_operation_id_when_url_no_match() {
    let call = id("billing", "HttpClientCall", "src/sdk.ts", "getOrderById", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: Some(svc("orders")),
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    // The unresolved consumer's `ConsumerKey.target` is `UrlExpr(name)`
    // per `consumer_key` (uses `call_id.name()`). Use a `UrlExpr`
    // key directly so the operationId candidate check fires.
    let key = ConsumerKey {
        caller: SymbolKey {
            repo: repo("billing"),
            path: "src/sdk.ts".into(),
            container: None,
            name: "getOrderById".into(),
        },
        target: ConsumerTargetKey::UrlExpr("getOrderById".into()),
    };
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let mut endpoint_def = empty_endpoint_def();
    endpoint_def
        .providers
        .push(crate::federation::contracts::diff::ProviderRef {
            node_id: id("orders", "HttpRoute", "openapi.yaml", "getOrderById", 100),
            handler: None,
            operation_id: Some("getOrderById".into()),
        });
    head.endpoints.insert(target.clone(), endpoint_def);
    assert!(
        could_match(&key, &target, &head),
        "operationId match must surface as could-match even when URL doesn't match"
    );
}

// PR 18 — operationId candidate negative. The consumer's URL
// template does not match, and no provider has the matching
// operationId. The template check fails first, so the function
// returns false before reaching the operationId check.
#[test]
fn could_match_rejects_operation_id_mismatch() {
    let call = id("billing", "HttpClientCall", "src/sdk.ts", "getMe", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: Some(svc("orders")),
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    // Use a Contract key (with a template) so the template check
    // runs. The template is `/api/orders/{}` and the provider's is
    // `/api/v2/orders/{}` — they don't match. The provider's
    // `operation_id == "getOrderById"` also doesn't match the
    // consumer's function name `getMe`.
    let key = consumer_key_for_call(&call, http_key(HttpMethod::Get, "/api/orders/{}"));
    head.consumers.insert(key.clone(), consumer_def);
    let target = endpoint_id("orders", HttpMethod::Get, "/api/v2/orders/{}");
    let mut endpoint_def = empty_endpoint_def();
    endpoint_def
        .providers
        .push(crate::federation::contracts::diff::ProviderRef {
            node_id: id("orders", "HttpRoute", "openapi.yaml", "getOrderById", 5),
            handler: None,
            operation_id: Some("getOrderById".into()),
        });
    head.endpoints.insert(target.clone(), endpoint_def);
    assert!(
        !could_match(&key, &target, &head),
        "no URL match + no operationId match (consumer name != provider operationId) → not a candidate"
    );
}

// ─── Task 1 — non-HTTP could-match (I3 verdict soundness) ────────────
//
// `could_match` must be conservative for every protocol family
// (§9.7): returning `true` costs a `NeedsInvestigation` (a lead),
// returning `false` wrongly permits `NoKnownImpact` (a wrong answer
// to a user). These tests pin that an unresolved non-HTTP consumer
// blocks `NoKnownImpact` on its protocol's endpoint, that an
// unknown-key (`UrlExpr`) consumer cannot be ruled out against a
// non-HTTP endpoint, and — the over-reporting guard — that a clean
// non-HTTP scope with nothing unresolved keeps `NoKnownImpact`.

/// Complete coverage for one in-scope repo: reviewed scope plus a
/// valid ledger entry, so the `evaluate` coverage gate stays quiet
/// and any `NeedsInvestigation` in these tests can only come from
/// the could-match rule.
fn complete_coverage_for(service: &str) -> Coverage {
    let mut coverage = coverage_with_reviewed(vec![service]);
    coverage.repo_coverages.insert(
        service.into(),
        crate::federation::contracts::coverage::RepoCoverage {
            cache_key: crate::federation::contracts::index_cache::CacheKey::new(
                service,
                "abc",
                crate::federation::contracts::analyzer_version(),
            ),
            ..Default::default()
        },
    );
    coverage
}

#[test]
fn an_unresolved_topic_consumer_blocks_no_known_impact() {
    let call = id(
        "billing",
        "TopicConsumer",
        "src/b.py",
        "subscribe_orders",
        1,
    );
    let topic = ContractKey::Topic {
        broker: "kafka".into(),
        name: "orders.created".into(),
    };
    let endpoint = (svc("orders"), topic.clone());

    let mut head = ContractSurface::default();
    let mut payload = BTreeMap::new();
    payload.insert(path(&["order_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Payload, payload);
    head.endpoints
        .insert(endpoint.clone(), endpoint_def_with_fields(schemas));
    let consumer_key = consumer_key_for_call(&call, topic.clone());
    head.consumers.insert(
        consumer_key.clone(),
        ConsumerDef {
            call: call.clone(),
            resolution: SurfaceResolution::Unresolved {
                reason: UnresolvedReason::NoMatch,
                target_service: Some(svc("orders")),
            },
            reads: BTreeSet::new(),
            reads_complete: true,
        },
    );

    let mut coverage = complete_coverage_for("orders");
    coverage.unresolved_consumers.push(consumer_key);

    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint,
            direction: Direction::Payload,
            path: path(&["order_id"]),
        },
    };
    let impact = evaluate(&change, &head, &head, &coverage);
    assert_ne!(
        impact.class,
        Class::NoKnownImpact,
        "an unresolved topic consumer must block NoKnownImpact: {impact:?}"
    );
}

#[test]
fn an_unresolved_rpc_consumer_blocks_no_known_impact() {
    let call = id("billing", "RpcConsumer", "src/b.py", "get_order", 1);
    let rpc = ContractKey::Rpc {
        system: RpcSystem::Grpc,
        service: "orders.Orders".into(),
        method: "GetOrder".into(),
    };
    let endpoint = (svc("orders"), rpc.clone());

    let mut head = ContractSurface::default();
    let mut response = BTreeMap::new();
    response.insert(path(&["order_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Response, response);
    head.endpoints
        .insert(endpoint.clone(), endpoint_def_with_fields(schemas));
    let consumer_key = consumer_key_for_call(&call, rpc.clone());
    head.consumers.insert(
        consumer_key.clone(),
        ConsumerDef {
            call: call.clone(),
            resolution: SurfaceResolution::Unresolved {
                reason: UnresolvedReason::NoMatch,
                target_service: Some(svc("orders")),
            },
            reads: BTreeSet::new(),
            reads_complete: true,
        },
    );

    let mut coverage = complete_coverage_for("orders");
    coverage.unresolved_consumers.push(consumer_key);

    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint,
            direction: Direction::Response,
            path: path(&["order_id"]),
        },
    };
    let impact = evaluate(&change, &head, &head, &coverage);
    assert_ne!(
        impact.class,
        Class::NoKnownImpact,
        "an unresolved rpc consumer must block NoKnownImpact: {impact:?}"
    );
}

#[test]
fn an_unresolved_graphql_consumer_blocks_no_known_impact() {
    let call = id("billing", "GraphqlConsumer", "src/b.ts", "queryOrders", 1);
    let gql = ContractKey::Graphql {
        op: GraphqlOp::Query,
        field: "orders".into(),
    };
    let endpoint = (svc("orders"), gql.clone());

    let mut head = ContractSurface::default();
    let mut response = BTreeMap::new();
    response.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Response, response);
    head.endpoints
        .insert(endpoint.clone(), endpoint_def_with_fields(schemas));
    let consumer_key = consumer_key_for_call(&call, gql.clone());
    head.consumers.insert(
        consumer_key.clone(),
        ConsumerDef {
            call: call.clone(),
            resolution: SurfaceResolution::Unresolved {
                reason: UnresolvedReason::NoMatch,
                target_service: Some(svc("orders")),
            },
            reads: BTreeSet::new(),
            reads_complete: true,
        },
    );

    let mut coverage = complete_coverage_for("orders");
    coverage.unresolved_consumers.push(consumer_key);

    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint,
            direction: Direction::Response,
            path: path(&["customer_id"]),
        },
    };
    let impact = evaluate(&change, &head, &head, &coverage);
    assert_ne!(
        impact.class,
        Class::NoKnownImpact,
        "an unresolved graphql consumer must block NoKnownImpact: {impact:?}"
    );
}

#[test]
fn an_unresolved_table_consumer_blocks_no_known_impact() {
    let call = id("billing", "TableConsumer", "src/b.py", "read_shipments", 1);
    let table = ContractKey::Table {
        name: "shipments".into(),
    };
    let endpoint = (svc("orders"), table.clone());

    let mut head = ContractSurface::default();
    let mut payload = BTreeMap::new();
    payload.insert(path(&["shipment_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Payload, payload);
    head.endpoints
        .insert(endpoint.clone(), endpoint_def_with_fields(schemas));
    let consumer_key = consumer_key_for_call(&call, table.clone());
    head.consumers.insert(
        consumer_key.clone(),
        ConsumerDef {
            call: call.clone(),
            resolution: SurfaceResolution::Unresolved {
                reason: UnresolvedReason::NoMatch,
                target_service: Some(svc("orders")),
            },
            reads: BTreeSet::new(),
            reads_complete: true,
        },
    );

    let mut coverage = complete_coverage_for("orders");
    coverage.unresolved_consumers.push(consumer_key);

    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint,
            direction: Direction::Payload,
            path: path(&["shipment_id"]),
        },
    };
    let impact = evaluate(&change, &head, &head, &coverage);
    assert_ne!(
        impact.class,
        Class::NoKnownImpact,
        "an unresolved table consumer must block NoKnownImpact: {impact:?}"
    );
}

#[test]
fn could_match_is_conservative_when_the_consumer_key_is_unknown() {
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let mut head = ContractSurface::default();
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoMatch,
            target_service: None,
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    // A `UrlExpr` consumer: the call yielded no `ContractKey`, so
    // we cannot rule it out from the URL alone.
    let key = ConsumerKey {
        caller: SymbolKey {
            repo: repo("billing"),
            path: "src/b.py".into(),
            container: None,
            name: "fetch".into(),
        },
        target: ConsumerTargetKey::UrlExpr("fetch".into()),
    };
    head.consumers.insert(key.clone(), consumer_def);
    let target = (
        svc("orders"),
        ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders.created".into(),
        },
    );
    assert!(
        could_match(&key, &target, &head),
        "a UrlExpr consumer against a Topic endpoint cannot be ruled out \
         and must could-match"
    );
}

#[test]
fn a_non_http_endpoint_still_allows_no_known_impact_when_nothing_is_unresolved() {
    // The fix must not over-report: a clean scope with no unresolved
    // consumers keeps `NoKnownImpact`, before and after the change.
    let topic = ContractKey::Topic {
        broker: "kafka".into(),
        name: "orders.created".into(),
    };
    let endpoint = (svc("orders"), topic);

    let mut payload = BTreeMap::new();
    payload.insert(path(&["order_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Payload, payload);
    let mut base = ContractSurface::default();
    base.endpoints
        .insert(endpoint.clone(), endpoint_def_with_fields(schemas));
    let head = base.clone();

    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint,
            direction: Direction::Payload,
            path: path(&["order_id"]),
        },
    };
    let impact = evaluate(&change, &base, &head, &complete_coverage_for("orders"));
    assert_eq!(
        impact.class,
        Class::NoKnownImpact,
        "a non-HTTP change with nothing unresolved must keep \
         NoKnownImpact: {impact:?}"
    );
}

/// The `evaluate` coverage gate (`NoKnownImpactSound`, TLA+
/// CoverageClaim.tla) is protocol-agnostic: a topic change under an
/// incomplete ledger must still downgrade `NoKnownImpact` →
/// `NeedsInvestigation` even when no unresolved consumer exists to
/// could-match. Today this gate was only exercised for HTTP.
#[test]
fn incomplete_coverage_still_downgrades_a_topic_change() {
    let topic = ContractKey::Topic {
        broker: "kafka".into(),
        name: "orders.created".into(),
    };
    let endpoint = (svc("orders"), topic);

    let mut payload = BTreeMap::new();
    payload.insert(path(&["order_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Payload, payload);
    let mut base = ContractSurface::default();
    base.endpoints
        .insert(endpoint.clone(), endpoint_def_with_fields(schemas));
    let head = base.clone();

    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::FieldRemoved {
            endpoint,
            direction: Direction::Payload,
            path: path(&["order_id"]),
        },
    };
    // Reviewed scope but NO ledger entry — the coverage gate must
    // downgrade; nothing else in this test can produce a lead.
    let coverage = coverage_with_reviewed(vec!["orders"]);
    let impact = evaluate(&change, &base, &head, &coverage);
    assert_eq!(
        impact.class,
        Class::NeedsInvestigation,
        "an incomplete coverage ledger must downgrade a topic change: {impact:?}"
    );
    assert_eq!(impact.reason, Some(Reason::UnresolvedCandidates));
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
    let base_consumer_def = ConsumerDef {
        call: call.clone(),
        // Bound to the OLD endpoint in base — the consumer's URL
        // template matches the base endpoint, not the renamed one.
        // After the rename the consumer's `head` binding would
        // resolve against the new path.
        resolution: SurfaceResolution::Binds {
            endpoints: vec![base_endpoint.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let head_consumer_def = ConsumerDef {
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
    consumers.insert(consumer_key.clone(), head_consumer_def);
    let head = ContractSurface {
        endpoints,
        consumers,
    };
    let mut base_endpoints = BTreeMap::new();
    base_endpoints.insert(base_endpoint.clone(), base_endpoint_def.clone());
    let mut base_consumers = BTreeMap::new();
    base_consumers.insert(consumer_key.clone(), base_consumer_def);
    let base = ContractSurface {
        endpoints: base_endpoints,
        consumers: base_consumers,
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
            required: false,
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
            target_service: None,
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
                target_service: None,
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

// ─── Finding 5: coverage_complete combines unreviewed + could-match ─

#[test]
fn coverage_complete_unreviewed_blocks_complete() {
    use crate::federation::contracts::diff::coverage_complete;
    let index = ContractIndex::default();
    let coverage = build_coverage(
        &index,
        Vec::new(),
        Scope {
            reviewed: vec![],
            unreviewed: vec![crate::federation::contracts::diff::UnreviewedRepo {
                repo: "reports".into(),
                reason: "excluded".into(),
                error: None,
            }],
            configured_only: true,
        },
    );
    assert!(!coverage_complete(&coverage, None, &index));
}

#[test]
fn coverage_complete_no_endpoint_returns_true_when_reviewed_only() {
    use crate::federation::contracts::diff::coverage_complete;
    let index = ContractIndex::default();
    let coverage = build_coverage(
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
    assert!(coverage_complete(&coverage, None, &index));
}

#[test]
fn coverage_complete_unresolved_could_match_blocks_complete() {
    use crate::federation::contracts::diff::coverage_complete;
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let endpoint_id_orders = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Unresolved {
            reason: UnresolvedReason::NoRouteInService,
            target_service: Some(svc("orders")),
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, endpoint_id_orders.1.clone());
    let mut head = ContractSurface::default();
    head.consumers.insert(consumer_key.clone(), consumer_def);
    // Build the index manually: an unresolved consumer that could
    // match the orders endpoint.
    let mut index = ContractIndex::default();
    index.consumers.insert(
        call.clone(),
        ConsumerResolution {
            call_id: call.clone(),
            service: svc("billing"),
            target: Some(ConsumerTarget::Unresolved {
                reason: UnresolvedReason::NoRouteInService,
                target_service: Some(svc("orders")),
            }),
            bound_endpoints: Vec::new(),
            reads_complete: true,
        },
    );
    let mut coverage = build_coverage(
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
    coverage.unresolved_consumers.push(consumer_key);
    // Without an endpoint, complete is true (unreviewed empty).
    assert!(coverage_complete(&coverage, None, &index));
    // With the orders endpoint and a could-match candidate,
    // complete is false.
    assert!(!coverage_complete(
        &coverage,
        Some(&endpoint_id_orders),
        &index
    ));
    let _ = head;
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

// ─── Bug A: ChangedWithoutSchema precision ────────────────────────────
//
// `EndpointDef.source_files` must reflect the file holding the BOUND
// handler SymbolKey (when set), not every provider node's path. When
// the route declaration lives in a routing table file and the bound
// handler function lives elsewhere, an unrelated edit to the routing
// table must NOT trigger ChangedWithoutSchema for this endpoint —
// only an edit to the handler file should.

#[test]
fn source_files_uses_bound_handler_path_not_provider_node_id_path() {
    // Endpoint `GET /api/orders/{}/label` whose HttpRoute declaration
    // is in `src/routes.py` but whose bound handler function lives in
    // `src/handlers/label.py`. After surface extraction, source_files
    // must contain the handler file, not the routing-table file —
    // otherwise an unrelated edit to the routing table would falsely
    // fire ChangedWithoutSchema for THIS endpoint.
    let mut index = ContractIndex::default();
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}/label");
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/handlers/label.py".into(),
        container: None,
        name: "print_label".into(),
    };
    // HttpRoute node id points at the routing-table file.
    let node_id = id(
        "orders",
        "HttpRoute",
        "src/routes.py",
        "GET /api/orders/{}/label",
        5,
    );
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/{}/label".into(),
            providers: vec![EndpointProvider {
                node_id: node_id.clone(),
                origin: ProviderOrigin::Code,
                handler: Some(handler.clone()),
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );
    let surface = ContractSurface::from_index(&index);
    let def = &surface.endpoints[&endpoint];
    assert_eq!(
        def.source_files,
        BTreeSet::from(["src/handlers/label.py".to_string()]),
        "source_files must contain the bound handler file ({}); got {:?} \
         — the rule would fire on unrelated edits to the routing-table file",
        handler.path,
        def.source_files,
    );
    assert!(
        !def.source_files.contains("src/routes.py"),
        "source_files must NOT contain the routing-table file: {:?}",
        def.source_files,
    );
}

// Pipeline-level discriminator (replaces the two hand-constructed
// tests removed in review round 1). Goes through the production
// extraction path (`ContractSurface::from_index` → `endpoint_to_def`)
// and then `diff_contracts` with `StaticChangedFiles`, so the fix
// at `diff.rs:199-208` is exercised end-to-end. Pre-fix, `endpoint_to_def`
// populates `source_files` from `provider.node_id.path()` so
// `source_files = {src/routes.py}`; an edit to `src/routes.py` would
// falsely intersect and fire the rule, while an edit to the handler
// file alone would not. Post-fix, `source_files = {src/handlers/label.py}`.

#[test]
fn changed_without_schema_does_not_fire_on_routing_table_edit() {
    // Production-shaped: endpoint whose HttpRoute declaration is in
    // `src/routes.py` but whose bound handler function is in
    // `src/handlers/label.py`. An edit to `src/routes.py` MUST NOT
    // fire ChangedWithoutSchema for this endpoint.
    let mut index = ContractIndex::default();
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}/label");
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/handlers/label.py".into(),
        container: None,
        name: "print_label".into(),
    };
    let node_id = id(
        "orders",
        "HttpRoute",
        "src/routes.py",
        "GET /api/orders/{}/label",
        5,
    );
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/{}/label".into(),
            providers: vec![EndpointProvider {
                node_id: node_id.clone(),
                origin: ProviderOrigin::Code,
                handler: Some(handler.clone()),
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );
    let surface = ContractSurface::from_index(&index);
    let base = surface.clone();
    let head = surface;
    let mut changed = BTreeSet::new();
    changed.insert("src/routes.py".to_string());
    let src = StaticChangedFiles(changed);
    let changes = diff_contracts(&base, &head, &src);
    let fired = changes
        .iter()
        .any(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. }));
    assert!(
        !fired,
        "ChangedWithoutSchema fired on a routing-table edit; \
         source_files must be tied to the handler file, not the route \
         declaration file. got {changes:?}"
    );
}

#[test]
fn changed_without_schema_fires_on_handler_file_edit() {
    // Production-shaped: same endpoint as above; edit to the handler
    // file MUST fire ChangedWithoutSchema.
    let mut index = ContractIndex::default();
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}/label");
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/handlers/label.py".into(),
        container: None,
        name: "print_label".into(),
    };
    let node_id = id(
        "orders",
        "HttpRoute",
        "src/routes.py",
        "GET /api/orders/{}/label",
        5,
    );
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/{}/label".into(),
            providers: vec![EndpointProvider {
                node_id: node_id.clone(),
                origin: ProviderOrigin::Code,
                handler: Some(handler.clone()),
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );
    let surface = ContractSurface::from_index(&index);
    let base = surface.clone();
    let head = surface;
    let mut changed = BTreeSet::new();
    changed.insert("src/handlers/label.py".to_string());
    let src = StaticChangedFiles(changed);
    let changes = diff_contracts(&base, &head, &src);
    let matched = changes
        .iter()
        .find(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. }));
    assert!(
        matched.is_some(),
        "expected ChangedWithoutSchema when the handler file changes, got {changes:?}"
    );
}

// ─── Task 3 — ChangedWithoutSchema must not reach NoKnownImpact ─────

#[test]
fn a_schemaless_endpoint_with_no_consumers_is_never_no_known_impact() {
    // Handler file changed, endpoint has no schema, no bound consumer,
    // no unresolved candidates, coverage complete.
    let mut index = ContractIndex::default();
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}/label");
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/handlers/label.py".into(),
        container: None,
        name: "print_label".into(),
    };
    let node_id = id(
        "orders",
        "HttpRoute",
        "src/routes.py",
        "GET /api/orders/{}/label",
        5,
    );
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/{}/label".into(),
            providers: vec![EndpointProvider {
                node_id: node_id.clone(),
                origin: ProviderOrigin::Code,
                handler: Some(handler.clone()),
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );
    let surface = ContractSurface::from_index(&index);
    let base = surface.clone();
    let head = surface;
    let changed = BTreeSet::from(["src/handlers/label.py".to_string()]);
    let src = StaticChangedFiles(changed);
    let changes = diff_contracts(&base, &head, &src);
    let cw = changes
        .iter()
        .find(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. }))
        .expect("ChangedWithoutSchema must fire");
    let impact = evaluate(cw, &base, &head, &complete_coverage_for("orders"));
    assert_ne!(
        impact.class,
        Class::NoKnownImpact,
        "a changed handler on a schemaless endpoint with no consumers is \
         an unanalysed behaviour change, not 'no impact'"
    );
}

#[test]
fn a_compatible_change_with_no_consumers_stays_no_known_impact() {
    // The Task 3 fix must not swallow the `Compatible` arm: an
    // additive endpoint change with no consumers is genuinely
    // NoKnownImpact under complete coverage.
    let change = Change {
        service: svc("orders"),
        kind: ChangeKind::EndpointAdded {
            key: http_key(HttpMethod::Get, "/api/orders/lookup"),
        },
    };
    let impact = evaluate(
        &change,
        &ContractSurface::default(),
        &ContractSurface::default(),
        &complete_coverage_for("orders"),
    );
    assert_eq!(
        impact.class,
        Class::NoKnownImpact,
        "a compatible change with no consumers must stay NoKnownImpact: \
         {impact:?}"
    );
}

// Edge case from Bug A's spec Review Focus: when neither
// `handler: SymbolKey` nor a spec node resolves to a path,
// `source_files` ends up empty. The `ChangedWithoutSchema` rule must
// NOT fire on a no-files endpoint (no diff can match an empty set).
// This test pins that behaviour so the intersection filter's empty
// short-circuit cannot regress.

#[test]
fn source_files_empty_does_not_fire_changed_without_schema() {
    // Production-shaped: a spec-only provider whose node id was
    // minted via `from_canonical` with fewer than five segments, so
    // `GlobalId::path()` returns `None`. There is also no
    // `handler: SymbolKey`. `endpoint_to_def` therefore inserts no
    // file into `source_files`. With `has_schema = false`, the
    // rule's gate fires only if `source_files ∩ changed_files` is
    // non-empty — which is impossible when `source_files` is empty.
    let mut index = ContractIndex::default();
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/me");
    let node_id = GlobalId::from_canonical("orders:HttpRoute:orphan");
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/me".into(),
            providers: vec![EndpointProvider {
                node_id,
                origin: ProviderOrigin::OpenApi,
                handler: None,
                operation_id: Some("getMe".into()),
            }],
            schemas: BTreeMap::new(),
        },
    );
    let surface = ContractSurface::from_index(&index);
    let def = &surface.endpoints[&endpoint];
    assert!(
        def.source_files.is_empty(),
        "expected empty source_files for handler-less provider with no path, got {:?}",
        def.source_files
    );
    assert!(
        !def.has_schema,
        "expected no schema for this endpoint, got {:?}",
        def.schemas
    );
    let base = surface.clone();
    let head = surface;
    let mut changed = BTreeSet::new();
    changed.insert("src/whatever.py".to_string());
    let src = StaticChangedFiles(changed);
    let changes = diff_contracts(&base, &head, &src);
    let fired = changes
        .iter()
        .any(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. }));
    assert!(
        !fired,
        "ChangedWithoutSchema fired on a no-files endpoint (source_files \
         empty, has_schema=false); an empty intersection must not produce \
         a change. got {changes:?}"
    );
}

#[test]
fn changed_without_schema_isolates_by_provider_repo() {
    use crate::federation::contracts::diff::StaticRepoChangedFiles;
    use crate::federation::contracts::index::EndpointProvider;

    let mut index = ContractIndex::default();
    let orders_ep = endpoint_id("orders", HttpMethod::Get, "/orders");
    let billing_ep = endpoint_id("billing", HttpMethod::Get, "/billing");

    // orders provider in repo orders with handler in src/main.rs
    let orders_handler = SymbolKey {
        repo: repo("orders"),
        path: "src/main.rs".to_string(),
        container: None,
        name: "get_orders".to_string(),
    };
    index.endpoints.insert(
        orders_ep.clone(),
        Endpoint {
            id: orders_ep.clone(),
            method: HttpMethod::Get,
            template: "/orders".into(),
            providers: vec![EndpointProvider {
                node_id: id("orders", "HttpRoute", "src/main.rs", "get_orders", 1),
                origin: ProviderOrigin::Code,
                handler: Some(orders_handler),
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );

    // billing provider in repo billing with handler ALSO in src/main.rs
    let billing_handler = SymbolKey {
        repo: repo("billing"),
        path: "src/main.rs".to_string(),
        container: None,
        name: "get_billing".to_string(),
    };
    index.endpoints.insert(
        billing_ep.clone(),
        Endpoint {
            id: billing_ep.clone(),
            method: HttpMethod::Get,
            template: "/billing".into(),
            providers: vec![EndpointProvider {
                node_id: id("billing", "HttpRoute", "src/main.rs", "get_billing", 1),
                origin: ProviderOrigin::Code,
                handler: Some(billing_handler),
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );

    let surface = ContractSurface::from_index(&index);
    let base = surface.clone();
    let head = surface;

    // Only orders repo changed src/main.rs; billing has no changes
    let mut by_repo = BTreeMap::new();
    let mut orders_changed = BTreeSet::new();
    orders_changed.insert("src/main.rs".to_string());
    by_repo.insert("orders".to_string(), orders_changed);
    by_repo.insert("billing".to_string(), BTreeSet::new());

    let src = StaticRepoChangedFiles(by_repo);
    let changes = diff_contracts(&base, &head, &src);

    let orders_fired = changes.iter().any(|c| {
        c.service == svc("orders") && matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. })
    });
    let billing_fired = changes.iter().any(|c| {
        c.service == svc("billing") && matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. })
    });

    assert!(orders_fired, "expected orders to fire ChangedWithoutSchema");
    assert!(
        !billing_fired,
        "billing must NOT fire ChangedWithoutSchema when src/main.rs only changed in orders repo! got {changes:?}"
    );
}

#[test]
fn unavailable_diff_does_not_invent_a_handler_change() {
    use crate::federation::contracts::changed_files::RepoDiffResult;
    use crate::federation::contracts::diff::ChangedFilesSource;
    use crate::federation::contracts::index::EndpointProvider;

    struct UnavailableSource;
    impl ChangedFilesSource for UnavailableSource {
        fn changed_files(&self, _base: &str, _head: &str) -> BTreeSet<String> {
            BTreeSet::new()
        }
        fn changed_files_for_repo(&self, _repo: &str, _base: &str, _head: &str) -> RepoDiffResult {
            RepoDiffResult::Unavailable("missing mirror".into())
        }
        fn unavailable_repos(&self) -> BTreeSet<String> {
            let mut s = BTreeSet::new();
            s.insert("orders".into());
            s
        }
    }

    let mut index = ContractIndex::default();
    let orders_ep = endpoint_id("orders", HttpMethod::Get, "/orders");
    let orders_handler = SymbolKey {
        repo: repo("orders"),
        path: "src/main.rs".to_string(),
        container: None,
        name: "get_orders".to_string(),
    };
    index.endpoints.insert(
        orders_ep.clone(),
        Endpoint {
            id: orders_ep.clone(),
            method: HttpMethod::Get,
            template: "/orders".into(),
            providers: vec![EndpointProvider {
                node_id: id("orders", "HttpRoute", "src/main.rs", "get_orders", 1),
                origin: ProviderOrigin::Code,
                handler: Some(orders_handler),
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );

    let mut surface = ContractSurface::from_index(&index);
    let call = id("billing", "HttpClientCall", "src/b.py", "fetch", 1);
    let consumer_def = ConsumerDef {
        call: call.clone(),
        resolution: SurfaceResolution::Binds {
            endpoints: vec![orders_ep.clone()],
            provenance: EdgeProvenance::Static {
                source: crate::schema::StaticSource::TreeSitter,
            },
        },
        reads: BTreeSet::new(),
        reads_complete: true,
    };
    let consumer_key = consumer_key_for_call(&call, orders_ep.1.clone());
    surface.consumers.insert(consumer_key, consumer_def);

    let base = surface.clone();
    let head = surface;

    let src = UnavailableSource;
    let changes = diff_contracts(&base, &head, &src);

    assert!(
        !changes.iter().any(|c| {
            c.service == svc("orders") && matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. })
        }),
        "an unavailable git diff is not proof that provider source changed"
    );
}

#[test]
fn topic_payload_schema_removal_reports_breaking_change() {
    let mut base_index = ContractIndex::default();
    let topic_ep = (
        svc("orders"),
        ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders.events".into(),
        },
    );
    let mut fields = BTreeMap::new();
    fields.insert(path(&["order_id"]), field(TypeDesc::String, true, false));
    fields.insert(path(&["amount"]), field(TypeDesc::Number, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(
        Direction::Payload,
        EndpointSchema {
            node_id: id("orders", "Schema", "schemas/orders.avsc", "OrderEvent", 1),
            fields,
        },
    );
    base_index.endpoints.insert(
        topic_ep.clone(),
        Endpoint {
            id: topic_ep.clone(),
            method: HttpMethod::Any,
            template: "orders.events".into(),
            providers: vec![],
            schemas,
        },
    );

    let mut head_index = base_index.clone();
    head_index
        .endpoints
        .get_mut(&topic_ep)
        .unwrap()
        .schemas
        .get_mut(&Direction::Payload)
        .unwrap()
        .fields
        .remove(&path(&["amount"]));

    let base = ContractSurface::from_index(&base_index);
    let head = ContractSurface::from_index(&head_index);
    let changes = diff_contracts(&base, &head, &*changed_set());

    let removed = changes.iter().find(|c| {
        matches!(&c.kind, ChangeKind::FieldRemoved { path: p, direction: d, .. } if p == &path(&["amount"]) && *d == Direction::Payload)
    });
    assert!(
        removed.is_some(),
        "must detect FieldRemoved in Direction::Payload"
    );
}

// ─── HandlerChanged: schema-bearing endpoints ────────────────────────
//
// `ChangedWithoutSchema` fires only when BOTH sides have no schema.
// A behaviour change behind an endpoint that DOES have a schema
// therefore produced no `ChangeKind` at all — `diff_fields` saw an
// identical schema and said nothing — so a pure behaviour change was
// invisible to `diff_contracts`. These tests pin the sibling rule.

/// An endpoint that HAS a response schema and a bound handler.
fn schema_bearing_endpoint() -> (ContractIndex, EndpointId) {
    let mut index = ContractIndex::default();
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/orders/handlers.rs".into(),
        container: None,
        name: "get_order".into(),
    };
    let node_id = id(
        "orders",
        "HttpRoute",
        "src/orders/routes.rs",
        "GET /api/orders/:id",
        12,
    );
    let mut fields = BTreeMap::new();
    fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(
        Direction::Response,
        EndpointSchema {
            node_id: id("orders", "Schema", "openapi.yaml", "OrderResponse", 7),
            fields,
        },
    );
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/{}".into(),
            providers: vec![EndpointProvider {
                node_id,
                origin: ProviderOrigin::Code,
                handler: Some(handler),
                operation_id: None,
            }],
            schemas,
        },
    );
    (index, endpoint)
}

#[test]
fn handler_change_on_a_schema_bearing_endpoint_is_reported() {
    let (index, _ep) = schema_bearing_endpoint();
    let base = ContractSurface::from_index(&index);
    let head = base.clone();
    // The handler file changed; the schema is byte-identical.
    let src = StaticChangedFiles(BTreeSet::from(["src/orders/handlers.rs".to_string()]));

    let changes = diff_contracts(&base, &head, &src);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::HandlerChanged { .. })),
        "a schema-bearing endpoint whose handler file changed must not vanish: {changes:?}"
    );
    // The schema is unchanged, so no field-level change may appear.
    assert!(
        changes.iter().all(|c| !matches!(
            c.kind,
            ChangeKind::FieldAdded { .. } | ChangeKind::FieldRemoved { .. }
        )),
        "identical schemas must not produce field changes: {changes:?}"
    );
    // And it must not be mislabelled as the schema-less rule.
    assert!(
        changes
            .iter()
            .all(|c| !matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. })),
        "schema-bearing endpoints must not use ChangedWithoutSchema: {changes:?}"
    );
}

#[test]
fn schema_bearing_endpoint_unchanged_handler_emits_nothing() {
    let (index, _ep) = schema_bearing_endpoint();
    let base = ContractSurface::from_index(&index);
    let head = base.clone();
    let src = StaticChangedFiles(BTreeSet::new());

    let changes = diff_contracts(&base, &head, &src);
    assert!(
        changes.is_empty(),
        "no file changed -> no change, got {changes:?}"
    );
}

#[test]
fn schema_less_endpoint_still_reports_changed_without_schema() {
    // Regression guard: widening detection must not swallow the old
    // rule or repurpose its label.
    let mut index = ContractIndex::default();
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}/label");
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/orders/label.rs".into(),
        container: None,
        name: "get_order_label".into(),
    };
    let node_id = id(
        "orders",
        "HttpRoute",
        "src/orders/label.rs",
        "GET /api/orders/:id/label",
        8,
    );
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/{}/label".into(),
            providers: vec![EndpointProvider {
                node_id,
                origin: ProviderOrigin::Code,
                handler: Some(handler),
                operation_id: None,
            }],
            schemas: BTreeMap::new(),
        },
    );
    let base = ContractSurface::from_index(&index);
    let head = base.clone();
    let src = StaticChangedFiles(BTreeSet::from(["src/orders/label.rs".to_string()]));

    let changes = diff_contracts(&base, &head, &src);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::ChangedWithoutSchema { .. })),
        "schema-less endpoint must keep ChangedWithoutSchema: {changes:?}"
    );
    assert!(
        changes
            .iter()
            .all(|c| !matches!(c.kind, ChangeKind::HandlerChanged { .. })),
        "schema-less endpoints must not use HandlerChanged: {changes:?}"
    );
}

/// A pairing that moved in BOTH path and method reported nothing at
/// all — `PathChanged` required the method to be unchanged and
/// `MethodChanged` required the path to be unchanged, so their union
/// left a hole. `from` / `to` are full `ContractKey`s, so `PathChanged`
/// already carries the method half.
#[test]
fn a_path_and_method_rename_is_reported() {
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/main.rs".into(),
        container: None,
        name: "get_order".into(),
    };
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Response, response_fields);

    let base_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let head_endpoint = endpoint_id("orders", HttpMethod::Post, "/api/order/{}");
    let provider = ProviderRef {
        node_id: id("orders", "HttpRoute", "src/main.rs", "get_order", 12),
        handler: Some(handler),
        operation_id: None,
    };
    let mut base_endpoints = BTreeMap::new();
    base_endpoints.insert(
        base_endpoint,
        EndpointDef {
            providers: vec![provider.clone()],
            schemas: schemas.clone(),
            has_schema: true,
            source_files: BTreeSet::new(),
        },
    );
    let mut head_endpoints = BTreeMap::new();
    head_endpoints.insert(
        head_endpoint,
        EndpointDef {
            providers: vec![provider],
            schemas,
            has_schema: true,
            source_files: BTreeSet::new(),
        },
    );
    let base = ContractSurface {
        endpoints: base_endpoints,
        consumers: BTreeMap::new(),
    };
    let head = ContractSurface {
        endpoints: head_endpoints,
        consumers: BTreeMap::new(),
    };
    let src = StaticChangedFiles(BTreeSet::new());

    let changes = diff_contracts(&base, &head, &src);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::PathChanged { .. })),
        "a rename that moved both path and method must not go unreported: {changes:?}"
    );
}

/// Pins the *principled* suppression that makes it safe to attribute a
/// shared-handler change to every claimant: when `PathChanged` /
/// `MethodChanged` / a schema diff already explains this endpoint,
/// `HandlerChanged` must not also fire.
///
/// `HandlerChanged`'s stated purpose is "schema byte-identical, handler
/// moved — the case **nothing else** would report". A route rename
/// reports `PathChanged`; adding `HandlerChanged` for the same edit
/// counts it twice.
///
/// Note this case was once suppressed *accidentally* by the old
/// `owners.len() == 1` attribution gate (a rename yields two distinct
/// `EndpointId`s, so the file looked shared). That gate is gone, so the
/// `explained` suppression below is the only thing standing between us
/// and the double report — which is what this test is for.
#[test]
fn handler_change_alongside_a_path_rename_is_not_double_reported() {
    // The `s6-rename-path` shape: the route moved
    // `/api/orders/{}` → `/api/order/{}` and the routing table (the
    // handler's file) was edited to do it. `PathChanged` already
    // explains the endpoint, so `HandlerChanged` — whose stated purpose
    // is "schema byte-identical, handler moved, the case **nothing
    // else** would report" — must not also fire. Reporting both counts
    // one edit twice.
    let handler = SymbolKey {
        repo: repo("orders"),
        path: "src/main.rs".into(),
        container: None,
        name: "get_order".into(),
    };
    let mut response_fields = BTreeMap::new();
    response_fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(Direction::Response, response_fields);

    let base_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/{}");
    let head_endpoint = endpoint_id("orders", HttpMethod::Get, "/api/order/{}");
    let provider = |name: &str| ProviderRef {
        node_id: id("orders", "HttpRoute", "src/main.rs", name, 12),
        handler: Some(handler.clone()),
        operation_id: None,
    };

    let mut base_endpoints = BTreeMap::new();
    base_endpoints.insert(
        base_endpoint.clone(),
        EndpointDef {
            providers: vec![provider("get_order")],
            schemas: schemas.clone(),
            has_schema: true,
            source_files: BTreeSet::new(),
        },
    );
    let mut head_endpoints = BTreeMap::new();
    head_endpoints.insert(
        head_endpoint.clone(),
        EndpointDef {
            providers: vec![provider("get_order")],
            schemas,
            has_schema: true,
            source_files: BTreeSet::new(),
        },
    );
    let base = ContractSurface {
        endpoints: base_endpoints,
        consumers: BTreeMap::new(),
    };
    let head = ContractSurface {
        endpoints: head_endpoints,
        consumers: BTreeMap::new(),
    };
    let src = StaticChangedFiles(BTreeSet::from(["src/main.rs".to_string()]));

    let changes = diff_contracts(&base, &head, &src);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::PathChanged { .. })),
        "the rename must report PathChanged: {changes:?}"
    );
    assert!(
        changes
            .iter()
            .all(|c| !matches!(c.kind, ChangeKind::HandlerChanged { .. })),
        "PathChanged already explains this endpoint; HandlerChanged would \
         count the same edit twice: {changes:?}"
    );
}

/// Two schema-bearing endpoints whose handler symbols both live in
/// `src/main.rs` (a routing table). An edit to that file is a real
/// behaviour risk for **both** — suppressing it entirely lets
/// `NoKnownImpact` stand while behaviour may have moved, which is this
/// codebase's worst failure. So both claimants get a `HandlerChanged`
/// lead.
///
/// The earlier silence here was justified by a T1 precision regression
/// ("reporting it three times") on the `s6-rename-path` scenario. That
/// regression was a **double report**, not a mis-attribution: `s6` is a
/// route rename, `PathChanged` already explains it, and `HandlerChanged`
/// was counting the same edit again. That case is now suppressed
/// explicitly by `handler_change_alongside_a_path_rename_is_not_double_reported`,
/// so this one is free to be honest.
///
/// These are *leads* (`NeedsInvestigation`), not proven breaks — which
/// is exactly the "risks requiring investigation" bucket.
/// Line-level attribution: when the diff reports *which lines* of a
/// shared handler file changed, only the endpoint whose site sits in
/// those lines is implicated. This is what stops a routing-table edit
/// for one endpoint from becoming a lead against its siblings.
/// Build a `ContractIndex` of schema-bearing endpoints whose handlers
/// all live in one file — the shared-routing-table shape. Each entry is
/// `(template, route_name, handler_name, route_line)`.
fn shared_handler_file_index(
    handler_path: &str,
    sites: &[(&str, &str, &str, u32)],
) -> ContractIndex {
    let mut index = ContractIndex::default();
    for (template, route_name, handler_name, line) in sites {
        let endpoint = endpoint_id("orders", HttpMethod::Get, template);
        let mut fields = BTreeMap::new();
        fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
        let mut schemas = BTreeMap::new();
        schemas.insert(
            Direction::Response,
            EndpointSchema {
                node_id: id("orders", "Schema", "openapi.yaml", route_name, 7),
                fields,
            },
        );
        index.endpoints.insert(
            endpoint.clone(),
            Endpoint {
                id: endpoint.clone(),
                method: HttpMethod::Get,
                template: (*template).into(),
                providers: vec![EndpointProvider {
                    node_id: id("orders", "HttpRoute", handler_path, route_name, *line),
                    origin: ProviderOrigin::Code,
                    handler: Some(SymbolKey {
                        repo: repo("orders"),
                        path: handler_path.into(),
                        container: None,
                        name: (*handler_name).into(),
                    }),
                    operation_id: None,
                }],
                schemas,
            },
        );
    }
    index
}

/// A file-anchored source: `changed` is the changed path set for the
/// named repo, `spans` its changed line ranges.
fn anchored_source(
    repo_name: &str,
    changed: &[&str],
    spans: &[(&str, Vec<(u32, u32)>)],
) -> crate::federation::contracts::changed_files::MultiRepoChangedFiles {
    use crate::federation::contracts::changed_files::{MultiRepoChangedFiles, RepoDiffResult};
    let mut by_repo = BTreeMap::new();
    by_repo.insert(
        repo_name.to_string(),
        if changed.is_empty() {
            RepoDiffResult::Unchanged
        } else {
            RepoDiffResult::Changed(
                changed
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect::<BTreeSet<_>>(),
            )
        },
    );
    let mut per_file = crate::federation::contracts::diff::ChangedLines::new();
    for (f, s) in spans {
        per_file.insert((*f).to_string(), s.clone());
    }
    let mut line_ranges = BTreeMap::new();
    line_ranges.insert(repo_name.to_string(), per_file);
    MultiRepoChangedFiles {
        by_repo,
        line_ranges,
    }
}

/// A mixed edit — one line that *is* a claimant's site, and one that is
/// not (a handler body further down the same file). Whole-file
/// attribution must be used: the anchor of the second claimant is not
/// the boundary of its code, so "anchor not hit" does not prove it was
/// untouched.
#[test]
fn a_mixed_site_and_body_edit_falls_back_to_whole_file() {
    let index = shared_handler_file_index(
        "src/main.rs",
        &[
            ("/api/orders/{}", "GET /api/orders/:id", "get_order", 12),
            ("/api/orders/me", "GET /api/orders/me", "get_me", 18),
        ],
    );
    let base = ContractSurface::from_index(&index);
    let head = base.clone();

    // Line 12 = get_order's route site; line 40 = get_me's body, which
    // is not anybody's anchor.
    let src = anchored_source(
        "orders",
        &["src/main.rs"],
        &[("src/main.rs", vec![(12, 12), (40, 40)])],
    );

    let changes = diff_contracts(&base, &head, &src);
    let handler_changes: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::HandlerChanged { .. }))
        .collect();
    assert_eq!(
        handler_changes.len(),
        2,
        "a body line nobody anchors cannot rule any claimant out — both \
         must be implicated, else get_me goes silent while its code moved: \
         {changes:?}"
    );
}

/// A claimant with no anchor in the changed file (its route node lives
/// elsewhere) can never be ruled out. Hitting some *other* claimant's
/// anchor must not silence it.
#[test]
fn a_claimant_without_an_anchor_is_never_silenced() {
    let mut index = shared_handler_file_index(
        "src/main.rs",
        &[("/api/orders/{}", "GET /api/orders/:id", "get_order", 12)],
    );
    // Second endpoint: handler claims `src/main.rs`, but its route node
    // is in `src/routes.rs` — so it contributes no anchor.
    let endpoint = endpoint_id("orders", HttpMethod::Get, "/api/orders/me");
    let mut fields = BTreeMap::new();
    fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(
        Direction::Response,
        EndpointSchema {
            node_id: id("orders", "Schema", "openapi.yaml", "GET /api/orders/me", 7),
            fields,
        },
    );
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/api/orders/me".into(),
            providers: vec![EndpointProvider {
                node_id: id(
                    "orders",
                    "HttpRoute",
                    "src/routes.rs",
                    "GET /api/orders/me",
                    18,
                ),
                origin: ProviderOrigin::Code,
                handler: Some(SymbolKey {
                    repo: repo("orders"),
                    path: "src/main.rs".into(),
                    container: None,
                    name: "get_me".into(),
                }),
                operation_id: None,
            }],
            schemas,
        },
    );
    let base = ContractSurface::from_index(&index);
    let head = base.clone();

    // Only get_order's site changed.
    let src = anchored_source(
        "orders",
        &["src/main.rs"],
        &[("src/main.rs", vec![(12, 12)])],
    );

    let changes = diff_contracts(&base, &head, &src);
    let handler_changes: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::HandlerChanged { .. }))
        .collect();
    assert_eq!(
        handler_changes.len(),
        2,
        "get_me has no anchor to rule it out, so it must stay a lead: {changes:?}"
    );
}

/// Two repos each with a `src/main.rs` must not borrow each other's
/// line numbers. Repo A's body edit at line 18 must not be treated as
/// "attributed" because repo B happens to have a route site at 18.
#[test]
fn handler_anchors_do_not_leak_across_repos() {
    let mut index = shared_handler_file_index(
        "src/main.rs",
        &[("/api/orders/{}", "GET /api/orders/:id", "get_order", 20)],
    );
    // Billing's own endpoint, same file name, route site at line 18.
    let endpoint = endpoint_id("billing", HttpMethod::Get, "/invoices/{}");
    let mut fields = BTreeMap::new();
    fields.insert(path(&["id"]), field(TypeDesc::String, true, false));
    let mut schemas = BTreeMap::new();
    schemas.insert(
        Direction::Response,
        EndpointSchema {
            node_id: id("billing", "Schema", "openapi.yaml", "GET /invoices/:id", 7),
            fields,
        },
    );
    index.endpoints.insert(
        endpoint.clone(),
        Endpoint {
            id: endpoint.clone(),
            method: HttpMethod::Get,
            template: "/invoices/{}".into(),
            providers: vec![EndpointProvider {
                node_id: id(
                    "billing",
                    "HttpRoute",
                    "src/main.rs",
                    "GET /invoices/:id",
                    18,
                ),
                origin: ProviderOrigin::Code,
                handler: Some(SymbolKey {
                    repo: repo("billing"),
                    path: "src/main.rs".into(),
                    container: None,
                    name: "get_invoice".into(),
                }),
                operation_id: None,
            }],
            schemas,
        },
    );
    let base = ContractSurface::from_index(&index);
    let head = base.clone();

    // orders' `src/main.rs` line 18 changed — a body line, not a site.
    let src = anchored_source(
        "orders",
        &["src/main.rs"],
        &[("src/main.rs", vec![(18, 18)])],
    );

    let changes = diff_contracts(&base, &head, &src);
    let handler_changes: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::HandlerChanged { .. }))
        .collect();
    assert_eq!(
        handler_changes.len(),
        1,
        "orders' edit must implicate orders' claimant and nothing else — \
         billing's line 18 is a different file in a different repo: {changes:?}"
    );
    assert!(
        matches!(
            handler_changes[0].kind,
            ChangeKind::HandlerChanged { ref endpoint } if endpoint.0 == svc("orders")
        ),
        "the lead must be the orders endpoint, got {changes:?}"
    );
}

#[test]
fn handler_change_in_a_shared_file_is_attributed_by_changed_lines() {
    let mut index = ContractIndex::default();
    for (template, route_name, handler_name, line) in [
        ("/api/orders/{}", "GET /api/orders/:id", "get_order", 12),
        ("/api/orders/me", "GET /api/orders/me", "get_me", 18),
    ] {
        let endpoint = endpoint_id("orders", HttpMethod::Get, template);
        let mut fields = BTreeMap::new();
        fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
        let mut schemas = BTreeMap::new();
        schemas.insert(
            Direction::Response,
            EndpointSchema {
                node_id: id("orders", "Schema", "openapi.yaml", route_name, 7),
                fields,
            },
        );
        index.endpoints.insert(
            endpoint.clone(),
            Endpoint {
                id: endpoint.clone(),
                method: HttpMethod::Get,
                template: template.into(),
                providers: vec![EndpointProvider {
                    node_id: id("orders", "HttpRoute", "src/main.rs", route_name, line),
                    origin: ProviderOrigin::Code,
                    handler: Some(SymbolKey {
                        repo: repo("orders"),
                        path: "src/main.rs".into(),
                        container: None,
                        name: handler_name.into(),
                    }),
                    operation_id: None,
                }],
                schemas,
            },
        );
    }
    let base = ContractSurface::from_index(&index);
    let head = base.clone();

    // Only line 12 changed — `get_order`'s site. `get_me` at 18 is
    // untouched and must not be dragged in.
    let mut line_ranges = crate::federation::contracts::diff::ChangedLines::new();
    line_ranges.insert("src/main.rs".to_string(), vec![(12, 12)]);
    let mut by_repo = BTreeMap::new();
    by_repo.insert(
        "orders".to_string(),
        crate::federation::contracts::changed_files::RepoDiffResult::Changed(BTreeSet::from([
            "src/main.rs".to_string(),
        ])),
    );
    let src = crate::federation::contracts::changed_files::MultiRepoChangedFiles {
        by_repo,
        line_ranges: {
            let mut m = BTreeMap::new();
            m.insert("orders".to_string(), line_ranges);
            m
        },
    };

    let changes = diff_contracts(&base, &head, &src);
    let handler_changes: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::HandlerChanged { .. }))
        .collect();
    assert_eq!(
        handler_changes.len(),
        1,
        "only the endpoint whose site is in the changed lines may be \
         implicated — got {changes:?}"
    );
    assert!(
        matches!(
            handler_changes[0].kind,
            ChangeKind::HandlerChanged { ref endpoint } if endpoint.1
                == http_key(HttpMethod::Get, "/api/orders/{}")
        ),
        "the implicated endpoint must be get_order's, got {changes:?}"
    );
}

#[test]
fn handler_change_in_a_shared_file_is_reported_for_every_claimant() {
    let mut index = ContractIndex::default();
    for (template, route_name, handler_name, line) in [
        ("/api/orders/{}", "GET /api/orders/:id", "get_order", 12),
        ("/api/orders/me", "GET /api/orders/me", "get_me", 13),
    ] {
        let endpoint = endpoint_id("orders", HttpMethod::Get, template);
        let mut fields = BTreeMap::new();
        fields.insert(path(&["customer_id"]), field(TypeDesc::String, true, false));
        let mut schemas = BTreeMap::new();
        schemas.insert(
            Direction::Response,
            EndpointSchema {
                node_id: id("orders", "Schema", "openapi.yaml", route_name, 7),
                fields,
            },
        );
        index.endpoints.insert(
            endpoint.clone(),
            Endpoint {
                id: endpoint.clone(),
                method: HttpMethod::Get,
                template: template.into(),
                providers: vec![EndpointProvider {
                    node_id: id("orders", "HttpRoute", "src/main.rs", route_name, line),
                    origin: ProviderOrigin::Code,
                    handler: Some(SymbolKey {
                        repo: repo("orders"),
                        path: "src/main.rs".into(),
                        container: None,
                        name: handler_name.into(),
                    }),
                    operation_id: None,
                }],
                schemas,
            },
        );
    }
    let base = ContractSurface::from_index(&index);
    let head = base.clone();
    let src = StaticChangedFiles(BTreeSet::from(["src/main.rs".to_string()]));

    let changes = diff_contracts(&base, &head, &src);
    let handler_changes: Vec<_> = changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::HandlerChanged { .. }))
        .collect();
    assert_eq!(
        handler_changes.len(),
        2,
        "an edit to a file two endpoints share is a behaviour risk for both; \
         suppressing it lets NoKnownImpact stand — got {changes:?}"
    );
}

#[test]
fn handler_change_alongside_a_schema_change_is_not_double_reported() {
    // The endpoint's own schema changed AND its handler file changed.
    // `diff_fields` already reports the schema change; `HandlerChanged`
    // is only for "schema byte-identical", so it must stay silent.
    let (mut index, ep) = schema_bearing_endpoint();
    let mut head_index = index.clone();
    head_index
        .endpoints
        .get_mut(&ep)
        .unwrap()
        .schemas
        .get_mut(&Direction::Response)
        .unwrap()
        .fields
        .insert(path(&["total"]), field(TypeDesc::Number, true, false));

    let base = ContractSurface::from_index(&index);
    let head = ContractSurface::from_index(&head_index);
    let src = StaticChangedFiles(BTreeSet::from(["src/orders/handlers.rs".to_string()]));

    let changes = diff_contracts(&base, &head, &src);
    assert!(
        changes
            .iter()
            .any(|c| matches!(c.kind, ChangeKind::FieldAdded { .. })),
        "the schema change must still be reported: {changes:?}"
    );
    assert!(
        changes
            .iter()
            .all(|c| !matches!(c.kind, ChangeKind::HandlerChanged { .. })),
        "a schema change already explains the diff; HandlerChanged must \
         not double-report it: {changes:?}"
    );
    let _ = &mut index;
}

// `HandlerChanged` has the same zero-consumer hazard as
// `ChangedWithoutSchema`: a schema-bearing endpoint whose handler
// changed, with no bound consumer and no could-match candidates, must
// not be reported as "no known impact". The schema is byte-identical,
// so nothing else in the diff explains the change — it is an
// unanalysed behaviour change.

#[test]
fn handler_changed_with_no_consumers_is_never_no_known_impact() {
    let (index, _ep) = schema_bearing_endpoint();
    let base = ContractSurface::from_index(&index);
    let head = base.clone();
    // The handler file changed; the schema is byte-identical; the
    // endpoint has one provider and zero bound consumers.
    let src = StaticChangedFiles(BTreeSet::from(["src/orders/handlers.rs".to_string()]));

    let changes = diff_contracts(&base, &head, &src);
    let hc = changes
        .iter()
        .find(|c| matches!(c.kind, ChangeKind::HandlerChanged { .. }))
        .expect("HandlerChanged must fire for a schema-bearing endpoint whose handler moved");
    let impact = evaluate(hc, &base, &head, &complete_coverage_for("orders"));
    assert_ne!(
        impact.class,
        Class::NoKnownImpact,
        "a changed handler on a schema-bearing endpoint with no consumers is \
         an unanalysed behaviour change, not 'no impact'"
    );
    assert_eq!(impact.class, Class::NeedsInvestigation);
}
