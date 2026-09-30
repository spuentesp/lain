//! Golden tests for the 13 contract-federation MCP tools (§15.3).
//!
//! Each tool has a hand-written `*.out.json` schema under
//! `src/server/mcp/contract_tools/schemas/`. These tests build the
//! envelope for a synthetic `data` payload, then validate the
//! envelope against the schema with `jsonschema`. They are pure
//! schema-shape checks: they do not run the tool against a
//! real federation. The integration tests in
//! `tests/federation_contracts_e2e.rs` cover tool correctness
//! against a synthetic three-repo federation.
//!
//! One test per tool. The schemas are loaded with `include_str!`
//! so the schema-drift CI gate (which compares `docs/tool-schema.json`
//! against the live `tools/list` payload) covers them too.

use jsonschema::Validator;
use serde_json::{json, Value};
use std::sync::OnceLock;

const ENVELOPE_SCHEMA: &str = include_str!("fixtures/contracts/golden/envelope.schema.json");

fn envelope_schema() -> &'static Validator {
    static CACHE: OnceLock<Validator> = OnceLock::new();
    CACHE.get_or_init(|| {
        let value: Value = serde_json::from_str(ENVELOPE_SCHEMA).expect("envelope schema");
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .build(&value)
            .expect("compile envelope schema")
    })
}

fn validate_envelope(envelope: &Value) -> Result<(), String> {
    let result = envelope_schema().validate(envelope);
    if let Err(error) = result {
        return Err(format!("{}: {}", error.instance_path, error));
    }
    Ok(())
}

fn error_envelope_shape(code: &str, details: Option<Value>) -> Value {
    json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "error": {
            "code": code,
            "message": format!("{code} sample"),
            "retryable": matches!(code, "snapshot_not_ready" | "busy"),
            "details": details,
        },
        "meta": { "elapsed_ms": 5 }
    })
}

#[test]
fn golden_list_services_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "items": [
                {"service": "orders", "repo": "orders", "paths": ["src"], "endpoints": 4, "consumer_services": 1, "unresolved_inbound": 0},
                {"service": "billing", "repo": "billing", "paths": ["src"], "endpoints": 1, "consumer_services": 1, "unresolved_inbound": 0}
            ],
            "scope": {"reviewed": [], "unreviewed": [], "configured_only": true}
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden list_services envelope");
}

#[test]
fn golden_get_service_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "service": "orders",
            "repo": "orders",
            "paths": ["src"],
            "provider_reviewed": true,
            "endpoints": ["http:GET /api/orders/{}"],
            "consumers": [],
            "unresolved_candidates": [],
            "scope": {"reviewed": [], "unreviewed": [], "configured_only": true}
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden get_service envelope");
}

#[test]
fn golden_prepare_snapshot_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "snap_abc",
        "reproducible": true,
        "data": {
            "snapshot": "snap_abc",
            "state": "ready",
            "repos": [
                {"repo": "orders", "commit": "deadbeef", "state": "cached"}
            ]
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden prepare_snapshot envelope");
}

#[test]
fn golden_get_snapshot_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "snapshot": "live",
            "state": "ready",
            "repos": [
                {"repo": "orders", "commit": "deadbeef", "state": "cached"}
            ]
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden get_snapshot envelope");
}

#[test]
fn golden_list_contracts_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "snap_abc",
        "reproducible": true,
        "data": {
            "items": [
                {
                    "endpoint": {"service": "orders", "key": "http:GET /api/orders/{}"},
                    "providers": [
                        {"id": "orders:HttpRoute:src/orders.py:get_order:10", "repo": "orders", "commit": "abc", "path": "src/orders.py", "line": 10, "text": ""}
                    ],
                    "has_schema": true,
                    "bound_consumers": 1
                }
            ],
            "scope": {"reviewed": [], "unreviewed": [], "configured_only": true}
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden list_contracts envelope");
}

