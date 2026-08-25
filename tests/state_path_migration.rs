//! Runtime check for the hashed state-file migration. Runs in its own
//! integration-test binary, so setting `XDG_STATE_HOME` here cannot race
//! with other tests (each file under `tests/` is a separate process).

use lain::config::state_path_for_workspace;

#[test]
fn legacy_stem_state_file_is_migrated_to_hashed_name() {
    let state_home = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_STATE_HOME", state_home.path());
    let lain_state = state_home.path().join("lain");
    std::fs::create_dir_all(&lain_state).unwrap();

    // A workspace whose config shares the stem "repos.yaml".
    let ws = tempfile::tempdir().unwrap();
    let cfg = ws.path().join("repos.yaml");
    std::fs::write(&cfg, "repos: []\n").unwrap();

    // Seed the legacy pre-hash file; the first resolution must rename
    // it to the hashed name and preserve the contents.
    // (`file_stem` of "repos.yaml" is "repos" → legacy "repos.json".)
    let legacy = lain_state.join("repos.json");
    std::fs::write(&legacy, "{\"migrated\":true}").unwrap();

    let resolved = state_path_for_workspace(&cfg);
    assert!(
        resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("repos-"),
        "hashed name expected, got {}",
        resolved.display()
    );
    assert!(!legacy.exists(), "legacy file must be renamed away");
    assert_eq!(std::fs::read_to_string(&resolved).unwrap(), "{\"migrated\":true}");

    // Idempotent: a second resolution keeps the hashed path.
    assert_eq!(state_path_for_workspace(&cfg), resolved);
}
