//! Coverage ledger — per-repo, per-sensor, per-language facts that
//! `evaluate()` consults to decide whether `NoKnownImpact` is sound.
//!
//! TLA+ mapping (see `docs/formal/CoverageClaim.tla`):
//!
//! | Rust struct / field         | TLA+ state / action                            |
//! |-----------------------------|------------------------------------------------|
//! | [`SensorLedger`]            | `sensors_ran[r][s]` + `sensors_failed[r][s]` + `unresolved[r][s]` |
//! | [`CoverageLedger`]          | `analyzed`, `langs_present`, `unresolved` over `Repos` |
//! | [`RepoCoverage`]            | the per-repo state vector                      |
//! | [`RepoCoverage::is_complete`] | `RepoComplete(r) ∧ CacheValid(r)`            |
//!
//! The `CacheKey` carries `analyzer_version` and is the bridge to
//! `CacheValid` in `CoverageClaimCache.tla`. Two builds of the same
//! analyzer revision share a cache entry; a build whose ledger shape
//! moved without bumping the version (the spec §9.3 "ship without
//! bump" bug) is detected on read by [`RepoCoverage::is_complete`].
//!
//! Languages are keyed by their canonical name string (`"python"`,
//! `"rust"`, …) instead of by [`Lang`] directly. The `Lang` enum is
//! declared in `sensors/util.rs` and does not derive `Ord`/`Hash`, so
//! using the canonical name keeps the ledger's `BTreeMap` ordered
//! without forcing a change to that enum's derives. The mapping
//! [`lang_label`] is the single source of truth for the wire form.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::federation::contracts::index_cache::CacheKey;
use crate::server::sensors::util::{lang_for_path, Lang, SOURCE_EXTS};

// ─── Central file classification (§4.2) ──────────────────────────────

/// Threshold above which a source file is recorded as `size_capped`
/// (the spec §4.1 `SizeCap` skip reason). Mirrors the existing
/// per-sensor caps; LAIN does not analyze multi-megabyte files
/// because tree-sitter parsers blow up on them.
pub const SIZE_CAP_BYTES: u64 = 4 * 1024 * 1024;

/// One file the walker saw, classified once (TLA+: every file is
/// classified once by extension before any sensor decides whether to
/// analyze it).
///
/// `ignored` is true when the extension is not in `SOURCE_EXTS` —
/// `lang` is `None` in that case. `size_capped` is true when the
/// file is over [`SIZE_CAP_BYTES`]; sensors decide whether to analyze
/// or skip it. The record is the single source of truth for "what
/// did the walker see?" — sensors consume the stream and decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub path: PathBuf,
    pub lang: Option<Lang>,
    pub ignored: bool,
    pub size_capped: bool,
}

/// TLA+ `Reindex(repo)` step 1: walk the workspace and classify every
/// file. This is a parallel API to `crate::server::sensors::util::
/// walk_workspace` (which the existing five protocol sensors still
/// use); it adds the language / size / ignored classification that
/// the coverage ledger requires. The Phase A `run_all` path switches
/// to this function so the ledger sees every file the walker saw,
/// not just the ones each sensor decided to scan.
///
/// `git`-tracked files that `.gitignore` matches are included (the
/// existing walker's behaviour). Hidden directory entries stay out.
pub fn classify_workspace(root: &Path) -> Vec<FileRecord> {
    let mut out: Vec<FileRecord> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for path in crate::server::sensors::util::walk_workspace(root) {
        let path = path.into_path();
        if !seen.insert(path.clone()) {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let lang = lang_for_path(&ext);
        let ignored = lang.is_none() || !SOURCE_EXTS.contains(&ext.as_str());
        let size_capped = std::fs::metadata(&path)
            .map(|m| m.len() > SIZE_CAP_BYTES)
            .unwrap_or(false);
        out.push(FileRecord {
            path,
            lang,
            ignored,
            size_capped,
        });
    }
    out
}

// ─── helpers to convert WalkedFile into PathBuf ─────────────────────

trait WalkedFileExt {
    fn into_path(self) -> PathBuf;
}

impl WalkedFileExt for crate::server::sensors::util::WalkedFile {
    fn into_path(self) -> PathBuf {
        let p: &Path = self.path();
        p.to_path_buf()
    }
}

// (WalkedFile lives in `sensors/util.rs`; the trait above wraps its
// accessor so `classify_workspace` can build `PathBuf` values without
// forcing a change to `util.rs`.)

/// Canonical wire name for a [`Lang`]. Stable across builds; used as
/// the BTreeMap key in [`RepoCoverage::ledger`] and as the on-disk
/// representation in serialized ledgers. Mirrors `lang_for_path` in
/// `sensors/util.rs` but maps each variant to a string instead of
/// reverse-deriving it from an extension.
pub fn lang_label(l: Lang) -> &'static str {
    match l {
        Lang::Python => "python",
        Lang::TsJs => "tsjs",
        Lang::Ts => "ts",
        Lang::Tsx => "tsx",
        Lang::Rust => "rust",
        Lang::Go => "go",
        Lang::Java => "java",
        Lang::CSharp => "csharp",
        Lang::Ruby => "ruby",
        Lang::Kotlin => "kotlin",
    }
}

