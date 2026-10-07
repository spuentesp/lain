//! Additional contract tests for `ContractFederationConfig` that
//! are not part of the in-file `#[cfg(test)]` block.

use crate::federation::contracts::config::{
    ContractFederationConfig, SchemaDecl, ServiceDecl, BUILTIN_GENERIC_KEYS, MAX_PATH_ARG,
};

#[test]
fn builtin_generic_keys_contains_the_documented_set() {
    let expected = [
        "GET /",
        "GET /health",
        "GET /healthz",
        "GET /ready",
        "GET /readyz",
        "GET /live",
        "GET /livez",
        "GET /ping",
        "GET /status",
        "GET /version",
        "GET /metrics",
        "GET /favicon.ico",
    ];
    for k in &expected {
        assert!(
            BUILTIN_GENERIC_KEYS.contains(k),
            "missing built-in generic key: {k}"
        );
    }
    assert_eq!(BUILTIN_GENERIC_KEYS.len(), expected.len());
}

#[test]
fn max_path_arg_is_five() {
    assert_eq!(MAX_PATH_ARG, 5);
}

#[test]
fn default_config_is_empty() {
    let cfg = ContractFederationConfig::default();
    assert!(cfg.services.is_empty());
    assert!(cfg.http_clients.is_empty());
    assert!(cfg.generic_keys.is_empty());
    assert!(cfg.schemas.is_empty());
    assert!(cfg.bindings.is_empty());
    // validate is a no-op on the default config.
    cfg.validate(&[]).unwrap();
}

#[test]
fn rejects_schema_decl_for_repo_with_no_configured_service() {
    // Task 4 negative case: a SchemaDecl whose `repo` does not
    // match any configured service's `repo` is rejected at
    // validate(). An implicit service (repo id with no entry in
    // services[]) cannot own a payload schema — the field_join
    // step 4b has no service_decl to read base_path from, and the
    // payload would silently attach to a wrong endpoint. The rule
    // is "configured service with matching repo" — see the
    // handoff ruling.
    let yaml = r#"
services:
  - name: orders
    repo: alpha
schemas:
  - topic: orders.events
    repo: beta
    file: schemas/orders.avsc
"#;
    let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
    let repo_ids = vec!["alpha".to_string(), "beta".to_string()];
    let err = cfg.validate(&repo_ids).unwrap_err();
    assert!(
        err.to_string().contains(
            "schemas entry 'orders.events' references repo 'beta' with no configured service"
        ),
        "got: {err}"
    );
}

#[test]
fn allows_schema_decl_when_a_service_in_the_same_repo_exists() {
    let mut cfg = ContractFederationConfig::default();
    cfg.services.push(ServiceDecl {
        name: "payments-api".into(),
        repo: "payments".into(),
        paths: vec![],
        hosts: vec![],
        env: vec![],
        base_path: None,
        route_prefixes: vec![],
    });
    cfg.schemas.push(SchemaDecl {
        topic: "payments.charged".into(),
        repo: "payments".into(),
        file: "schemas/payments.avsc".into(),
    });
    let repo_ids = vec!["payments".to_string()];
    cfg.validate(&repo_ids).unwrap();
}