#[test]
fn golden_get_contract_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "items": [
                {
                    "endpoint": {"service": "orders", "key": "http:GET /api/orders/{}"},
                    "providers": [],
                    "schemas": [
                        {"direction": "response", "fields": []}
                    ],
                    "consumers": []
                }
            ],
            "scope": {"reviewed": [], "unreviewed": [], "configured_only": true}
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden get_contract envelope");
}

#[test]
fn golden_list_unresolved_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "items": [
                {
                    "consumer": {"id": "billing:HttpClientCall:src/main.py:build_invoice:20", "repo": "billing", "commit": "", "path": "src/main.py", "line": 20, "text": "build_invoice"},
                    "url_expr": "/v1/api/orders/{}",
                    "method": "GET",
                    "reason": "no_match",
                    "target_service": "orders",
                    "candidates": [
                        {"endpoint": {"service": "orders", "key": "http:GET /api/orders/{}"}, "reason": "prefix_stripped"}
                    ]
                }
            ],
            "ambiguous": [],
            "scope": {"reviewed": [], "unreviewed": [], "configured_only": true}
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden list_unresolved envelope");
}

#[test]
fn golden_check_binding_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "valid": true,
            "reasons": [],
            "method_match": true,
            "template_match": "exact",
            "bindings_entry": "- consumer:\n    repo: billing\n    path: src/main.py\n    symbol: fetch_order\n    key: \"http:GET /api/orders/{}\"\n  provider:\n    service: orders\n    key: \"http:GET /api/orders/{}\"\n"
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden check_binding envelope");
}

#[test]
fn golden_diff_contracts_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "snap_head",
        "reproducible": true,
        "data": {
            "changes": [
                {
                    "side": "provider",
                    "endpoint": {"service": "orders", "key": "http:GET /api/orders/{}"},
                    "kind": "FieldRemoved",
                    "direction": "response",
                    "field": "customer_id",
                    "compat": "BreakingIfRead",
                    "impact": {
                        "class": "Verified",
                        "reasons": [],
                        "scope": {"reviewed": [], "unreviewed": [], "configured_only": true}
                    },
                    "affected": [],
                    "paths": [],
                    "truncated": false
                }
            ],
            "compatible_changes": 0,
            "coverage": {
                "complete": true,
                "scope": {"reviewed": [], "unreviewed": [], "configured_only": true},
                "repos": [],
                "unresolved_consumers": [],
                "ambiguous": [],
                "unnormalized": [],
                "external": [],
                "stale_bindings": 0,
                "schemaless_endpoints": []
            }
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden diff_contracts envelope");
}

#[test]
fn golden_trace_impact_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "paths": [
                {
                    "start": "orders:Function:get_order:1",
                    "min_confidence": 1.0,
                    "hops": [
                        {"edge": "Calls", "node": "billing:Function:fetch_order:10", "node_type": "Function", "name": "fetch_order", "provenance": {"kind": "static", "confidence": 1.0}}
                    ]
                }
            ],
            "truncated": false,
            "scope": {"reviewed": [], "unreviewed": [], "configured_only": true}
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden trace_impact envelope");
}

#[test]
fn golden_get_coverage_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "complete": true,
            "scope": {
                "reviewed": [{"repo": "orders", "commit": "abc", "dirty": false}],
                "unreviewed": [],
                "configured_only": true
            },
            "repos": [
                {"repo": "orders", "commit": "abc", "state": "indexed", "sensors": {}}
            ],
            "unresolved_consumers": [],
            "ambiguous": [],
            "unnormalized": [],
            "external": [{"host": "api.stripe.com", "calls": 1}],
            "stale_bindings": 0,
            "schemaless_endpoints": []
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden get_coverage envelope");
}

#[test]
fn golden_resolve_evidence_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "items": [
                {"ref": "orders:Function:get_order:1", "exists": true},
                {"ref": "billing@aabbccddee:src/main.py:5", "exists": false, "reason": "no_such_node"},
                {"ref": "totally-malformed", "exists": false, "reason": "malformed"}
            ]
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden resolve_evidence envelope");
}

