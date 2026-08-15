use crate::federation::health::RepoHealth;
use crate::federation::manifest::{FederationManifest, RepoEntry};
use crate::federation::repo_id::RepoId;

#[test]
fn roundtrip_save_load() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("federation_manifest.bin");
    let mut m = FederationManifest::default();
    m.add_repo(RepoEntry {
        id: RepoId::new("auth-svc").unwrap(),
        source_kind: "local_clone".into(),
        source_config: serde_yaml::from_str("url: https://example.com/auth.git").unwrap(),
        last_indexed_unix: 1234567890,
        content_hash: "abc123".into(),
        health: RepoHealth::Ready,
    });
    m.save(&path).unwrap();
    let loaded = FederationManifest::load_or_default(&path).unwrap();
    assert_eq!(loaded.repos.len(), 1);
    assert_eq!(loaded.repos[0].id.as_str(), "auth-svc");
}

#[test]
fn load_or_default_returns_empty_when_file_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let m = FederationManifest::load_or_default(&tmp.path().join("nope.bin")).unwrap();
    assert!(m.repos.is_empty());
}

#[test]
fn remove_repo_drops_entry() {
    let mut m = FederationManifest::default();
    m.add_repo(RepoEntry {
        id: RepoId::new("a").unwrap(),
        source_kind: "workspace_dir".into(),
        source_config: serde_yaml::Value::Null,
        last_indexed_unix: 0,
        content_hash: String::new(),
        health: RepoHealth::Ready,
    });
    m.remove_repo(&RepoId::new("a").unwrap());
    assert!(m.repos.is_empty());
}
