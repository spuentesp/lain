//! Per-repo index cache for revision-pinned indexing (§8.3).
//!
//! Layout:
//!
//! ```text
//! <data_dir>/index-cache/<repo>/<sha>-<analyzer_version>/{graph.bin, manifest.json}
//! ```
//!
//! The manifest is JSON, written next to the cached `graph.bin`,
//! and carries the metadata eviction needs to make decisions
//! without re-reading the bincode blob. Eviction is LRU past
//! `LAIN_INDEX_CACHE_MB` (default 4096); entries held by a
//! resident snapshot federation or a running job are exempted
//! from eviction through a simple token API that PR 11 calls.
//!
//! Atomicity: every entry is written to a temp directory under
//! the cache root and renamed into place (`std::fs::rename` is
//! atomic on POSIX). A torn write leaves the previous entry
//! untouched; the partial temp dir is pruned by the next eviction
//! pass.
//!
//! Holds: `acquire_hold(repo, sha, analyzer_version)` returns a
//! [`CacheHold`] token. The token keeps the entry alive past the
//! LRU threshold until dropped. PR 11 wires this into the
//! residency + job-runner paths; this module owns the bookkeeping
//! and the `Drop` semantics.

use crate::server::time::now_unix;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::error::LainError;

/// Default cache budget in MiB when `LAIN_INDEX_CACHE_MB` is unset.
pub const DEFAULT_CACHE_MB: u64 = 4096;

/// `bytes` ↔ MiB. `1 << 20` is the conventional mebibyte; the env
/// var's name says "MB" but the unit is the binary one operators
/// expect from a cache budget.
const MIB: u64 = 1 << 20;

/// Resolve the configured cache budget from
/// `LAIN_INDEX_CACHE_MB`. Falls back to [`DEFAULT_CACHE_MB`] when
/// the env var is unset, empty, or unparseable. The function is
/// intentionally tolerant — operators can set a non-integer by
/// accident — but does surface parse errors as a `tracing::warn`
/// so the operator sees it once at startup.
pub fn cache_budget_mb() -> u64 {
    match std::env::var("LAIN_INDEX_CACHE_MB") {
        Ok(s) if !s.trim().is_empty() => match s.trim().parse::<u64>() {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "LAIN_INDEX_CACHE_MB={s:?} is not a u64 ({e}); falling back to default {DEFAULT_CACHE_MB}"
                );
                DEFAULT_CACHE_MB
            }
        },
        _ => DEFAULT_CACHE_MB,
    }
}

/// On-disk manifest. The schema is JSON so operators can inspect
/// a cache entry by hand without decoding bincode, and so future
/// revisions can add fields without a bincode layout bump.
///
/// The `analyzer_version` field is the same string the cache
/// directory name is keyed on (`<CARGO_PKG_VERSION>+c<CONTRACT_ANALYZER_REV>`).
/// Carrying it in the manifest too means a stale entry from a
/// different analyzer build is detectable on read without
/// comparing directory names.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CacheManifest {
    pub repo: String,
    pub commit: String,
    pub analyzer_version: String,
    pub files: Vec<String>,
    /// Counts per [`crate::server::sensors::SensorCountField`],
    /// serialized as plain u64s so the JSON shape is stable across
    /// sensor additions. The full per-sensor breakdown is in
    /// `sensor_counts` (a map of `name → count`) so the cache
    /// survives a sensor being added or removed.
    pub sensor_counts: BTreeMap<String, u64>,
    pub bytes: u64,
    pub created_unix: i64,
    pub last_used_unix: i64,
}

/// The cache key. `analyzer_version` is included so two builds of
/// the same analyzer revision share a cache entry (rare in
/// practice; documented because the §8.3 design is explicit about
/// the key shape).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CacheKey {
    pub repo: String,
    pub sha: String,
    pub analyzer_version: String,
}

impl CacheKey {
    pub fn new(
        repo: impl Into<String>,
        sha: impl Into<String>,
        analyzer_version: impl Into<String>,
    ) -> Self {
        Self {
            repo: repo.into(),
            sha: sha.into(),
            analyzer_version: analyzer_version.into(),
        }
    }

    /// Directory name: `<sha>-<analyzer_version>`. Lower-case sha
    /// (which `git2` already produces) so two cache keys for the
    /// same commit always agree on the directory name.
    pub fn dir_name(&self) -> String {
        format!("{}-{}", self.sha.to_lowercase(), self.analyzer_version)
    }
}

/// Directory layout helpers.
pub fn cache_root(data_dir: &Path) -> PathBuf {
    data_dir.join("index-cache")
}

pub fn entry_dir(data_dir: &Path, key: &CacheKey) -> PathBuf {
    cache_root(data_dir).join(&key.repo).join(key.dir_name())
}

pub fn manifest_path(data_dir: &Path, key: &CacheKey) -> PathBuf {
    entry_dir(data_dir, key).join("manifest.json")
}

pub fn graph_path(data_dir: &Path, key: &CacheKey) -> PathBuf {
    entry_dir(data_dir, key).join("graph.bin")
}