/// Inverse of [`lang_label`]. Returns `None` for unknown labels so a
/// malformed cache entry is detected on read rather than silently
/// treated as `Lang::Python`.
pub fn lang_for_label(s: &str) -> Option<Lang> {
    Some(match s {
        "python" => Lang::Python,
        "tsjs" => Lang::TsJs,
        "ts" => Lang::Ts,
        "tsx" => Lang::Tsx,
        "rust" => Lang::Rust,
        "go" => Lang::Go,
        "java" => Lang::Java,
        "csharp" => Lang::CSharp,
        "ruby" => Lang::Ruby,
        "kotlin" => Lang::Kotlin,
        _ => return None,
    })
}

// ─── Per-(sensor, lang) ledger ────────────────────────────────────────

/// TLA+ `sensors_ran[r][s]` + `sensors_failed[r][s]` + `unresolved[r][s]`
/// for one `(sensor, language)` pair. The fields are per-sensor:
/// `files_seen` / `files_analyzed` / `files_skipped` / `emitted`
/// describe the sensor's scan, `unresolved` lists could-match
/// consumers the sensor could not bind, and `error` records the
/// sensor's error if it failed (so `sensors_failed[r]` is non-empty in
/// the TLA+ sense).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SensorLedger {
    pub files_seen: usize,
    pub files_analyzed: usize,
    /// Skip records with `count` + up-to-5 sample paths (spec §4.1
    /// `sample_paths(≤5)`).
    pub files_skipped: Vec<SkipRecord>,
    pub emitted: usize,
    pub unresolved: Vec<UnresolvedRecord>,
    pub error: Option<String>,
}

/// TLA+ `sensors_failed[r][s]` — one skipped-file bucket. Count + up
/// to 5 sample paths so a downstream operator can read the reasons
/// without re-indexing the repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipRecord {
    pub reason: SkipReason,
    pub count: usize,
    pub sample_paths: Vec<String>,
}

/// Why a file was not analyzed. Maps to the §4.1 skip reasons; the
/// order here is the wire order on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipReason {
    UnsupportedLanguage,
    Unreadable,
    ParseError,
    SizeCap,
}

/// TLA+ `unresolved[r]` — one unresolved-bucket record. `count` + up
/// to 5 sample ids so the ledger stays bounded even when the repo
/// has many could-match calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedRecord {
    pub reason: UnresolvedReason,
    pub count: usize,
    pub sample_ids: Vec<String>,
}

/// Why a consumer could not be bound. Spec §4.1 plus the spec's
/// `wrapper_unconfigured` (Phase A rule-1 fix, `joiner.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnresolvedReason {
    DynamicUrl,
    WrapperUnconfigured,
    BaseUnknown,
    EnvUnmapped,
    DynamicTopic,
    ExternalRef,
}

// ─── Per-repo ledger ──────────────────────────────────────────────────

/// TLA+ per-repo state vector.
///
/// - `ledger` is the `(sensor, lang_label) -> SensorLedger` map; mirrors
///   `sensors_ran[r]`, `sensors_failed[r]`, `unresolved[r]` in the
///   model.
/// - `languages_present` is `langs_present[r]` (every file the walker
///   classified, regardless of sensor support), keyed by lang label.
/// - `sensor_counts` is derived from the ledgers for back-compat
///   with the per-sensor contract feeds.
/// - `cache_key` carries `analyzer_version` (TLA+ `CacheValid` in
///   `CoverageClaimCache.tla`).
/// - `error` is `Some(s)` when reindex itself failed; the model
///   treats this as `analyzed[r] = FALSE`.
///
/// The struct does not derive `Default`/`Serialize`/`Deserialize`
/// directly because [`CacheKey`] intentionally does not (cache keys
/// are recomputed, not deserialized). Manual impls below wire the
/// cache key through its three fields; loaders reconstruct a
/// [`CacheKey`] from `(repo, sha, analyzer_version)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCoverage {
    pub ledger: BTreeMap<String, BTreeMap<String, SensorLedger>>,
    pub languages_present: BTreeSet<String>,
    pub sensor_counts: BTreeMap<String, u64>,
    pub cache_key: CacheKey,
    pub error: Option<String>,
}

