//! Contract federation — types and operations for cross-repo
//! provider/consumer/field joins.
//!
//! The split is by concern:
//!
//! - `model` (PR 3): the per-repo, sensor-written facts and the
//!   federation-level key types. No sensor logic, no joiner logic —
//!   just the data shapes every later task speaks.
//! - `normalize` (PR 5): URL → `NormalizedUrl` (§4.5). Pure
//!   functions; no graph or config dependencies.
//! - `route_match` (PR 5): provider template ↔ consumer template
//!   matching (§7.4). Pure logic; used by the joiner in PR 7.
//! - `config` (PR 7): `ContractFederationConfig` (§7.1) — the
//!   parsed-and-validated form of the contract-federation sections
//!   in `repos.yaml`. Pure data + validation; no graph or sensor
//!   dependencies.
//! - `index` (PR 7): `ContractIndex` (§4.3) — the per-federation
//!   shape derived by the joiner. Pure data.
//! - `joiner` (PR 7): `ContractJoiner::run(nodes, edges, config)`
//!   computing the desired `Binds` edge set and `ContractIndex`.
//! - `field_join` (PR 9): §7.5 four-rule case analysis that
//!   resolves each `FieldRef` against the response schema of the
//!   endpoints its call joins. The joiner step 5 calls into it.
//! - `mirrors` (PR 10): the per-repo bare
//!   `<data_dir>/mirrors/<repo>.git`, ref resolution (sha / sha
//!   prefix / `refs/…` / branch / tag), the one-`fetch-then-fail`
//!   policy, worktree add/remove, and the per-repo `File::lock`
//!   that serializes fetch + worktree add + worktree remove +
//!   worktree prune. `prune` runs at startup under the same lock.
//! - `index_cache` (PR 10): `<data_dir>/index-cache/<repo>/<sha>-<analyzer_version>/{graph.bin,manifest.json}`
//!   layout, manifest, atomic temp+rename write, LRU eviction past
//!   `LAIN_INDEX_CACHE_MB` (default 4096) with hold exemption for
//!   resident snapshot federations and running jobs.
//! - `snapshots` (PR 11): records (`<data_dir>/snapshots/<id>.json`),
//!   per-`(repo, sha, analyzer_version)` job runner, residency
//!   (`LAIN_SNAPSHOT_RESIDENT`), and the `from_snapshot`
//!   projection path. See `contracts/snapshots/mod.rs`.
//! - `digest` (PR 10): the canonical blake3 digest over the
//!   per-repo graph (every node sorted by id + every edge sorted by
//!   `(edge_type, source_id, target_id)`, each bincode-encoded after
//!   clearing `last_lsp_sync`, `last_git_sync`, `is_hydrated`, and
//!   `embedding`). Used by
//!   `tests/contracts_analyzer_digest.rs` to pin the
//!   sensor / normalizer / joiner output shape.

pub mod changed_files;
pub mod clients;
pub mod config;
pub mod coverage;
pub mod diff;
pub mod digest;
pub mod field_join;
pub mod index;
pub mod index_cache;
pub mod joiner;
pub mod mirrors;
pub mod model;
pub mod normalize;
pub mod route_match;
pub mod snapshots;

/// Per-module revision counter for the contract analyzer (§8.3).
///
/// Bump this whenever a change to the sensors, the normalizer, or
/// the joiner changes the per-repo graph shape: a new node kind, a
/// new edge type, a new field on `ContractFact`, a normalized URL
/// rewrite, a joiner confidence adjustment — anything that would
/// make the same fixture index to a different `graph.bin` than
/// before. The digest test in `tests/contracts_analyzer_digest.rs`
/// reads the committed `tests/fixtures/contracts/analyzer_digest.txt`,
/// recomputes the canonical blake3 digest, and compares the two;
/// a mismatch without a matching bump fails with instructions to
/// regenerate the fixture (`scripts/contracts-fixture.sh` runs the
/// fixture into a temp dir, indexes in snapshot mode, and writes the
/// new digest file). Bumping without a real change is harmless: the
/// digest will then match the new fixture, and the next mismatch
/// will be the next genuine change.
///
/// `analyzer_version = "<CARGO_PKG_VERSION>+c<CONTRACT_ANALYZER_REV>"`
/// (verbatim from §8.3) so cache entries are keyed on a tuple that
/// combines the build version and the analyzer revision. Two
/// different builds of the same analyzer revision share a cache
/// entry; one analyzer revision's cache is invisible to the next.
pub const CONTRACT_ANALYZER_REV: u32 = 3;

/// Build the analyzer-version string used as the cache-entry name
/// suffix and embedded in the snapshot record (`§8.3` + `§8.4`).
///
/// Format: `"<CARGO_PKG_VERSION>+c<CONTRACT_ANALYZER_REV>"`. The `+c`
/// sentinel is the §8.3 verbatim shape so a cache key derived
/// before this constant lands is mechanically distinct from one
/// derived after.
pub fn analyzer_version() -> String {
    format!("{}+c{}", env!("CARGO_PKG_VERSION"), CONTRACT_ANALYZER_REV)
}

#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod diff_tests;
#[cfg(test)]
mod index_tests;
#[cfg(test)]
mod joiner_tests;
#[cfg(test)]
mod model_tests;
#[cfg(test)]
mod scenario_tests;
