//! Additional contract tests for `ContractFederationConfig` that
//! are not part of the in-file `#[cfg(test)]` block.

use crate::federation::contracts::config::{
    ContractFederationConfig, BUILTIN_GENERIC_KEYS, MAX_PATH_ARG,
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