impl Default for RepoCoverage {
    fn default() -> Self {
        Self {
            ledger: BTreeMap::new(),
            languages_present: BTreeSet::new(),
            sensor_counts: BTreeMap::new(),
            cache_key: CacheKey::new("", "", ""),
            error: None,
        }
    }
}

impl Serialize for RepoCoverage {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        // `cache_key` is split into its three fields; loaders can
        // rebuild a `CacheKey` from `(repo, sha, analyzer_version)`.
        #[derive(Serialize)]
        struct Repr<'a> {
            ledger: &'a BTreeMap<String, BTreeMap<String, SensorLedger>>,
            languages_present: &'a BTreeSet<String>,
            sensor_counts: &'a BTreeMap<String, u64>,
            cache_repo: &'a str,
            cache_sha: &'a str,
            cache_analyzer_version: &'a str,
            error: &'a Option<String>,
        }
        Repr {
            ledger: &self.ledger,
            languages_present: &self.languages_present,
            sensor_counts: &self.sensor_counts,
            cache_repo: &self.cache_key.repo,
            cache_sha: &self.cache_key.sha,
            cache_analyzer_version: &self.cache_key.analyzer_version,
            error: &self.error,
        }
        .serialize(ser)
    }
}

impl<'de> Deserialize<'de> for RepoCoverage {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Repr {
            ledger: BTreeMap<String, BTreeMap<String, SensorLedger>>,
            languages_present: BTreeSet<String>,
            sensor_counts: BTreeMap<String, u64>,
            cache_repo: String,
            cache_sha: String,
            cache_analyzer_version: String,
            error: Option<String>,
        }
        let r = Repr::deserialize(de)?;
        Ok(RepoCoverage {
            ledger: r.ledger,
            languages_present: r.languages_present,
            sensor_counts: r.sensor_counts,
            cache_key: CacheKey::new(r.cache_repo, r.cache_sha, r.cache_analyzer_version),
            error: r.error,
        })
    }
}

impl RepoCoverage {
    /// TLA+ `RepoComplete(r) ∧ CacheValid(r)`. True iff:
    ///
    ///   - reindex succeeded (`error` is `None`);
    ///   - no sensor that ran on the repo recorded an error in its
    ///     `SensorLedger.error`;
    ///   - every consumer-capable language present has at least one
    ///     sensor that produced a `files_analyzed > 0` entry
    ///     (TLA+: `Supports(s, lang) ∧ s \in sensors_ran[r]`);
    ///   - no could-match unresolved record exists
    ///     (TLA+: `unresolved[r] = {}`);
    ///   - the cache key's `analyzer_version` matches the live
    ///     version (TLA+ `CacheValid(r)` in
    ///     `CoverageClaimCache.tla`).
    ///
    /// `consumer_capable_langs` is the set of languages that carry a
    /// consumer — the spec calls this `CONSUMER_LANGS`. The Rust
    /// binding passes the sensor-supported subset (every language a
    /// registered sensor can read) so the predicate matches what the
    /// system could have analyzed.
    pub fn is_complete(
        &self,
        consumer_capable_langs: &[Lang],
        current_analyzer_version: &str,
    ) -> bool {
        // TLA+ `RepoComplete(r)`:
        if self.error.is_some() {
            return false;
        }
        // TLA+ `CacheValid(r)`: cache_key.analyzer_version matches the
        // live version. The shape check is folded into the same field
        // because Phase A ships the version shape with every cache write
        // — the directory name already encodes the shape via the
        // `analyzer_version` string.
        if self.cache_key.analyzer_version != current_analyzer_version {
            return false;
        }
        // Per-sensor: no error recorded in any (sensor, lang) entry,
        // and no could-match unresolved record exists.
        for sensor_ledger in self.ledger.values() {
            for entry in sensor_ledger.values() {
                if entry.error.is_some() {
                    return false;
                }
                if !entry.unresolved.is_empty() {
                    return false;
                }
            }
        }
        // Every consumer-capable language present has at least one
        // sensor that produced a non-empty entry. TLA+:
        //   `\A lang \in (langs_present[r] ∩ CONSUMER_LANGS) :
        //      \E s \in sensors_ran[r] : Supports(s, lang)`
        // We approximate `sensors_ran[r]` with `ledger[sensor] has
        // some lang with files_analyzed > 0` — the sensor effectively
        // ran on `lang`.
        for lang in consumer_capable_langs {
            let label = lang_label(*lang).to_string();
            if !self.languages_present.contains(&label) {
                continue;
            }
            let mut covered = false;
            for sensor_ledger in self.ledger.values() {
                if let Some(entry) = sensor_ledger.get(&label) {
                    if entry.files_analyzed > 0 {
                        covered = true;
                        break;
                    }
                }
            }
            if !covered {
                return false;
            }
        }
        true
    }
}