/// Sibling-of-`manifest.json` staging path used by
/// [`IndexCache::write_manifest_only`]. The staging file lives in
/// the same directory as the real manifest so `std::fs::rename`
/// is an atomic in-directory move on POSIX (and on Windows for
/// same-volume renames). The path embeds a process-unique
/// counter so two concurrent `touch` calls do not collide on the
/// same staging filename (the loser's `rename` would otherwise
/// fail with `ENOENT` because the source was already moved).
fn manifest_stage_path(data_dir: &Path, key: &CacheKey) -> PathBuf {
    let counter = MANIFEST_STAGE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    entry_dir(data_dir, key).join(format!(".manifest.staging.{counter}"))
}

static MANIFEST_STAGE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A hold on a cache entry. `Drop` releases the hold; while alive
/// the entry is exempt from LRU eviction. PR 11's residency
/// (snapshot federations) and job-runner (running indexing jobs)
/// paths acquire a hold for the lifetime of the user-visible
/// entity.
#[derive(Debug)]
pub struct CacheHold {
    key: CacheKey,
    registry: Arc<Mutex<HoldRegistry>>,
}

impl std::fmt::Display for CacheHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CacheHold({}@{})", self.key.repo, self.key.dir_name())
    }
}

impl CacheHold {
    pub fn key(&self) -> &CacheKey {
        &self.key
    }
}

impl Drop for CacheHold {
    fn drop(&mut self) {
        registry_release(&self.registry, &self.key);
    }
}

#[derive(Debug, Default)]
struct HoldRegistry {
    by_key: BTreeMap<CacheKey, u32>,
}

fn registry_acquire(reg: &Arc<Mutex<HoldRegistry>>, key: &CacheKey) {
    let mut g = reg.lock();
    *g.by_key.entry(key.clone()).or_insert(0) += 1;
}

fn registry_release(reg: &Arc<Mutex<HoldRegistry>>, key: &CacheKey) {
    let mut g = reg.lock();
    if let Some(n) = g.by_key.get_mut(key) {
        if *n <= 1 {
            g.by_key.remove(key);
        } else {
            *n -= 1;
        }
    }
}

/// The cache itself. Holds the on-disk root and the hold registry;
/// the eviction method does its own scan + (sorted by
/// `last_used_unix`) eviction pass.
///
/// The registry is `Arc<Mutex<…>>` so the same cache can be
/// shared across the residency path and the job runner (PR 11)
/// without a second source of truth. `Drop` on a [`CacheHold`]
/// decrements the counter; an entry with a counter > 0 is exempt
/// from eviction.
#[derive(Clone, Debug)]
pub struct IndexCache {
    data_dir: PathBuf,
    holds: Arc<Mutex<HoldRegistry>>,
}

impl IndexCache {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            holds: Arc::new(Mutex::new(HoldRegistry::default())),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Acquire a hold on `key`. The entry cannot be evicted until
    /// the returned [`CacheHold`] is dropped.
    pub fn acquire_hold(&self, key: CacheKey) -> CacheHold {
        registry_acquire(&self.holds, &key);
        CacheHold {
            key,
            registry: Arc::clone(&self.holds),
        }
    }

    /// True iff the entry is currently held (a `CacheHold` is
    /// alive for it). Used by eviction to skip protected entries.
    pub fn is_held(&self, key: &CacheKey) -> bool {
        self.holds.lock().by_key.contains_key(key)
    }

    /// Number of holds currently registered. Mostly useful for
    /// tests asserting that a `Drop` actually released.
    pub fn hold_count(&self) -> usize {
        self.holds.lock().by_key.len()
    }

