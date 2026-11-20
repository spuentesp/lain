//! Check the actual dependency paths emitted by the package's build script.
#[test]
fn build_script_watches_existing_inputs_after_its_move() {
    let output = build_script_output();
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

/// The build script's recorded stdout. Cargo keeps it as `output` beside the
/// script's `out` directory, but the layout of the build directory has moved
/// between cargo releases, so look in the script's directory and the ones
/// around it, and say what is there when it is nowhere.
fn build_script_output() -> String {
    let out = std::path::Path::new(env!("OUT_DIR"));
    let script_dir = out.parent().expect("OUT_DIR has a parent");
    let mut tried = Vec::new();
    for dir in [Some(script_dir), script_dir.parent()]
        .into_iter()
        .flatten()
    {
        let candidate = dir.join("output");
        match std::fs::read_to_string(&candidate) {
            Ok(text) if text.contains("cargo:rerun-if-changed=") => return text,
            Ok(_) => tried.push(format!(
                "{} (no rerun-if-changed lines)",
                candidate.display()
            )),
            Err(error) => tried.push(format!("{} ({error})", candidate.display())),
        }
    }
    let listing = |dir: &std::path::Path| {
        std::fs::read_dir(dir)
            .map(|entries| {
                let mut names: Vec<_> = entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names.join(", ")
            })
            .unwrap_or_else(|error| format!("unreadable: {error}"))
    };
    panic!(
        "build script output not found.\nOUT_DIR = {}\ntried: {tried:#?}\nscript dir holds: {}\nparent holds: {}",
        out.display(),
        listing(script_dir),
        script_dir.parent().map(listing).unwrap_or_default(),
    );
}