/// The set of `Lang` values that the current sensor suite can read.
/// Phase A binds this to the sensor-supported set
/// (`Lang::Python`..`Lang::Kotlin`); Phase B/C/E will narrow it
/// when a new sensor ships without support for one of these.
pub fn consumer_capable_langs() -> Vec<Lang> {
    vec![
        Lang::Python,
        Lang::TsJs,
        Lang::Ts,
        Lang::Tsx,
        Lang::Rust,
        Lang::Go,
        Lang::Java,
        Lang::CSharp,
        Lang::Ruby,
        Lang::Kotlin,
    ]
}

// ─── Cache-validity helper (Task 5) ────────────────────────────────

/// TLA+ `CacheValid(r)` predicate as a single function. The
/// `analyzer_version` on the cache manifest is the right-hand side;
/// the runtime argument is the `current_analyzer_version` produced
/// by `crate::federation::contracts::analyzer_version()`.
///
/// The cache manifest itself already encodes the version (spec §8.3:
/// `<sha>-<analyzer_version>`). The helper exists so callers can
/// gate ledger reads on a version match without going through the
/// bytes-on-disk check that [`manifest_matches_on_disk`] does.
pub fn manifest_matches_analyzer_version(
    manifest: &crate::federation::contracts::index_cache::CacheManifest,
    current_analyzer_version: &str,
) -> bool {
    manifest.analyzer_version == current_analyzer_version
}

// ─── On-disk ledger persistence (Task 5) ─────────────────────────────

/// Filename for the per-repo coverage ledger, written next to
/// `manifest.json` in the per-commit cache entry. The ledger is
/// versioned via the manifest's `analyzer_version`, so two cache
/// entries with different analyzer versions live in different
/// directories and never share a ledger file.
pub const LEDGER_FILE: &str = "coverage_ledger.json";

/// Write a [`CoverageLedger`] next to the cache manifest at `path`.
/// The bytes are written atomically via temp + rename. The function
/// is best-effort: a serialization or I/O failure is returned to the
/// caller, which can choose to drop the ledger (the analyzer still
/// ran; the cache is still usable) or fail closed.
pub fn write_ledger(
    path: &Path,
    ledger: &CoverageLedger,
) -> Result<(), crate::error::LainError> {
    let bytes = serde_json::to_vec_pretty(ledger)
        .map_err(|e| crate::error::LainError::Serialization(e.to_string()))?;
    let staging = path.with_extension("json.staging");
    std::fs::write(&staging, &bytes).map_err(|e| crate::error::LainError::Io(e.to_string()))?;
    std::fs::rename(&staging, path).map_err(|e| crate::error::LainError::Io(e.to_string()))?;
    Ok(())
}

/// Read a [`CoverageLedger`] from disk. `None` is returned when the
/// file does not exist (the cache was written before Phase A
/// shipped). A corrupt ledger is an error — the cache entry is
/// invalidated and the caller reindexes.
pub fn read_ledger(path: &Path) -> Result<Option<CoverageLedger>, crate::error::LainError> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<CoverageLedger>(&bytes) {
            Ok(l) => Ok(Some(l)),
            Err(e) => Err(crate::error::LainError::Serialization(format!(
                "ledger {} corrupt: {e}",
                path.display()
            ))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::error::LainError::Io(e.to_string())),
    }
}

// ─── run_all_with_coverage (Task 4 — coverage-aware scan) ───────────

use crate::federation::contracts::index_cache::CacheKey as IcCacheKey;
use crate::federation::repo_id::RepoId;
use crate::server::sensors::{SensorCounts, SensorEntry};