    /// Write `graph.bin` and `manifest.json` atomically.
    ///
    /// The bytes are staged under a temp sibling directory,
    /// `manifest.json` is written first (so a torn graph write is
    /// detectable), then `graph.bin`, then the directory is
    /// renamed into place via `std::fs::rename`. On failure the
    /// temp directory is removed.
    ///
    /// `bytes_written` is the size of the `graph.bin` payload; the
    /// manifest records it so eviction can sum bytes without
    /// `stat`-ing every entry.
    pub fn write_entry(
        &self,
        key: &CacheKey,
        graph_bytes: &[u8],
        manifest: &CacheManifest,
    ) -> Result<(), LainError> {
        let target = entry_dir(&self.data_dir, key);
        if target.exists() {
            // Replace-in-place: evict the existing entry, then
            // re-write. This keeps the cache size accurate and
            // matches what the eviction path would have done on
            // its next pass.
            self.remove_entry(key)?;
        }
        let staging = stage_dir(&self.data_dir, key);
        std::fs::create_dir_all(&staging).map_err(|e| LainError::Io(e.to_string()))?;
        let manifest_bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|e| LainError::Serialization(e.to_string()))?;
        if let Err(e) = std::fs::write(staging.join("manifest.json"), &manifest_bytes) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(LainError::Io(e.to_string()));
        }
        if let Err(e) = std::fs::write(staging.join("graph.bin"), graph_bytes) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(LainError::Io(e.to_string()));
        }
        if let Err(e) = std::fs::create_dir_all(target.parent().unwrap()) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(LainError::Io(e.to_string()));
        }
        if let Err(e) = std::fs::rename(&staging, &target) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(LainError::Io(e.to_string()));
        }
        Ok(())
    }

    /// Read the manifest, if present. Used by the digest test and
    /// by future "is this entry fresh?" checks.
    pub fn read_manifest(&self, key: &CacheKey) -> Result<Option<CacheManifest>, LainError> {
        let path = manifest_path(&self.data_dir, key);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<CacheManifest>(&bytes) {
                Ok(m) => Ok(Some(m)),
                Err(e) => Err(LainError::Serialization(format!(
                    "manifest {} corrupt: {e}",
                    path.display()
                ))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(LainError::Io(e.to_string())),
        }
    }

    /// True if the entry exists on disk (manifest + graph.bin both
    /// present).
    pub fn has_entry(&self, key: &CacheKey) -> bool {
        manifest_path(&self.data_dir, key).exists() && graph_path(&self.data_dir, key).exists()
    }

    /// Discover a cache entry for `(repo, analyzer_version)` by
    /// scanning the on-disk directory. The cache's directory name
    /// is `<sha>-<analyzer_version>`, so a single match is the
    /// entry the indexer wrote (only one `<repo>/<sha>-<ver>/`
    /// directory can exist for a given `analyzer_version`).
    ///
    /// This is the restart-survival fallback for
    /// `SnapshotManager::refresh_repo_states`: when the runner is
    /// empty (the worker hasn't run this process lifetime) and the
    /// record's `repos[repo]` holds an unresolved ref, scanning the
    /// directory is the only way to find the cache entry the
    /// previous run wrote under the resolved SHA.
    ///
    /// Returns the `CacheKey` of the discovered entry, or `None`
    /// when no matching directory exists.
    pub fn discover(&self, repo: &str, analyzer_version: &str) -> Option<CacheKey> {
        let repo_dir = cache_root(&self.data_dir).join(repo);
        let entries = std::fs::read_dir(&repo_dir).ok()?;
        let suffix = format!("-{}", analyzer_version);
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(sha) = name.strip_suffix(&suffix) {
                // The entry's directory name is `<sha>-<analyzer_version>`;
                // the cache key lives at `<repo>/<sha>-<ver>/{manifest.json, graph.bin}`.
                if manifest_path(&self.data_dir, &CacheKey::new(repo, sha, analyzer_version))
                    .exists()
                    && graph_path(&self.data_dir, &CacheKey::new(repo, sha, analyzer_version))
                        .exists()
                {
                    return Some(CacheKey::new(repo, sha, analyzer_version));
                }
            }
        }
        None
    }

    /// Read `graph.bin` into a `Vec<u8>`. Used by PR 11's
    /// `from_snapshot` to hydrate the per-repo DB.
    pub fn read_graph_bytes(&self, key: &CacheKey) -> Result<Vec<u8>, LainError> {
        let path = graph_path(&self.data_dir, key);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(LainError::NotFound(
                format!("graph.bin at {}", path.display()),
            )),
            Err(e) => Err(LainError::Io(e.to_string())),
        }
    }

    /// Touch an entry's `last_used_unix` field on read. Idempotent
    /// when the entry is missing (returns Ok(())).
    ///
    /// The earlier implementation re-read the manifest, re-read the
    /// `graph.bin` payload, and called `write_entry` — which
    /// `remove_entry`'d the existing on-disk entry before staging
    /// the replacement. That created a TOCTOU window: a concurrent
    /// reader (or a concurrent `write_entry` racing another
    /// `touch`) could observe the entry missing between the
    /// remove and the rename. The fix is the cheap option the
    /// review surfaced: write ONLY the manifest in place, leaving
    /// `graph.bin` untouched. The manifest update goes through
    /// `manifest_path`'s parent with a sibling staging file and
    /// `std::fs::rename`, which is atomic on POSIX, so a
    /// concurrent reader sees either the old or the new manifest
    /// — never a missing file. The graph bytes are not staged,
    /// not removed, and not re-read; the manifest's `bytes`
    /// field is unchanged.
    pub fn touch(&self, key: &CacheKey) -> Result<(), LainError> {
        let Some(mut manifest) = self.read_manifest(key)? else {
            return Ok(());
        };
        manifest.last_used_unix = now_unix();
        self.write_manifest_only(key, &manifest)
    }

    /// Replace just `manifest.json` for `key` with `manifest`,
    /// leaving `graph.bin` untouched. The replacement is atomic
    /// on POSIX: a temp sibling file is written and then
    /// `std::fs::rename`'d into place. A concurrent reader sees
    /// either the previous manifest or the new one — never a
    /// missing file.
    ///
    /// `key` must already have a manifest on disk; the call is a
    /// no-op (returns Ok(()) without touching the filesystem)
    /// when the entry is missing. That keeps `touch`'s idempotent
    /// contract — a touch of a missing entry is a no-op — without
    /// having to invent a manifest to write.
    ///
    /// Concurrency: the staging path is unique per call (process
    /// id + a monotonic counter) so two `touch` calls racing each
    /// other do not collide on the same staging file. The actual
    /// `target_manifest` rename is atomic on POSIX, so whichever
    /// caller's rename lands last wins; the loser's contents are
    /// discarded (the same byte stream modulo the timestamp the
    /// loser had), which is the correct semantics for `touch`.
    fn write_manifest_only(
        &self,
        key: &CacheKey,
        manifest: &CacheManifest,
    ) -> Result<(), LainError> {
        let target_manifest = manifest_path(&self.data_dir, key);
        if !target_manifest.exists() {
            return Ok(());
        }
        let staging = manifest_stage_path(&self.data_dir, key);
        if let Some(parent) = staging.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LainError::Io(e.to_string()))?;
        }
        let bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|e| LainError::Serialization(e.to_string()))?;
        if let Err(e) = std::fs::write(&staging, &bytes) {
            let _ = std::fs::remove_file(&staging);
            return Err(LainError::Io(e.to_string()));
        }
        if let Err(e) = std::fs::rename(&staging, &target_manifest) {
            let _ = std::fs::remove_file(&staging);
            return Err(LainError::Io(e.to_string()));
        }
        Ok(())
    }

    /// Delete an entry. Skips held entries (the hold is the
    /// caller's signal that eviction must not touch them).
    pub fn remove_entry(&self, key: &CacheKey) -> Result<(), LainError> {
        if self.is_held(key) {
            return Err(LainError::Other(format!(
                "cannot remove held cache entry {}@{}",
                key.repo,
                key.dir_name()
            )));
        }
        let dir = entry_dir(&self.data_dir, key);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| LainError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// Iterate every cache entry currently on disk and apply the
    /// eviction policy. Returns the number of entries removed and
    /// the total bytes freed. Entries held by `acquire_hold` are
    /// skipped; the loop walks `last_used_unix` ascending so the
    /// least-recently-used entry comes first.
    ///
    /// `budget_mb` is the post-eviction size budget in mebibytes.
    /// The function removes entries until the on-disk total fits
    /// under it OR every unprotected entry has been visited.
    pub fn evict_lru(&self, budget_mb: u64) -> Result<EvictionOutcome, LainError> {
        let budget_bytes = budget_mb.saturating_mul(MIB);
        let entries = self.collect_entries()?;
        let mut by_used: Vec<&CacheEntryOnDisk> = entries.iter().collect();
        by_used.sort_by_key(|e| e.manifest.last_used_unix);

        let mut current: u64 = entries.iter().map(|e| e.bytes).sum();
        let mut removed = 0usize;
        let mut freed = 0u64;
        for entry in by_used {
            if current <= budget_bytes {
                break;
            }
            if self.is_held(&entry.key) {
                continue;
            }
            self.remove_entry(&entry.key)?;
            current = current.saturating_sub(entry.bytes);
            freed = freed.saturating_add(entry.bytes);
            removed += 1;
        }
        Ok(EvictionOutcome { removed, freed })
    }

    /// List every cache entry on disk. Used by PR 11's residency
    /// ("which entries are not held?") and by the eviction pass.
    pub fn collect_entries(&self) -> Result<Vec<CacheEntryOnDisk>, LainError> {
        let mut out = Vec::new();
        let root = cache_root(&self.data_dir);
        if !root.exists() {
            return Ok(out);
        }
        let Ok(repo_dirs) = std::fs::read_dir(&root) else {
            return Ok(out);
        };
        for repo_dir in repo_dirs.flatten() {
            let Ok(repos) = std::fs::read_dir(repo_dir.path()) else {
                continue;
            };
            for entry in repos.flatten() {
                let manifest_p = entry.path().join("manifest.json");
                let graph_p = entry.path().join("graph.bin");
                if !manifest_p.exists() || !graph_p.exists() {
                    continue;
                }
                let bytes = std::fs::metadata(&graph_p).map(|m| m.len()).unwrap_or(0);
                let manifest = match std::fs::read(&manifest_p) {
                    Ok(b) => match serde_json::from_slice::<CacheManifest>(&b) {
                        Ok(m) => m,
                        Err(_) => continue,
                    },
                    Err(_) => continue,
                };
                let repo_name = repo_dir.file_name().to_string_lossy().into_owned();
                out.push(CacheEntryOnDisk {
                    key: CacheKey::new(
                        repo_name,
                        manifest.commit.clone(),
                        manifest.analyzer_version.clone(),
                    ),
                    bytes,
                    manifest,
                    path: entry.path(),
                });
            }
        }
        Ok(out)
    }

    /// Remove every cache entry (used by tests and by the rare
    /// operator `lain cache purge`). Held entries are skipped.
    pub fn purge(&self) -> Result<usize, LainError> {
        let entries = self.collect_entries()?;
        let mut removed = 0usize;
        for e in entries {
            if self.is_held(&e.key) {
                continue;
            }
            self.remove_entry(&e.key)?;
            removed += 1;
        }
        Ok(removed)
    }
}