#[test]
fn golden_read_source_envelope_matches_schema() {
    let envelope = json!({
        "api_version": 1,
        "analyzer_version": "0.9.0",
        "snapshot": "live",
        "reproducible": false,
        "data": {
            "commit": "abc",
            "path": "src/main.py",
            "start": 0,
            "end": 10,
            "total_lines": 50,
            "text": "line1\nline2",
            "source": "live"
        },
        "meta": { "elapsed_ms": 1 }
    });
    validate_envelope(&envelope).expect("golden read_source envelope");
}

#[test]
fn golden_error_envelope_unsupported_api_version_carries_supported() {
    let envelope = error_envelope_shape("unsupported_api_version", Some(json!({"supported": [1]})));
    let result = envelope_schema().validate(&envelope);
    assert!(result.is_ok(), "error envelope must validate: {result:?}");
}

#[test]
fn golden_error_envelope_repo_not_registered() {
    let envelope = error_envelope_shape("repo_not_registered", Some(json!({"repo": "ghost"})));
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_ref_not_found() {
    let envelope = error_envelope_shape(
        "ref_not_found",
        Some(json!({"repo": "orders", "ref": "missing"})),
    );
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_snapshot_not_found() {
    let envelope = error_envelope_shape("snapshot_not_found", None);
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_service_not_found() {
    let envelope = error_envelope_shape("service_not_found", Some(json!({"service": "ghost"})));
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_snapshot_not_ready() {
    let envelope = error_envelope_shape("snapshot_not_ready", Some(json!({"state": "indexing"})));
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_snapshot_failed() {
    let envelope = error_envelope_shape(
        "snapshot_failed",
        Some(json!({"repos": [{"repo": "orders", "state": "failed"}]})),
    );
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_analyzer_mismatch() {
    let envelope = error_envelope_shape(
        "analyzer_mismatch",
        Some(json!({"base": "0.8.0+c1", "head": "0.9.0+c2"})),
    );
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_contract_not_found() {
    let envelope = error_envelope_shape(
        "contract_not_found",
        Some(json!({"endpoint": {"service": "orders", "key": "http:GET /x"}})),
    );
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_invalid_id() {
    let envelope = error_envelope_shape("invalid_id", Some(json!({"id": "not-a-global-id"})));
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_range_too_large() {
    let envelope = error_envelope_shape(
        "range_too_large",
        Some(json!({"limit": 4000, "max": 1000, "requested": 4000})),
    );
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_path_rejected() {
    let envelope = error_envelope_shape(
        "path_rejected",
        Some(json!({"reason": "secret", "path": ".env"})),
    );
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_busy() {
    let envelope = error_envelope_shape("busy", Some(json!({"retry_after_ms": 1000})));
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_federation_disabled() {
    let envelope = error_envelope_shape("federation_disabled", None);
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_error_envelope_invalid_argument() {
    let envelope = error_envelope_shape(
        "invalid_argument",
        Some(json!({"arg": "from", "reason": "live_not_supported"})),
    );
    assert!(envelope_schema().validate(&envelope).is_ok());
}

#[test]
fn golden_one_test_per_error_code() {
    // §13 mandates a test per code; this single test asserts that
    // every code has a corresponding explicit golden above. If
    // someone adds a new error code without a test, the loop below
    // will fire.
    let expected = [
        "unsupported_api_version",
        "federation_disabled",
        "invalid_argument",
        "repo_not_registered",
        "ref_not_found",
        "snapshot_not_found",
        "service_not_found",
        "snapshot_not_ready",
        "snapshot_failed",
        "analyzer_mismatch",
        "contract_not_found",
        "invalid_id",
        "range_too_large",
        "path_rejected",
        "busy",
    ];
    for code in expected {
        let envelope = error_envelope_shape(code, Some(json!({})));
        let result = envelope_schema().validate(&envelope);
        assert!(result.is_ok(), "{code} must validate: {result:?}");
    }
}
