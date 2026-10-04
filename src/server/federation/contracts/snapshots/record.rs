//! Snapshot record (`docs/CONTRACT_FEDERATION.md` §8.4).
//!
//! The on-disk shape is a JSON file at
//! `<data_dir>/snapshots/<snapshot_id>.json` carrying every fact the
//! `prepare_snapshot` and `get_snapshot` MCP tools need to render
//! their output, plus the cached config used by `from_snapshot`'s
//! join:
//!
//! ```text
//! {
//!   id, repos: {repo: commit}, excluded: [repo],
//!   refs: {repo: ref-as-given}, join_config,
//!   config_hash, analyzer_version, state,
//!   repo_states: {repo: {state, error?}},
//!   created_unix, last_access_unix
//! }
//! ```
//!
//! `join_config` is the canonical-JSON serialisation of the
//! `ContractFederationConfig` at preparation time. A derived
//! snapshot prepared with `from: <id>` inherits the base's
//! `join_config`, so the base and head are always comparable
//! (`§8.4` "Which config"). The derived snapshot's `repos` only
//! overrides the base repos the caller named; the rest inherit the
//! base's commit. Derived snapshots do NOT inherit failure.
//!
//! Identity is `blake3` over the canonical JSON of `{repos,
//! excluded, config_hash, analyzer_version}` (`§8.4`). The first 16
//! hex characters prefixed with `snap_` form the public id; the
//! same input after ref resolution yields the same id, so the
//! `prepare_snapshot` idempotence invariant (`§13` "Idempotent")
//! falls out for free.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::federation::contracts::config::ContractFederationConfig;
use crate::federation::contracts::index_cache::CacheKey;

/// `snap_` — the literal prefix the snapshot id is written with
/// (`§8.4`).
pub const SNAPSHOT_ID_PREFIX: &str = "snap_";

/// The state machine a snapshot walks through (`§11`):
/// `pending → indexing → ready | failed`, plus the
/// `ready → indexing` transition when a cache entry was evicted
/// out from under a resident federation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotState {
    Pending,
    Indexing,
    Ready,
    Failed,
}

impl SnapshotState {
    pub fn as_str(self) -> &'static str {
        match self {
            SnapshotState::Pending => "pending",
            SnapshotState::Indexing => "indexing",
            SnapshotState::Ready => "ready",
            SnapshotState::Failed => "failed",
        }
    }
}

/// The per-repo status inside a snapshot record (`§8.4`). The wire
/// shape mirrors §12: `cached`, `queued`, `indexing`, `failed`,
/// `excluded`. `error` is set when `state == failed` so the tool
/// caller can render the per-repo failure reason verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RepoSnapshotState {
    Cached { commit: String },
    Queued { commit: String },
    Indexing { commit: String },
    Failed { commit: String, error: String },
    Excluded,
}

impl RepoSnapshotState {
    pub fn state_label(&self) -> &'static str {
        match self {
            RepoSnapshotState::Cached { .. } => "cached",
            RepoSnapshotState::Queued { .. } => "queued",
            RepoSnapshotState::Indexing { .. } => "indexing",
            RepoSnapshotState::Failed { .. } => "failed",
            RepoSnapshotState::Excluded => "excluded",
        }
    }

    pub fn commit(&self) -> Option<&str> {
        match self {
            RepoSnapshotState::Cached { commit }
            | RepoSnapshotState::Queued { commit }
            | RepoSnapshotState::Indexing { commit }
            | RepoSnapshotState::Failed { commit, .. } => Some(commit.as_str()),
            RepoSnapshotState::Excluded => None,
        }
    }
}

/// The inputs to `prepare_snapshot` after ref resolution and
/// `max_base_age_s` inheritance — the canonical snapshot request
/// shape the manager operates on. Constructed from the tool's
/// arguments; `repos` maps repo → commit (40-hex), `excluded` is
/// the explicit exclude list, `join_config` is the resolved
/// `ContractFederationConfig` (current config for non-derived
/// snapshots; the base's `join_config` for derived snapshots).
#[derive(Debug, Clone)]
pub struct SnapshotInput {
    pub repos: BTreeMap<String, String>,
    pub excluded: Vec<String>,
    pub refs_: BTreeMap<String, String>,
    pub join_config: ContractFederationConfig,
    pub config_hash: String,
    pub analyzer_version: String,
}