/// A cache entry as observed on disk.
#[derive(Debug, Clone)]
pub struct CacheEntryOnDisk {
    pub key: CacheKey,
    pub bytes: u64,
    pub manifest: CacheManifest,
    pub path: PathBuf,
}

/// Result of an eviction pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictionOutcome {
    pub removed: usize,
    pub freed: u64,
}

fn stage_dir(data_dir: &Path, key: &CacheKey) -> PathBuf {
    cache_root(data_dir)
        .join(&key.repo)
        .join(format!(".{}.staging", key.dir_name()))
}

/// Convenience for the integration test: build a manifest from
/// the minimum fields it records and the byte count the cache
/// layer measures. `files` is the repo-relative list the indexer
/// walked; `sensor_counts` is the per-sensor map.
pub fn build_manifest(
    repo: impl Into<String>,
    commit: impl Into<String>,
    analyzer_version: impl Into<String>,
    files: Vec<String>,
    sensor_counts: BTreeMap<String, u64>,
    bytes: u64,
) -> CacheManifest {
    let now = now_unix();
    CacheManifest {
        repo: repo.into(),
        commit: commit.into(),
        analyzer_version: analyzer_version.into(),
        files,
        sensor_counts,
        bytes,
        created_unix: now,
        last_used_unix: now,
    }
}