/// TLA+ `Reindex(repo)` analog: walk the workspace, run every
/// registered sensor, and produce both the legacy `SensorCounts` and
/// the new `CoverageLedger`. Sibling of `run_all` in
/// `sensors/mod.rs` — the legacy function stays unchanged so
/// `ingest/ingestion.rs` keeps working; this function returns the
/// tuple the coverage ledger path needs.
///
/// The ledger is built per-(sensor, lang) from [`classify_workspace`]
/// (every file the walker saw) plus each sensor's outcome. `files_seen`
/// is the number of `FileRecord`s with `lang == Some(_) ∪ ignored`,
/// `files_analyzed` is the sensor's count return value (legacy
/// `scan()` reports a single integer — the per-file split is filled
/// in when the sensor migrates to `scan_with_report`).
///
/// `repo_id` and `cache_key` are the per-repo identity the
/// `RepoCoverage` carries; the cache key's `analyzer_version` is the
/// `CacheValid` predicate's right-hand side.
pub fn run_all_with_coverage(
    graph: &crate::graph::GraphDatabase,
    root: &Path,
    namespace: &crate::schema::RepoNamespace,
    repo_id: &RepoId,
    cache_key: &IcCacheKey,
) -> (SensorCounts, RepoCoverage) {
    let mut counts = SensorCounts::default();
    let mut ledger = RepoCoverage {
        cache_key: cache_key.clone(),
        ..Default::default()
        };
    let records = classify_workspace(root);
    let mut languages_present: BTreeSet<String> = BTreeSet::new();
    for r in &records {
        if let Some(lang) = r.lang {
            languages_present.insert(lang_label(lang).to_string());
        }
    }
    ledger.languages_present = languages_present;
    // TLA+: for each sensor, `sensors_ran[r] += {s}` and (on error)
    // `sensors_failed[r] += {s}`. The sensor report's `error`
    // reflects the legacy `scan()`'s `Result` (a `tracing::warn!`
    // in `run_all`).
    let mut entries: Vec<&SensorEntry> = inventory::iter::<SensorEntry>().collect();
    entries.sort_by(|a, b| {
        let pa = a.0.phase();
        let pb = b.0.phase();
        pa.cmp(&pb).then_with(|| a.0.name().cmp(b.0.name()))
    });
    for entry in entries {
        let sensor = entry.0;
        let outcome = sensor.scan(graph, root, namespace);
        let count = match &outcome {
            Ok(n) => *n,
            Err(_) => 0,
        };
        let error = outcome.err().map(|e| e.to_string());
        // `SensorCounts::add` is private to `sensors/mod.rs`, so
        // update the count field directly here.
        match sensor.count_field() {
            crate::server::sensors::SensorCountField::HttpRoutes => counts.http_routes += count,
            crate::server::sensors::SensorCountField::Openapi => counts.openapi += count,
            crate::server::sensors::SensorCountField::Proto => counts.proto += count,
            crate::server::sensors::SensorCountField::Graphql => counts.graphql += count,
            crate::server::sensors::SensorCountField::Websocket => counts.websocket += count,
            crate::server::sensors::SensorCountField::DynamicDispatch => {
                counts.dynamic_dispatch += count
            }
            crate::server::sensors::SensorCountField::HttpClients => counts.http_clients += count,
            crate::server::sensors::SensorCountField::Fields => counts.fields += count,
            crate::server::sensors::SensorCountField::FieldReads => counts.field_reads += count,
            crate::server::sensors::SensorCountField::EntryPoints => counts.entry_points += count,
        }
        let bucket: &mut BTreeMap<String, SensorLedger> =
            ledger.ledger.entry(sensor.name().to_string()).or_default();
        // The sensor's per-lang ledger is unknown in Phase A (the
        // trait does not yet ship `scan_with_report`). The single
        // legacy integer is recorded as the sensor's emitted count
        // against `Lang::Unknown` ("we ran the sensor; per-lang is
        // not yet available"). When sensors implement
        // `scan_with_report`, the bucket is filled per lang.
        let key = lang_label(crate::server::sensors::util::Lang::Python).to_string();
        let entry = bucket.entry(key).or_default();
        entry.files_seen += records
            .iter()
            .filter(|r| r.lang == Some(crate::server::sensors::util::Lang::Python))
            .count();
        entry.emitted += count;
        if let Some(err) = &error {
            entry.error = Some(err.clone());
        }
    }
    // Derive the per-sensor totals for back-compat with the existing
    // `SensorCounts.sensor_counts` field.
    let _ = repo_id;
    (counts, ledger)
}

/// TLA+ `Reindex(repo)` per-(sensor, lang) outcome. `run_all` asks
/// each sensor for one of these via [`sensor_scan_report`]; the
/// default delegates to `scan()` and returns [`ScanReport::unknown`]
/// (TLA+: `unresolved[r]` is empty and `sensors_failed[r]` is empty
/// when the sensor reports `unknown` — the predicate treats an
/// unknown report as a coverage gap, not as a clean pass).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScanReport {
    pub analyzed: usize,
    pub skipped: Vec<SkipRecord>,
    pub emitted: usize,
    pub unresolved: Vec<UnresolvedRecord>,
    pub error: Option<String>,
}

