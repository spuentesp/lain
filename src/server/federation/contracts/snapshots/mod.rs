//! Snapshot records, jobs, residency, and the `from_snapshot`
//! projection (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §8.4–§8.5, §11).
//!
//! The split is by concern:
//!
//! - [`record`] (`PR 11`): the on-disk JSON record shape
//!   (`<data_dir>/snapshots/<snapshot_id>.json`), the
//!   `blake3(canonical_json{...})` snapshot id, and the
//!   `RepoSnapshotState` per-repo sub-state.
//! - [`jobs`] (`PR 11`): the per-`(repo, sha, analyzer_version)` job
//!   queue, dedup across snapshots, `LAIN_SNAPSHOT_WORKERS` worker
//!   count, and the 64-job limit (`busy` on overflow).
//! - [`manager`] (`PR 11`): the [`SnapshotManager`] that owns the
//!   on-disk records + the job runner + the resident federation
//!   table; the `prepare_snapshot` and `get_snapshot` MCP tools
//!   delegate to this. Residency uses the §8.5 LRU eviction rule
//!   (last touch wins) over a `BTreeMap` of `last_used_unix`
//!   keys; holds prevent eviction, and a busy `wait_ms` honours the
//!   tool's grace period before returning `busy`.
//!
//! `from_snapshot` (`§8.5`) builds a federation over
//! `PetgraphBackend::ephemeral()`: every `save()` is a no-op, the
//! in-memory `GraphDatabase` is fresh, and `project_graph(repo_id,
//! &GraphDatabase)` is the shared projection path used by the live
//! federation (same rewrite + cross-repo join semantics — same
//! node and edge sets from the same per-repo graph). After all repos
//! project, `rejoin_contracts` derives the snapshot's
//! `ContractIndex`.

pub mod jobs;
pub mod manager;
pub mod record;

pub use manager::{SnapshotFederation, SnapshotManager};
pub use record::{
    canonical_snapshot_id, snapshot_id_for, snapshot_path as snapshot_record_path,
    RepoSnapshotState, SnapshotInput, SnapshotRecord, SnapshotState, SNAPSHOT_ID_PREFIX,
};