impl SnapshotInput {
    /// Build the canonical-JSON byte stream the snapshot id is
    /// computed over. The shape is `{repos, excluded, config_hash,
    /// analyzer_version}` per §8.4. Refs and `join_config` are not
    /// part of identity because refs resolve to the same commit
    /// (`resolve_ref` returns the canonical sha), and `join_config`
    /// is folded into `config_hash`. Anything outside this set is
    /// excluded by design — the id must be stable across the
    /// ref-name vs. sha form of the same commit, and across a
    /// config edit that produces the same `config_hash`.
    fn canonical_id_payload(&self) -> Vec<u8> {
        let canonical = serde_json::json!({
            "repos": self.repos,
            "excluded": self.excluded,
            "config_hash": self.config_hash,
            "analyzer_version": self.analyzer_version,
        });
        canonical.to_string().into_bytes()
    }
}

/// Compute the snapshot id (`§8.4`): `"snap_" + first 16 hex of
/// blake3(canonical_json({repos, excluded, config_hash,
/// analyzer_version}))`.
pub fn snapshot_id_for(input: &SnapshotInput) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&input.canonical_id_payload());
    let digest = hasher.finalize();
    let hex = digest.to_hex();
    format!("{}{}", SNAPSHOT_ID_PREFIX, &hex.as_str()[..32])
}

/// Public escape hatch used by tests that want to assert the
/// canonical-JSON identity calculation without a `SnapshotInput`.
pub fn canonical_snapshot_id(
    repos: &BTreeMap<String, String>,
    excluded: &[String],
    config_hash: &str,
    analyzer_version: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    let canonical = serde_json::json!({
        "repos": repos,
        "excluded": excluded,
        "config_hash": config_hash,
        "analyzer_version": analyzer_version,
    });
    hasher.update(canonical.to_string().as_bytes());
    let hex = hasher.finalize().to_hex();
    format!("{}{}", SNAPSHOT_ID_PREFIX, &hex.as_str()[..32])
}

/// The on-disk record (`§8.4`). The fields are public so callers
/// (the manager + the tool handlers) can construct / inspect them
/// directly; serde handles the JSON shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotRecord {
    pub id: String,
    /// `repo -> commit` after ref resolution. Keys are sorted by
    /// `BTreeMap` so the JSON shape is deterministic.
    pub repos: BTreeMap<String, String>,
    /// Repos in the union of `repos` + `exclude` that the snapshot
    /// leaves out (the `exclude` argument). Sorted.
    pub excluded: Vec<String>,
    /// `repo -> ref-as-given` (the input the tool caller named —
    /// `"main"`, `"refs/heads/main"`, a sha, a tag). Useful for
    /// `get_snapshot`'s JSON output so the operator can see what
    /// the snapshot was *prepared* against (vs. the resolved
    /// commit). Sorted.
    pub refs_: BTreeMap<String, String>,
    /// Canonical JSON of the join-relevant config sections at
    /// preparation time (`§7.1`).
    pub join_config: serde_json::Value,
    pub config_hash: String,
    pub analyzer_version: String,
    pub state: SnapshotState,
    /// `repo -> {state, error?}`. Sorted.
    pub repo_states: BTreeMap<String, RepoSnapshotState>,
    pub created_unix: i64,
    pub last_access_unix: i64,
}

/// On-disk layout helpers.
pub fn snapshots_root(data_dir: &Path) -> PathBuf {
    data_dir.join("snapshots")
}

pub fn snapshot_path(data_dir: &Path, snapshot_id: &str) -> PathBuf {
    snapshots_root(data_dir).join(format!("{snapshot_id}.json"))
}

/// Read a snapshot record from disk. Returns `None` when the file
/// does not exist; surfaces a `LainError` for corrupt JSON so the
/// manager can decide between "delete stale" and "fail loudly".
pub fn read_record(
    data_dir: &Path,
    snapshot_id: &str,
) -> Result<Option<SnapshotRecord>, crate::error::LainError> {
    let path = snapshot_path(data_dir, snapshot_id);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<SnapshotRecord>(&bytes)
            .map(Some)
            .map_err(|e| {
                crate::error::LainError::Serialization(format!(
                    "snapshot record {} corrupt: {e}",
                    path.display()
                ))
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::error::LainError::Io(e.to_string())),
    }
}

