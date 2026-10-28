//! Check the actual dependency paths emitted by the package's build script.
#[test]
fn build_script_watches_existing_inputs_after_its_move() {
    let output = std::path::Path::new(env!("OUT_DIR"))
        .parent()
        .unwrap()
        .join("output");
    let output = std::fs::read_to_string(output).unwrap();
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let paths: Vec<_> = output
        .lines()
        .filter_map(|line| line.strip_prefix("cargo:rerun-if-changed="))
        .map(|path| manifest.join(path))
        .collect();
    assert!(!paths.is_empty(), "build script must declare its inputs");
    for path in &paths {
        assert!(
            path.exists(),
            "missing watch input causes perpetual rebuilds: {}",
            path.display()
        );
    }
    assert!(
        paths.iter().any(|path| path.ends_with("frameworks.yaml")),
        "framework configuration must invalidate generated queries"
    );
}
