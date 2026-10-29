//! A repository operator can replace detection conventions without rebuilding.
use lain::server::sensors::grpc_consumer_sensor::{discover_rust_grpc_candidates, RustGrpcConfig};

#[test]
fn repo_yaml_and_query_overrides_are_honored_and_invalid_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let patterns = dir.path().join(".lain/patterns");
    std::fs::create_dir_all(patterns.join("rust")).unwrap();
    std::fs::write(
        patterns.join("rust-grpc.yaml"),
        r#"
generated_type_pattern: '^custom::Transport$'
constructors: [build]
deny_methods: [ignored]
accessors: []
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("client.rs"),
        "fn f() { let c = custom::Transport::build(ch); c.get(req); c.ignored(req); }",
    )
    .unwrap();
    assert_eq!(discover_rust_grpc_candidates(dir.path()).unwrap().len(), 1);
    std::fs::write(patterns.join("rust/grpc-candidate.scm"), r#"
(call_expression function: (field_expression value: (_) @receiver field: (field_identifier) @method) (#eq? @method "ignored")) @call
"#).unwrap();
    assert!(discover_rust_grpc_candidates(dir.path())
        .unwrap()
        .is_empty());
    std::fs::write(
        patterns.join("rust-grpc.yaml"),
        "generated_type_pattern: '['",
    )
    .unwrap();
    assert!(RustGrpcConfig::load(dir.path()).is_err());
}