/// List every snapshot id on disk, sorted ascending so a manager
/// restart loads in deterministic order. Used at startup to re-enqueue
/// `pending`/`indexing` records and to drive retention sweeps.
pub fn list_record_ids(data_dir: &Path) -> Result<Vec<String>, crate::error::LainError> {
    let root = snapshots_root(data_dir);
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out: Vec<String> = Vec::new();
    let Ok(dir) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    for entry in dir.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        if !name.ends_with(".json") {
            continue;
        }
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        out.push(id.to_string());
    }
    out.sort();
    Ok(out)
}

/// Atomic write: stage to a temp sibling, then `std::fs::rename` into
/// place. A torn write leaves the previous record intact; the next
/// sweep removes the temp dir. The temp filename embeds a
/// process-unique counter so two concurrent writes do not race on the
/// same staging path.
pub fn write_record(
    data_dir: &Path,
    record: &SnapshotRecord,
) -> Result<(), crate::error::LainError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let target = snapshot_path(data_dir, &record.id);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| crate::error::LainError::Io(e.to_string()))?;
    }
    let staging = target.with_extension(format!(
        "staging-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|e| crate::error::LainError::Serialization(e.to_string()))?;
    if let Err(e) = std::fs::write(&staging, &bytes) {
        let _ = std::fs::remove_file(&staging);
        return Err(crate::error::LainError::Io(e.to_string()));
    }
    if let Err(e) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_file(&staging);
        return Err(crate::error::LainError::Io(e.to_string()));
    }
    Ok(())
}

/// Update `last_access_unix` on the on-disk record. Idempotent when
/// the record is missing (returns Ok(()) without touching the
/// filesystem). The same atomic-rename trick `IndexCache::touch`
/// uses keeps a concurrent reader from observing a missing file.
pub fn touch_last_access(
    data_dir: &Path,
    snapshot_id: &str,
    now_unix: i64,
) -> Result<(), crate::error::LainError> {
    let Some(mut record) = read_record(data_dir, snapshot_id)? else {
        return Ok(());
    };
    record.last_access_unix = now_unix;
    write_record(data_dir, &record)
}

/// Build the per-repo cache key from a snapshot record entry. The
/// `analyzer_version` is the §8.3 analyzer key the snapshot record
/// carries — pinning a snapshot to that string means a different
/// analyzer build invalidates the cache.
pub fn cache_key_for(record: &SnapshotRecord, repo: &str) -> Option<CacheKey> {
    let commit = record.repos.get(repo)?;
    Some(CacheKey::new(repo, commit, record.analyzer_version.clone()))
}