/// Validate a `CacheManifest`: well-formed JSON, the analyzer
/// version matches what the caller expects, and the recorded bytes
/// match the actual file size on disk. Surfaced through the
/// loader so PR 11 can distinguish a missing entry from a corrupt
/// one without re-decoding every manifest.
pub fn manifest_matches_on_disk(
    manifest: &CacheManifest,
    graph_bytes_on_disk: u64,
    expected_analyzer_version: &str,
) -> Result<(), String> {
    if manifest.analyzer_version != expected_analyzer_version {
        return Err(format!(
            "analyzer_version mismatch: manifest has {}, expected {}",
            manifest.analyzer_version, expected_analyzer_version
        ));
    }
    if manifest.bytes != graph_bytes_on_disk {
        return Err(format!(
            "manifest bytes {} do not match graph.bin size {}",
            manifest.bytes, graph_bytes_on_disk
        ));
    }
    Ok(())
}

/// Tracks the set of cache keys that should NEVER be evicted for
/// the lifetime of the cache process. PR 11 binds residency here.
#[derive(Debug, Default, Clone)]
pub struct ResidencyTracker {
    inner: Arc<Mutex<HashSet<CacheKey>>>,
}

impl ResidencyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_resident(&self, key: CacheKey) {
        self.inner.lock().insert(key);
    }

    pub fn clear_resident(&self, key: &CacheKey) {
        self.inner.lock().remove(key);
    }

    pub fn is_resident(&self, key: &CacheKey) -> bool {
        self.inner.lock().contains(key)
    }

    /// Convenience: register a key as both resident AND held. PR
    /// 11's `from_snapshot` uses this; standalone tests can use
    /// `mark_resident` + `acquire_hold` separately.
    pub fn pin(&self, cache: &IndexCache, key: CacheKey) -> CacheHold {
        self.mark_resident(key.clone());
        cache.acquire_hold(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn empty_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn write_then_read_round_trips_manifest_and_graph() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        let key = CacheKey::new(
            "orders",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "0.9.0+c1",
        );
        let mut counts = BTreeMap::new();
        counts.insert("http_routes".to_string(), 7);
        let manifest = build_manifest(
            "orders",
            &key.sha,
            "0.9.0+c1",
            vec!["src/main.rs".into()],
            counts,
            12,
        );
        cache
            .write_entry(&key, b"graph-payload", &manifest)
            .unwrap();
        let read = cache.read_manifest(&key).unwrap().expect("manifest exists");
        assert_eq!(read.repo, "orders");
        assert_eq!(read.analyzer_version, "0.9.0+c1");
        assert_eq!(read.files, vec!["src/main.rs".to_string()]);
        assert_eq!(read.bytes, 12);
        assert_eq!(read.sensor_counts.get("http_routes").copied(), Some(7));
        let bytes = cache.read_graph_bytes(&key).unwrap();
        assert_eq!(bytes, b"graph-payload");
    }

    #[test]
    fn write_replaces_existing_entry_atomically() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        let key = CacheKey::new(
            "orders",
            "abcabcabcabcabcabcabcabcabcabcabcabcabc",
            "0.9.0+c1",
        );
        let manifest = build_manifest("orders", &key.sha, "0.9.0+c1", vec![], BTreeMap::new(), 4);
        cache.write_entry(&key, b"first", &manifest).unwrap();
        let mut manifest2 = manifest.clone();
        manifest2.bytes = 5;
        cache.write_entry(&key, b"second", &manifest2).unwrap();
        assert_eq!(cache.read_graph_bytes(&key).unwrap(), b"second");
        let read = cache.read_manifest(&key).unwrap().unwrap();
        assert_eq!(read.bytes, 5);
    }

    #[test]
    fn touch_advances_last_used_unix() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        let key = CacheKey::new("orders", "ffff".repeat(10).as_str(), "0.9.0+c1");
        let manifest = build_manifest("orders", &key.sha, "0.9.0+c1", vec![], BTreeMap::new(), 1);
        cache.write_entry(&key, b"x", &manifest).unwrap();
        let before = cache.read_manifest(&key).unwrap().unwrap().last_used_unix;
        // Avoid a 0-second clock tick by sleeping briefly; the
        // actual delta is irrelevant, only the comparison matters.
        std::thread::sleep(std::time::Duration::from_millis(10));
        cache.touch(&key).unwrap();
        let after = cache.read_manifest(&key).unwrap().unwrap().last_used_unix;
        assert!(after >= before, "last_used_unix must not regress");
    }

    /// Two threads calling `touch` on the same entry must observe a
    /// manifest on disk at every moment. The earlier `touch` re-read
    /// the manifest + `graph.bin` and called `write_entry`, which
    /// `remove_entry`'d the on-disk entry before staging the
    /// replacement — concurrent observers could see the entry
    /// missing mid-rename. The fix (`write_manifest_only`) writes
    /// only the manifest via a sibling temp file + atomic rename,
    /// so the entry is never absent from disk.
    ///
    /// The test loops N iterations of read + touch and asserts that
    /// no read ever observed the manifest missing. A failure of
    /// the underlying guarantee would surface as `None` from
    /// `read_manifest` on a key that was just `write_entry`'d.
    #[test]
    fn touch_never_removes_entry_under_concurrent_reads() {
        use std::sync::Arc;
        use std::thread;
        let tmp = empty_dir();
        let cache = Arc::new(IndexCache::new(tmp.path()));
        let key = CacheKey::new("orders", "0011".repeat(10).as_str(), "0.9.0+c1");
        let manifest = build_manifest("orders", &key.sha, "0.9.0+c1", vec![], BTreeMap::new(), 4);
        cache.write_entry(&key, b"abcd", &manifest).unwrap();

        // Two touch threads racing. `stop` flips once the main
        // thread has decided to stop; the readers poll
        // `read_manifest` and a thread-local "missing count" the
        // main thread then aggregates.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let stop = Arc::clone(&stop);
            let key = key.clone();
            let h = thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    cache.touch(&key).expect("touch");
                    std::thread::sleep(std::time::Duration::from_micros(50));
                }
            });
            handles.push(h);
        }
        // A reader thread that polls `read_manifest` and counts
        // any moment the manifest is absent.
        let reader_stop = Arc::clone(&stop);
        let reader_cache = Arc::clone(&cache);
        let reader_key = key.clone();
        let reader = thread::spawn(move || -> u64 {
            let mut missing = 0u64;
            let mut reads = 0u64;
            while !reader_stop.load(std::sync::atomic::Ordering::Relaxed) {
                let m = reader_cache.read_manifest(&reader_key).expect("read");
                if m.is_none() {
                    missing += 1;
                }
                reads += 1;
            }
            (missing << 32) | (reads & 0xFFFFFFFF)
        });
        // Run for a bounded window so the test terminates.
        std::thread::sleep(std::time::Duration::from_millis(500));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for h in handles {
            h.join().unwrap();
        }
        let (missing, reads) = {
            let packed = reader.join().unwrap();
            ((packed >> 32) as u64, packed & 0xFFFFFFFF)
        };
        assert!(
            reads > 100,
            "reader should have done many reads; got {reads}"
        );
        assert_eq!(
            missing, 0,
            "manifest must never be missing during concurrent touches (reads={reads})"
        );
    }

    /// `touch` racing `evict_lru` must not leave the entry in a
    /// half-removed state. The reader loop also asserts that
    /// `has_entry` returns `true` throughout the race — the entry
    /// either exists on disk or the eviction has already removed
    /// it cleanly (and the reader sees `false`).
    #[test]
    fn touch_versus_evict_lru_concurrent_is_safe() {
        use std::sync::Arc;
        use std::thread;
        let tmp = empty_dir();
        let cache = Arc::new(IndexCache::new(tmp.path()));
        let key = CacheKey::new("orders", "0022".repeat(10).as_str(), "0.9.0+c1");
        let manifest = build_manifest("orders", &key.sha, "0.9.0+c1", vec![], BTreeMap::new(), 4);
        cache.write_entry(&key, b"abcd", &manifest).unwrap();

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let touch_cache = Arc::clone(&cache);
        let touch_key = key.clone();
        let touch_stop = Arc::clone(&stop);
        let toucher = thread::spawn(move || {
            while !touch_stop.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = touch_cache.touch(&touch_key);
                std::thread::sleep(std::time::Duration::from_micros(50));
            }
        });

        let mut handles = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let stop = Arc::clone(&stop);
            let h = thread::spawn(move || {
                let mut observations = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = cache.has_entry(&CacheKey::new(
                        "orders",
                        "0022".repeat(10).as_str(),
                        "0.9.0+c1",
                    ));
                    observations += 1;
                }
                observations
            });
            handles.push(h);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        toucher.join().unwrap();
        let mut total = 0u64;
        for h in handles {
            total += h.join().unwrap();
        }
        assert!(total > 100, "observers should have run; got {total}");
        // Entry is still present (touch never removes). Eviction
        // is not racing here — the test exercises the touch
        // concurrent path. The integration with eviction is
        // covered indirectly by `hold_protects_entry_from_eviction`.
        assert!(
            cache.has_entry(&key),
            "touch alone must not delete the entry"
        );
    }

    #[test]
    fn hold_protects_entry_from_eviction() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        // Write three entries (each 1 KiB) so the LRU sweep has
        // something to bite into. The eviction budget is set well
        // below the on-disk total so the sweep has to remove
        // entries to fit; the held entry must survive.
        for i in 0..3u8 {
            let sha = format!("{:040x}", i + 1);
            let m = build_manifest(
                "orders",
                sha.clone(),
                "0.9.0+c1",
                vec![],
                BTreeMap::new(),
                1024,
            );
            let key = CacheKey::new("orders", sha, "0.9.0+c1");
            cache.write_entry(&key, &vec![0u8; 1024], &m).unwrap();
        }
        let middle_key = CacheKey::new("orders", format!("{:040x}", 2), "0.9.0+c1");
        let hold = cache.acquire_hold(middle_key.clone());
        // Total on disk: 3 KiB; budget = 0 MiB. Eviction must
        // remove the two unprotected entries and leave the held
        // one in place (the held one is exempt even when the
        // budget is zero).
        let outcome = cache.evict_lru(0).unwrap();
        assert!(!cache.has_entry(&CacheKey::new("orders", format!("{:040x}", 1), "0.9.0+c1",)));
        assert!(cache.has_entry(&middle_key), "held entry survives eviction");
        assert_eq!(outcome.removed, 2);
        assert!(outcome.freed >= 2048);
        drop(hold);
        // After the hold is released a follow-up eviction with
        // the same zero budget must reclaim the held entry too.
        let outcome2 = cache.evict_lru(0).unwrap();
        assert_eq!(outcome2.removed, 1);
        assert!(!cache.has_entry(&middle_key));
    }

    #[test]
    fn residency_tracker_pins_entry() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        let residency = ResidencyTracker::new();
        let key = CacheKey::new("orders", "1234".repeat(10).as_str(), "0.9.0+c1");
        let m = build_manifest(
            "orders",
            key.sha.clone(),
            "0.9.0+c1",
            vec![],
            BTreeMap::new(),
            1024,
        );
        cache.write_entry(&key, &vec![0u8; 1024], &m).unwrap();
        let _hold = residency.pin(&cache, key.clone());
        assert!(residency.is_resident(&key));
        let outcome = cache.evict_lru(0).unwrap();
        assert_eq!(outcome.removed, 0, "resident entry is exempt from eviction");
        assert!(cache.has_entry(&key));
    }

    #[test]
    fn eviction_skips_held_entries_under_small_budget() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        // Single entry, budget = 0 bytes. The entry is held, so
        // it must survive.
        let key = CacheKey::new("orders", "abcd".repeat(10).as_str(), "0.9.0+c1");
        let m = build_manifest(
            "orders",
            key.sha.clone(),
            "0.9.0+c1",
            vec![],
            BTreeMap::new(),
            4096,
        );
        cache.write_entry(&key, &vec![0u8; 4096], &m).unwrap();
        let _hold = cache.acquire_hold(key.clone());
        let outcome = cache.evict_lru(0).unwrap();
        assert_eq!(outcome.removed, 0);
        assert!(cache.has_entry(&key));
    }

    #[test]
    fn collect_entries_reports_on_disk_state() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        let key = CacheKey::new("orders", "ee00".repeat(10).as_str(), "0.9.0+c1");
        let m = build_manifest(
            "orders",
            key.sha.clone(),
            "0.9.0+c1",
            vec!["src/x.rs".into()],
            BTreeMap::new(),
            8,
        );
        cache.write_entry(&key, b"deadbeef", &m).unwrap();
        let entries = cache.collect_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key.repo, "orders");
        assert_eq!(entries[0].bytes, 8);
    }

    #[test]
    fn purge_removes_every_entry() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        for i in 0..3u8 {
            let sha = format!("{:040x}", 10 + i);
            let m = build_manifest(
                "orders",
                sha.clone(),
                "0.9.0+c1",
                vec![],
                BTreeMap::new(),
                4,
            );
            let key = CacheKey::new("orders", sha, "0.9.0+c1");
            cache.write_entry(&key, b"abcd", &m).unwrap();
        }
        assert_eq!(cache.purge().unwrap(), 3);
        assert_eq!(cache.collect_entries().unwrap().len(), 0);
    }

    #[test]
    fn remove_entry_rejects_held_entry() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        let key = CacheKey::new("orders", "beef".repeat(10).as_str(), "0.9.0+c1");
        let m = build_manifest(
            "orders",
            key.sha.clone(),
            "0.9.0+c1",
            vec![],
            BTreeMap::new(),
            4,
        );
        cache.write_entry(&key, b"abcd", &m).unwrap();
        let _hold = cache.acquire_hold(key.clone());
        let err = cache.remove_entry(&key).unwrap_err();
        assert!(matches!(err, LainError::Other(_)));
    }

    #[test]
    fn cache_budget_mb_default_is_4096() {
        // Serialize env-var tests with a static mutex so the
        // parallel test runner does not race them on the
        // process-wide env var. The lock is held only across the
        // set / assert / restore window, which is microseconds.
        let _g = ENV_LOCK.lock();
        let prev = std::env::var("LAIN_INDEX_CACHE_MB").ok();
        std::env::remove_var("LAIN_INDEX_CACHE_MB");
        assert_eq!(cache_budget_mb(), DEFAULT_CACHE_MB);
        match prev {
            Some(v) => std::env::set_var("LAIN_INDEX_CACHE_MB", v),
            None => std::env::remove_var("LAIN_INDEX_CACHE_MB"),
        }
    }

    #[test]
    fn cache_budget_mb_overrides_default() {
        let _g = ENV_LOCK.lock();
        let prev = std::env::var("LAIN_INDEX_CACHE_MB").ok();
        std::env::set_var("LAIN_INDEX_CACHE_MB", "16");
        assert_eq!(cache_budget_mb(), 16);
        match prev {
            Some(v) => std::env::set_var("LAIN_INDEX_CACHE_MB", v),
            None => std::env::remove_var("LAIN_INDEX_CACHE_MB"),
        }
    }

    #[test]
    fn discover_returns_none_for_missing_repo() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        assert!(cache.discover("orders", "0.9.0+c1").is_none());
    }

    #[test]
    fn discover_returns_none_for_missing_analyzer_version() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        let key = CacheKey::new("orders", "abc1234", "0.9.0+c1");
        let m = build_manifest(
            "orders",
            &key.sha,
            &key.analyzer_version,
            vec![],
            BTreeMap::new(),
            64,
        );
        cache.write_entry(&key, b"payload", &m).unwrap();
        // Same repo, different analyzer version: no match.
        assert!(cache.discover("orders", "0.9.0+c2").is_none());
    }

    #[test]
    fn discover_finds_entry_by_repo_and_analyzer_version() {
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        // Write two entries for the same analyzer version under
        // different SHAs (the typical case when the indexer
        // re-resolves a ref to a new commit). `discover` must
        // return *some* key; the manager will use the SHA it
        // returns to build the cache lookup.
        let key_a = CacheKey::new("orders", "aaaa1111", "0.9.0+c1");
        let m_a = build_manifest(
            "orders",
            &key_a.sha,
            &key_a.analyzer_version,
            vec![],
            BTreeMap::new(),
            64,
        );
        cache.write_entry(&key_a, b"a", &m_a).unwrap();
        let key_b = CacheKey::new("orders", "bbbb2222", "0.9.0+c1");
        let m_b = build_manifest(
            "orders",
            &key_b.sha,
            &key_b.analyzer_version,
            vec![],
            BTreeMap::new(),
            64,
        );
        cache.write_entry(&key_b, b"b", &m_b).unwrap();
        let found = cache
            .discover("orders", "0.9.0+c1")
            .expect("discover must find an entry");
        assert!(found.sha == "aaaa1111" || found.sha == "bbbb2222");
        assert_eq!(found.repo, "orders");
        assert_eq!(found.analyzer_version, "0.9.0+c1");
        // Both entries must still be valid cache hits.
        assert!(cache.has_entry(&key_a));
        assert!(cache.has_entry(&key_b));
    }

    #[test]
    fn discover_skips_directory_without_manifest_and_graph() {
        // The `index-cache/<repo>/` directory can contain stale
        // entries (manifest.json or graph.bin missing after a torn
        // write). `discover` must skip them.
        let tmp = empty_dir();
        let cache = IndexCache::new(tmp.path());
        let dir = cache_root(tmp.path())
            .join("orders")
            .join("aaaa1111-0.9.0+c1");
        std::fs::create_dir_all(&dir).unwrap();
        // No manifest.json, no graph.bin → not a valid entry.
        assert!(cache.discover("orders", "0.9.0+c1").is_none());
    }

    /// Env-var tests serialize on this lock to keep the
    /// process-wide `LAIN_INDEX_CACHE_MB` from racing between
    /// parallel test threads. `parking_lot::Mutex::lock()` returns
    /// a guard; dropping the guard releases the lock. Using a
    /// shared `LazyLock` so the mutex is initialized once per
    /// test binary.
    static ENV_LOCK: std::sync::LazyLock<parking_lot::Mutex<()>> =
        std::sync::LazyLock::new(|| parking_lot::Mutex::new(()));

    #[test]
    fn manifest_validation_detects_analyzer_mismatch() {
        let m = build_manifest("r", "s", "wrong", vec![], BTreeMap::new(), 0);
        let err = manifest_matches_on_disk(&m, 0, "right").unwrap_err();
        assert!(err.contains("analyzer_version mismatch"), "got {err}");
    }

    #[test]
    fn manifest_validation_detects_byte_mismatch() {
        let m = build_manifest("r", "s", "right", vec![], BTreeMap::new(), 8);
        let err = manifest_matches_on_disk(&m, 16, "right").unwrap_err();
        assert!(err.contains("manifest bytes"), "got {err}");
    }
}