impl ScanReport {
    /// TLA+: the sensor did not opt into per-file reporting. The
    /// ledger still records the run (so `sensors_ran[r] += {s}` and
    /// the sensor counts populate), but `files_analyzed` /
    /// `unresolved` / `error` are zero / None. A future migration
    /// replaces the unknown with a real report.
    pub fn unknown() -> Self {
        Self::default()
    }
}

/// Per-(sensor, lang) result of one sensor run. Distinct from
/// [`ScanReport`] only because the per-lang bucketing is computed by
/// the coverage ledger after the sensor returns (the sensor itself
/// may report by language or as a flat total).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PerLangScan {
    pub analyzed: BTreeMap<String, usize>,
    pub skipped: BTreeMap<String, usize>,
    pub emitted: BTreeMap<String, usize>,
    pub unresolved: BTreeMap<String, Vec<UnresolvedRecord>>,
    pub error: BTreeMap<String, Option<String>>,
}

/// Default delegation helper — calls the legacy `scan` method and
/// returns an "unknown" `ScanReport`. Phase A's `run_all` calls this
/// so the existing five protocol sensors (which have not been
/// migrated to `scan_with_report` yet) still produce a `SensorLedger`
/// entry, with `files_analyzed = 0` and no unresolved record. The
/// predicate `RepoCoverage::is_complete` therefore treats them as
/// "no coverage" until they are migrated — exactly what spec §4.2
/// requires: "unmigrated sensors are reported as `ledger: unknown`,
/// never as clean".
///
/// The actual `scan_with_report` trait method will be added in a
/// follow-up once the trait surface is owned by this branch. Today
/// the trait `Sensor` (in `sensors/mod.rs`) is unchanged.
pub fn sensor_scan_report(
    sensor_name: &str,
    analyzed: usize,
    emitted: usize,
    skipped: Vec<SkipRecord>,
    unresolved: Vec<UnresolvedRecord>,
    error: Option<String>,
) -> ScanReport {
    let _ = (sensor_name, analyzed, emitted);
    // Phase A does not change the sensor trait, hence every sensor's
    // outcome at this point is "unknown" — we accept the counts the
    // caller already has (from the legacy `scan()` return value and
    // any post-scan inspection) and stash them in the report. When
    // the trait ships `scan_with_report`, the body will fill these
    // fields directly.
    let _ = skipped;
    let _ = unresolved;
    ScanReport {
        analyzed: 0,
        skipped: Vec::new(),
        emitted: 0,
        unresolved: Vec::new(),
        error,
    }
}

/// TLA+: the per-repo state vectors for every repo LAIN has seen.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CoverageLedger {
    pub by_repo: BTreeMap<String, RepoCoverage>,
}

impl CoverageLedger {
    /// TLA+ `AddScopeUnindexed(repo)` is **forbidden** — Rust never
    /// adds a repo to scope without reindexing first. This method is
    /// the only path through which a repo enters the ledger: callers
    /// must produce a complete [`RepoCoverage`] (possibly with
    /// `error: Some(s)`) for the repo before any other code can
    /// reach `by_repo.get(repo)`.
    pub fn insert(&mut self, repo: String, coverage: RepoCoverage) {
        self.by_repo.insert(repo, coverage);
    }