/// Build a `CacheKey` from a snapshot id and a repo + commit +
/// analyzer version. Convenience for the manager's residency pin.
pub fn cache_key(repo: &str, commit: &str, analyzer_version: &str) -> CacheKey {
    CacheKey::new(repo, commit, analyzer_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> SnapshotInput {
        let cfg = ContractFederationConfig::default();
        let mut repos = BTreeMap::new();
        repos.insert("orders".into(), "abc".into());
        repos.insert("billing".into(), "def".into());
        SnapshotInput {
            repos,
            excluded: vec!["reports".into()],
            refs_: BTreeMap::new(),
            join_config: cfg,
            config_hash: "h".into(),
            analyzer_version: "0.9.0+c1".into(),
        }
    }

    #[test]
    fn snapshot_id_is_stable_across_calls() {
        let a = snapshot_id_for(&input());
        let b = snapshot_id_for(&input());
        assert_eq!(a, b);
        assert!(a.starts_with(SNAPSHOT_ID_PREFIX));
        assert_eq!(a.len(), SNAPSHOT_ID_PREFIX.len() + 32);
    }

    #[test]
    fn snapshot_id_changes_with_repos() {
        let mut b = input();
        b.repos.insert("platform".into(), "ghi".into());
        let a = snapshot_id_for(&input());
        let bb = snapshot_id_for(&b);
        assert_ne!(a, bb);
    }

    #[test]
    fn snapshot_id_changes_with_excluded() {
        let mut b = input();
        b.excluded.push("extra".into());
        let a = snapshot_id_for(&input());
        let bb = snapshot_id_for(&b);
        assert_ne!(a, bb);
    }

    #[test]
    fn snapshot_id_changes_with_config_hash() {
        let mut b = input();
        b.config_hash = "other".into();
        let a = snapshot_id_for(&input());
        let bb = snapshot_id_for(&b);
        assert_ne!(a, bb);
    }

    #[test]
    fn snapshot_id_changes_with_analyzer_version() {
        let mut b = input();
        b.analyzer_version = "0.9.0+c2".into();
        let a = snapshot_id_for(&input());
        let bb = snapshot_id_for(&b);
        assert_ne!(a, bb);
    }

    #[test]
    fn snapshot_id_independent_of_ref_names() {
        let a = input();
        let mut b = a.clone();
        b.refs_.insert("orders".into(), "main".into());
        // The id payload is built from `{repos, excluded, config_hash,
        // analyzer_version}`; the `refs_` map is not part of identity.
        assert_eq!(snapshot_id_for(&a), snapshot_id_for(&b));
    }

    #[test]
    fn record_round_trips_through_serde() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ContractFederationConfig::default();
        let mut repos = BTreeMap::new();
        repos.insert("orders".into(), "deadbeef".repeat(10));
        repos.insert("billing".into(), "feedface".repeat(10));
        let record = SnapshotRecord {
            id: "snap_test".into(),
            repos,
            excluded: vec!["reports".into()],
            refs_: BTreeMap::new(),
            join_config: serde_json::to_value(&cfg).unwrap(),
            config_hash: "h".into(),
            analyzer_version: "0.9.0+c1".into(),
            state: SnapshotState::Pending,
            repo_states: BTreeMap::new(),
            created_unix: 100,
            last_access_unix: 100,
        };
        write_record(dir.path(), &record).unwrap();
        let read = read_record(dir.path(), "snap_test").unwrap().unwrap();
        assert_eq!(read.id, record.id);
        assert_eq!(read.repos, record.repos);
        assert_eq!(read.state, SnapshotState::Pending);
    }

    #[test]
    fn list_record_ids_returns_sorted() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["snap_b", "snap_a", "snap_c"] {
            let record = SnapshotRecord {
                id: id.into(),
                repos: BTreeMap::new(),
                excluded: vec![],
                refs_: BTreeMap::new(),
                join_config: serde_json::json!({}),
                config_hash: "h".into(),
                analyzer_version: "0.9.0+c1".into(),
                state: SnapshotState::Pending,
                repo_states: BTreeMap::new(),
                created_unix: 0,
                last_access_unix: 0,
            };
            write_record(dir.path(), &record).unwrap();
        }
        let ids = list_record_ids(dir.path()).unwrap();
        assert_eq!(ids, vec!["snap_a", "snap_b", "snap_c"]);
    }

    #[test]
    fn touch_last_access_advances_field() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ContractFederationConfig::default();
        let mut repos = BTreeMap::new();
        repos.insert("orders".into(), "abc".into());
        let record = SnapshotRecord {
            id: "snap_x".into(),
            repos,
            excluded: vec![],
            refs_: BTreeMap::new(),
            join_config: serde_json::to_value(&cfg).unwrap(),
            config_hash: "h".into(),
            analyzer_version: "0.9.0+c1".into(),
            state: SnapshotState::Ready,
            repo_states: BTreeMap::new(),
            created_unix: 100,
            last_access_unix: 100,
        };
        write_record(dir.path(), &record).unwrap();
        touch_last_access(dir.path(), "snap_x", 200).unwrap();
        let read = read_record(dir.path(), "snap_x").unwrap().unwrap();
        assert_eq!(read.last_access_unix, 200);
        // touching a missing id is a no-op
        touch_last_access(dir.path(), "snap_missing", 300).unwrap();
    }

    #[test]
    fn cache_key_for_returns_some_for_known_repo() {
        let cfg = ContractFederationConfig::default();
        let mut repos = BTreeMap::new();
        repos.insert("orders".into(), "abcdef".into());
        let record = SnapshotRecord {
            id: "snap_x".into(),
            repos,
            excluded: vec![],
            refs_: BTreeMap::new(),
            join_config: serde_json::to_value(&cfg).unwrap(),
            config_hash: "h".into(),
            analyzer_version: "0.9.0+c1".into(),
            state: SnapshotState::Ready,
            repo_states: BTreeMap::new(),
            created_unix: 0,
            last_access_unix: 0,
        };
        let key = cache_key_for(&record, "orders").unwrap();
        assert_eq!(key.repo, "orders");
        assert_eq!(key.sha, "abcdef");
        assert_eq!(key.analyzer_version, "0.9.0+c1");
        assert!(cache_key_for(&record, "missing").is_none());
    }
}
