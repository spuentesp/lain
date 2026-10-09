//! Additional contract tests for `ContractFederationConfig` that
//! are not part of the in-file `#[cfg(test)]` block.

use crate::federation::contracts::config::{
    ContractFederationConfig, DatabaseDecl, SchemaDecl, ServiceDecl, BUILTIN_GENERIC_KEYS,
    MAX_PATH_ARG,
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

// ─── Task 7: `databases` config — consume or reject ───────────────────
//
// `DatabaseDecl.name` and `DatabaseDecl.tables` are the keys the
// joiner uses to attribute a `Table` fact to a service. Two
// databases with the same `name`, or the same table listed in two
// different databases, are configuration mistakes the operator
// must see at startup — silently picking the first one is the
// `shared_with` silent-no-op bug. Reject at `validate()`.

#[test]
fn rejects_duplicate_database_name() {
    let yaml = r#"
services:
  - name: orders
    repo: alpha
databases:
  - name: orders_db
    service: orders
    tables: [orders]
  - name: orders_db
    service: orders
    tables: [order_items]
"#;
    let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
    let err = cfg
        .validate(&["alpha".to_string()])
        .expect_err("duplicate database name must be rejected");
    assert!(
        err.to_string()
            .contains("duplicate database name 'orders_db'"),
        "got: {err}"
    );
}

#[test]
fn rejects_duplicate_table_across_databases() {
    // Two databases list the same table `orders`. The joiner keys
    // table ownership on table name (only), so this used to
    // attribute every `orders` Table fact to the first db's
    // service. Reject: the operator must disambiguate.
    let yaml = r#"
services:
  - name: orders
    repo: alpha
  - name: analytics
    repo: beta
databases:
  - name: orders_db
    service: orders
    tables: [orders]
  - name: analytics_db
    service: analytics
    tables: [orders]
"#;
    let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
    let err = cfg
        .validate(&["alpha".to_string(), "beta".to_string()])
        .expect_err("duplicate table across databases must be rejected");
    assert!(
        err.to_string()
            .contains("table 'orders' is listed by both database 'orders_db' and 'analytics_db'"),
        "got: {err}"
    );
}

#[test]
fn rejects_duplicate_table_within_one_database() {
    // Same table listed twice in the same database — also a
    // config error, not a silent dedup.
    let yaml = r#"
services:
  - name: orders
    repo: alpha
databases:
  - name: orders_db
    service: orders
    tables: [orders, orders]
"#;
    let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
    let err = cfg
        .validate(&["alpha".to_string()])
        .expect_err("duplicate table within a database must be rejected");
    assert!(
        err.to_string()
            .contains("table 'orders' is listed more than once in database 'orders_db'"),
        "got: {err}"
    );
}

#[test]
fn allows_distinct_tables_across_databases() {
    // Positive control: orders_db lists `orders`, analytics_db
    // lists `metrics` — distinct table names, distinct database
    // names. The configuration is unambiguous.
    let mut cfg = ContractFederationConfig::default();
    cfg.services.push(ServiceDecl {
        name: "orders".into(),
        repo: "alpha".into(),
        paths: vec![],
        hosts: vec![],
        env: vec![],
        base_path: None,
        route_prefixes: vec![],
    });
    cfg.services.push(ServiceDecl {
        name: "analytics".into(),
        repo: "beta".into(),
        paths: vec![],
        hosts: vec![],
        env: vec![],
        base_path: None,
        route_prefixes: vec![],
    });
    cfg.databases.push(DatabaseDecl {
        name: "orders_db".into(),
        service: "orders".into(),
        tables: vec!["orders".into()],
        shared_with: vec![],
    });
    cfg.databases.push(DatabaseDecl {
        name: "analytics_db".into(),
        service: "analytics".into(),
        tables: vec!["metrics".into()],
        shared_with: vec![],
    });
    cfg.validate(&["alpha".to_string(), "beta".to_string()])
        .expect("distinct table names + distinct database names must validate");
}

#[test]
fn database_service_can_target_implicit_service() {
    // `databases[].service` validation must align with
    // `http_clients.service` — both check against `known_services`
    // (configured + implicit repo ids). An implicit service
    // (repo id with no `services[]` entry) is a valid owner; the
    // joiner falls back to the assigned service when no database
    // is declared, and the validation must not reject the
    // declaration itself just because the operator omitted a
    // matching `services[]` entry.
    let mut cfg = ContractFederationConfig::default();
    cfg.databases.push(DatabaseDecl {
        name: "alpha_db".into(),
        service: "alpha".into(), // implicit service: repo id with no `services[]` entry
        tables: vec!["users".into()],
        shared_with: vec![],
    });
    cfg.validate(&["alpha".to_string()])
        .expect("implicit-service database.service must validate");
}

#[test]
fn database_service_rejects_unknown_service() {
    // Negative control for the alignment: a `databases[].service`
    // that matches neither a configured service nor an implicit
    // repo id is still rejected.
    let mut cfg = ContractFederationConfig::default();
    cfg.services.push(ServiceDecl {
        name: "orders".into(),
        repo: "alpha".into(),
        paths: vec![],
        hosts: vec![],
        env: vec![],
        base_path: None,
        route_prefixes: vec![],
    });
    cfg.databases.push(DatabaseDecl {
        name: "orders_db".into(),
        service: "nope".into(), // unknown
        tables: vec!["orders".into()],
        shared_with: vec![],
    });
    let err = cfg
        .validate(&["alpha".to_string()])
        .expect_err("unknown database.service must be rejected");
    assert!(
        err.to_string()
            .contains("database 'orders_db' references unknown service 'nope'"),
        "got: {err}"
    );
}

#[test]
fn database_shared_with_can_target_implicit_service() {
    // `shared_with` must also accept implicit services: a repo
    // id with no `services[]` entry is still a valid peer the
    // table is shared with.
    let mut cfg = ContractFederationConfig::default();
    cfg.services.push(ServiceDecl {
        name: "orders".into(),
        repo: "alpha".into(),
        paths: vec![],
        hosts: vec![],
        env: vec![],
        base_path: None,
        route_prefixes: vec![],
    });
    cfg.databases.push(DatabaseDecl {
        name: "orders_db".into(),
        service: "orders".into(),
        tables: vec!["orders".into()],
        shared_with: vec!["beta".into()], // implicit service
    });
    cfg.validate(&["alpha".to_string(), "beta".to_string()])
        .expect("implicit-service shared_with must validate");
}