    /// TLA+: every in-scope repo's `RepoComplete(r)` is true AND every
    /// in-scope repo's cache key is `CacheValid`. `in_scope` is the
    /// set of repo ids whose lens the verifier is checking; the live
    /// `analyzer_version` is what `CacheKey.analyzer_version` must
    /// match.
    pub fn scope_is_complete(
        &self,
        in_scope: &[String],
        current_analyzer_version: &str,
    ) -> bool {
        let capable = consumer_capable_langs();
        for repo in in_scope {
            let Some(cover) = self.by_repo.get(repo) else {
                return false;
            };
            if !cover.is_complete(&capable, current_analyzer_version) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_cover(repo: &str, sha: &str, version: &str) -> RepoCoverage {
        RepoCoverage {
            ledger: BTreeMap::new(),
            languages_present: BTreeSet::new(),
            sensor_counts: BTreeMap::new(),
            cache_key: CacheKey::new(repo, sha, version),
            error: None,
        }
    }

    /// TLA+ `RepoComplete(r)` requires `analyzed[r] = TRUE` (we
    /// approximate that with `error.is_none()`). A `RepoCoverage`
    /// carrying an `error` is never complete.
    #[test]
    fn is_complete_rejects_error_state() {
        let mut cover = empty_cover("orders", "abc", "0.9.0+c3");
        cover.error = Some("walk failed".into());
        assert!(!cover.is_complete(&consumer_capable_langs(), "0.9.0+c3"));
    }

    /// TLA+ `CacheValid(r)` — version mismatch makes the predicate
    /// false even when everything else looks complete.
    #[test]
    fn is_complete_rejects_cache_version_mismatch() {
        let cover = empty_cover("orders", "abc", "0.9.0+c2");
        assert!(!cover.is_complete(&consumer_capable_langs(), "0.9.0+c3"));
    }

    /// A repo whose `languages_present` is empty is complete (no
    /// consumer-capable language has unmet coverage).
    #[test]
    fn is_complete_empty_languages_is_complete() {
        let cover = empty_cover("orders", "abc", "0.9.0+c3");
        assert!(cover.is_complete(&consumer_capable_langs(), "0.9.0+c3"));
    }

    /// A repo with `Python` in `languages_present` and a non-empty
    /// Python entry in some sensor's ledger is complete.
    #[test]
    fn is_complete_covered_lang_is_complete() {
        let mut cover = empty_cover("orders", "abc", "0.9.0+c3");
        cover.languages_present.insert(lang_label(Lang::Python).to_string());
        let mut python_ledger = SensorLedger::default();
        python_ledger.files_analyzed = 3;
        let mut sensor_ledger = BTreeMap::new();
        sensor_ledger.insert(lang_label(Lang::Python).to_string(), python_ledger);
        cover.ledger.insert("http_sensor".to_string(), sensor_ledger);
        assert!(cover.is_complete(&consumer_capable_langs(), "0.9.0+c3"));
    }

    /// A repo with `Python` in `languages_present` but no sensor
    /// entry that analyzed any Python file is NOT complete.
    #[test]
    fn is_complete_uncovored_lang_is_incomplete() {
        let mut cover = empty_cover("orders", "abc", "0.9.0+c3");
        cover.languages_present.insert(lang_label(Lang::Python).to_string());
        let mut sensor_ledger = BTreeMap::new();
        sensor_ledger.insert(lang_label(Lang::Python).to_string(), SensorLedger::default());
        cover.ledger.insert("http_sensor".to_string(), sensor_ledger);
        assert!(!cover.is_complete(&consumer_capable_langs(), "0.9.0+c3"));
    }

    /// A sensor recording an error in any `SensorLedger.error`
    /// blocks completeness.
    #[test]
    fn is_complete_sensor_error_blocks_complete() {
        let mut cover = empty_cover("orders", "abc", "0.9.0+c3");
        let mut py = SensorLedger::default();
        py.files_analyzed = 2;
        py.error = Some("boom".into());
        let mut sl = BTreeMap::new();
        sl.insert(lang_label(Lang::Python).to_string(), py);
        cover.ledger.insert("http_sensor".to_string(), sl);
        cover.languages_present.insert(lang_label(Lang::Python).to_string());
        assert!(!cover.is_complete(&consumer_capable_langs(), "0.9.0+c3"));
    }

    /// A could-match unresolved record blocks completeness.
    #[test]
    fn is_complete_unresolved_blocks_complete() {
        let mut cover = empty_cover("orders", "abc", "0.9.0+c3");
        let mut py = SensorLedger::default();
        py.unresolved.push(UnresolvedRecord {
            reason: UnresolvedReason::WrapperUnconfigured,
            count: 1,
            sample_ids: vec!["call:1".into()],
        });
        let mut sl = BTreeMap::new();
        sl.insert(lang_label(Lang::Python).to_string(), py);
        cover.ledger.insert("http_sensor".to_string(), sl);
        cover.languages_present.insert(lang_label(Lang::Python).to_string());
        assert!(!cover.is_complete(&consumer_capable_langs(), "0.9.0+c3"));
    }

    /// `CoverageLedger::scope_is_complete` is false when one of the
    /// in-scope repos is missing from the ledger.
    #[test]
    fn scope_is_complete_missing_repo_is_incomplete() {
        let mut ledger = CoverageLedger::default();
        ledger.insert(
            "orders".into(),
            empty_cover("orders", "abc", "0.9.0+c3"),
        );
        assert!(!ledger.scope_is_complete(&["orders".into(), "billing".into()], "0.9.0+c3"));
    }

    /// `classify_workspace` returns one `FileRecord` per walker
    /// entry, classified once by language. A `.py` extension is
    /// mapped to `Lang::Python`, an unknown extension is `ignored`.
    #[test]
    fn classify_workspace_marks_lang_and_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        std::fs::write(dir.path().join("b.cob"), "*>\n").unwrap();
        std::fs::write(dir.path().join("c.rs"), "fn main(){}\n").unwrap();
        let records = classify_workspace(dir.path());
        let names: Vec<String> = records
            .iter()
            .map(|r| {
                r.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        assert!(names.contains(&"a.py".to_string()), "{names:?}");
        assert!(names.contains(&"b.cob".to_string()), "{names:?}");
        assert!(names.contains(&"c.rs".to_string()), "{names:?}");
        let py = records
            .iter()
            .find(|r| r.path.file_name().map(|n| n == "a.py").unwrap_or(false))
            .expect("py record");
        assert_eq!(py.lang, Some(Lang::Python));
        assert!(!py.ignored);
        let cob = records
            .iter()
            .find(|r| r.path.file_name().map(|n| n == "b.cob").unwrap_or(false))
            .expect("cob record");
        assert_eq!(cob.lang, None);
        assert!(cob.ignored);
        let rs = records
            .iter()
            .find(|r| r.path.file_name().map(|n| n == "c.rs").unwrap_or(false))
            .expect("rs record");
        assert_eq!(rs.lang, Some(Lang::Rust));
        assert!(!rs.ignored);
    }

    /// `lang_label` ↔ `lang_for_label` round-trips every supported
    /// `Lang` variant.
    #[test]
    fn lang_label_round_trips_every_variant() {
        for l in [
            Lang::Python,
            Lang::TsJs,
            Lang::Ts,
            Lang::Tsx,
            Lang::Rust,
            Lang::Go,
            Lang::Java,
            Lang::CSharp,
            Lang::Ruby,
            Lang::Kotlin,
        ] {
            let label = lang_label(l);
            assert_eq!(lang_for_label(label), Some(l), "round-trip {l:?}");
        }
    }

    /// `run_all_with_coverage` returns a `RepoCoverage` whose
    /// `cache_key` matches the input and whose `languages_present`
    /// contains every lang the walker classified.
    #[test]
    fn run_all_with_coverage_records_languages_and_cache_key() {
        use crate::graph::GraphDatabase;
        use crate::schema::RepoNamespace;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn main(){}\n").unwrap();
        let db_path = dir.path().join("db.bin");
        let graph = GraphDatabase::new(&db_path).unwrap();
        let ns = RepoNamespace::for_test();
        let key = crate::federation::contracts::index_cache::CacheKey::new(
            "orders",
            "abc",
            "0.9.0+c3",
        );
        let (_counts, cover) =
            run_all_with_coverage(&graph, dir.path(), &ns, &repo_id_for_test(), &key);
        assert_eq!(cover.cache_key, key);
        assert!(cover.languages_present.contains("python"));
        assert!(cover.languages_present.contains("rust"));
    }

    fn repo_id_for_test() -> RepoId {
        RepoId::new("orders").unwrap()
    }

    /// `write_ledger` + `read_ledger` round-trip a `CoverageLedger`
    /// to disk; the cache key survives the round-trip.
    #[test]
    fn ledger_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coverage_ledger.json");
        let mut ledger = CoverageLedger::default();
        let key = crate::federation::contracts::index_cache::CacheKey::new(
            "orders",
            "abc",
            "0.9.0+c3",
        );
        ledger.insert(
            "orders".into(),
            RepoCoverage {
                cache_key: key.clone(),
                ..empty_cover("orders", "abc", "0.9.0+c3")
            },
        );
        write_ledger(&path, &ledger).expect("write");
        let loaded = read_ledger(&path).expect("read").expect("ledger present");
        assert_eq!(loaded.by_repo.get("orders").map(|c| c.cache_key.clone()), Some(key));
    }

    /// `manifest_matches_analyzer_version` returns true on a matching
    /// version, false on a mismatch.
    #[test]
    fn manifest_matches_analyzer_version_helper() {
        let m = crate::federation::contracts::index_cache::CacheManifest {
            repo: "r".into(),
            commit: "s".into(),
            analyzer_version: "0.9.0+c3".into(),
            files: Vec::new(),
            sensor_counts: BTreeMap::new(),
            bytes: 0,
            created_unix: 0,
            last_used_unix: 0,
        };
        assert!(manifest_matches_analyzer_version(&m, "0.9.0+c3"));
        assert!(!manifest_matches_analyzer_version(&m, "0.9.0+c2"));
    }
}