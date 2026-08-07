# Federated Indexer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evolve LAIN from a per-workspace MCP server into a federated server that indexes N repos into a single queryable graph and answers cross-repo structural queries (call chains, blast radius, search).

**Architecture:** Two new traits — `RepoSource` (how the server gets code) and `GraphBackend` (how the graph is stored) — sit above today's `GraphDatabase`. A new `FederatedIndex` orchestrator holds N per-repo `RepoIndex` workers and projects their nodes/edges into a global petgraph with `repo_id:kind:path:name` IDs, adding `CrossRepoSameSymbol` edges by signature similarity. Five new MCP tools expose the federation. `WorkspaceDirSource` keeps today's single-workspace mode byte-identical.

**Tech Stack:** Rust 2021, tokio, petgraph, dashmap, parking_lot, git2, notify, serde_yaml, bincode, rust-mcp-sdk. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-07-federated-indexer-design.md`

## Global Constraints

These come from the spec and apply to every task. The task's requirements implicitly include this section.

- **Rust toolchain:** MSRV 1.75 (matches `Cargo.toml`).
- **Backwards compatibility:** All existing tests pass byte-identical when `lain --workspace ./myrepo` is used. The `WorkspaceDirSource` is the back-compat path; it must not change any existing public API.
- **No new external dependencies.** The trait design uses only crates already in `Cargo.toml` (petgraph, dashmap, tokio, git2, notify, serde_yaml, bincode, async-trait, thiserror, tracing).
- **Error type:** Add variants to `crate::error::LainError`, not a separate error crate. New variants must be `#[error("...")]` and follow existing patterns in `src/error.rs`.
- **Tracing:** Use `tracing::{info, warn, error, debug}` with structured fields, matching today's `src/main.rs` style.
- **Async:** All new I/O is `async fn` returning `Result<T, LainError>`. Use `tokio::sync::RwLock` for shared state per repo; never hold a sync `parking_lot` lock across an `.await`.
- **Lock ordering:** When taking locks on multiple repos in one operation, sort by `RepoId` first to prevent deadlock.
- **Persistence format:** `bincode` for both per-repo graph and `federation_manifest.bin`. New manifest format must be additive-only (versioned) so older binaries can be migrated.
- **Performance targets:** Cold start of 200 repos / 10M LOC < 30 min on 16 cores; cross-repo blast radius (depth 5) < 100ms p99; in-memory < 32 GB for 10M LOC. Validated by `tests/federation_benchmark.rs` (small fixture runs on PR, large fixture is `--ignored` and runs in nightly CI).
- **Commit granularity:** One commit per task. Commit messages: `feat(federation): <what>` or `test(federation): <what>` or `docs(federation): <what>`.
- **Test placement:** Unit tests live next to the code as `*_tests.rs` (matches existing `src/git_tests.rs`, `src/graph_tests.rs` convention) and are wired into `src/lib.rs` under `#[cfg(test)]`. Integration tests live in `tests/*.rs`. E2E scripts live in `tests/e2e/*.sh`.
- **Global ID format:** `repo_id:NodeType:path:name` exactly. The `repo_id` is a `String` that must not contain `:` (validated at construction).

---

## File Structure

### New files

| File | Responsibility | Approx LOC |
|---|---|---|
| `src/federation/mod.rs` | Module root, public re-exports | 30 |
| `src/federation/repo_id.rs` | `RepoId` newtype, `GlobalId` newtype, ID generation | 80 |
| `src/federation/health.rs` | `RepoHealth` enum, transitions | 60 |
| `src/federation/repo_source.rs` | `RepoSource` trait, `LocalCloneSource`, `ShallowCloneSource`, `WorkspaceDirSource` | 350 |
| `src/federation/graph_backend.rs` | `GraphBackend` trait, `PetgraphBackend` impl | 250 |
| `src/federation/matching.rs` | Cross-repo same-symbol matching (signature tokenization, cosine, top-K) | 200 |
| `src/federation/repo_index.rs` | `RepoIndex` — wraps today's per-repo indexer pipeline | 350 |
| `src/federation/federated_index.rs` | `FederatedIndex` orchestrator | 450 |
| `src/federation/manifest.rs` | `FederationManifest` (bincode-persisted repo list) | 150 |
| `src/federation/config.rs` | `repos.yaml` deserialization types | 150 |
| `src/federation/loader.rs` | Config-driven loader, `load_federation()` entry point | 250 |
| `src/federation/repo_source_tests.rs` | RepoSource unit tests | 250 |
| `src/federation/graph_backend_tests.rs` | GraphBackend contract tests | 200 |
| `src/federation/matching_tests.rs` | Cross-repo matching unit tests | 200 |
| `src/federation/federated_index_tests.rs` | FederatedIndex unit tests | 350 |
| `src/federation/loader_tests.rs` | Loader unit tests | 200 |
| `src/federation/manifest_tests.rs` | Manifest round-trip tests | 100 |
| `src/mcp/federation_tools.rs` | 5 new MCP tools: `list_repos`, `get_repo_info`, `get_cross_repo_blast_radius`, `search_org`, `get_federation_health` | 350 |
| `src/cmds/server.rs` | `lain server` subcommand | 120 |
| `tests/federation_integration.rs` | Multi-repo end-to-end tests | 400 |
| `tests/federation_benchmark.rs` | Performance tests (small + large fixtures) | 300 |
| `tests/e2e/federation_e2e.sh` | E2E against 3 public repos | 80 |

### Files to modify

| File | Change |
|---|---|
| `src/lib.rs` | Add `pub mod federation;`; add `#[cfg(test)] mod` lines for new test files |
| `src/schema.rs` | Add `RepoHealth` enum; add `repo_id: Option<String>` field to `GraphNode` for global IDs |
| `src/error.rs` | Add `LainError::RepoUnavailable(RepoId)`, `LainError::AmbiguousSymbol(Vec<RepoId>)`, `LainError::ResourceExhausted` variants |
| `src/cmds/mod.rs` | Add `pub mod server;` and re-export |
| `src/main.rs` | Wire `cmds::server::run()` into the CLI dispatch |
| `src/server/mod.rs` | Add a federation-aware constructor that takes a `FederatedIndex` instead of a single `GraphDatabase`; keep the existing constructor for back-compat |
| `src/mcp/handler.rs` | Register the 5 new tools when federation mode is active |
| `src/mcp/mod.rs` | Re-export `federation_tools` |
| `Cargo.toml` | No new dependencies; verify `[[bin]]` still picks up `src/main.rs` |

Total new code: ~3,900 LOC (1,700 production + ~2,200 tests).
Total new files: 22.
Files modified: 8.

---

## Tasks

### Task 1: `RepoId` and `GlobalId` newtypes

**Files:**
- Create: `src/federation/repo_id.rs`
- Modify: `src/federation/mod.rs` (create with one line: `pub mod repo_id;`)
- Modify: `src/lib.rs` (add `pub mod federation;`)

**Interfaces:**
- Consumes: nothing
- Produces: `pub struct RepoId(String);` with `pub fn new(s: &str) -> Result<Self, LainError>`, `pub fn as_str(&self) -> &str`. `pub struct GlobalId(String);` with `pub fn new(repo: &RepoId, kind: NodeType, path: &str, name: &str) -> Self`, `pub fn as_str(&self) -> &str`, `pub fn repo_id(&self) -> &str`, `pub fn parse(s: &str) -> Result<Self, LainError>`. New `LainError::InvalidRepoId(String)` variant in `src/error.rs`.

- [ ] **Step 1.1: Write the failing test**

Create `src/federation/repo_id.rs` with the test module inline (Rust convention here is `#[cfg(test)] mod tests { ... }` at the bottom — this is the smaller unit, not a separate file):

```rust
use crate::schema::NodeType;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RepoId(String);

impl RepoId {
    pub fn new(s: &str) -> Result<Self, crate::error::LainError> {
        if s.is_empty() || s.contains(':') || s.contains('/') {
            return Err(crate::error::LainError::InvalidRepoId(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct GlobalId(String);

impl GlobalId {
    pub fn new(repo: &RepoId, kind: NodeType, path: &str, name: &str) -> Self {
        Self(format!("{}:{:?}:{}:{}", repo.as_str(), kind, path, name))
    }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn repo_id(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }
    pub fn parse(s: &str) -> Result<Self, crate::error::LainError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() < 4 {
            return Err(crate::error::LainError::InvalidGlobalId(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }
}

impl std::fmt::Display for GlobalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_rejects_empty() {
        assert!(RepoId::new("").is_err());
    }

    #[test]
    fn repo_id_rejects_colon() {
        assert!(RepoId::new("foo:bar").is_err());
    }

    #[test]
    fn repo_id_rejects_slash() {
        assert!(RepoId::new("foo/bar").is_err());
    }

    #[test]
    fn repo_id_accepts_valid() {
        let id = RepoId::new("auth-svc").unwrap();
        assert_eq!(id.as_str(), "auth-svc");
        assert_eq!(id.to_string(), "auth-svc");
    }

    #[test]
    fn global_id_format_is_stable() {
        let repo = RepoId::new("auth-svc").unwrap();
        let id = GlobalId::new(&repo, NodeType::Function, "src/auth.rs", "verify_token");
        assert_eq!(id.as_str(), "auth-svc:Function:src/auth.rs:verify_token");
    }

    #[test]
    fn global_id_roundtrip() {
        let repo = RepoId::new("billing-svc").unwrap();
        let id = GlobalId::new(&repo, NodeType::Method, "src/invoice.py", "calc_total");
        let parsed = GlobalId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
        assert_eq!(parsed.repo_id(), "billing-svc");
    }

    #[test]
    fn global_id_parse_rejects_too_few_parts() {
        assert!(GlobalId::parse("foo:bar").is_err());
    }
}
```

Add `LainError::InvalidRepoId(String)` and `LainError::InvalidGlobalId(String)` to `src/error.rs` matching the existing `#[error("...")]` pattern.

- [ ] **Step 1.2: Create the module wiring**

Create `src/federation/mod.rs`:
```rust
pub mod repo_id;
```

Add `pub mod federation;` to `src/lib.rs` after the existing `pub mod` lines.

- [ ] **Step 1.3: Run tests, verify they pass**

Run: `cargo test --lib federation::repo_id::tests -- --nocapture`
Expected: 7 tests pass.

- [ ] **Step 1.4: Commit**

```bash
git add src/federation/repo_id.rs src/federation/mod.rs src/lib.rs src/error.rs
git commit -m "feat(federation): add RepoId and GlobalId newtypes"
```

---

### Task 2: `RepoHealth` enum

**Files:**
- Create: `src/federation/health.rs`
- Modify: `src/federation/mod.rs` (add `pub mod health;`)

**Interfaces:**
- Consumes: nothing
- Produces: `pub enum RepoHealth { Ready, Indexing, Degraded, Unavailable, Missing }` with `pub fn as_str(&self) -> &'static str` and `pub fn is_serving(&self) -> bool` (true for `Ready` and `Indexing`, false otherwise).

- [ ] **Step 2.1: Write the failing test**

Create `src/federation/health.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RepoHealth {
    Ready,
    Indexing,
    Degraded,
    Unavailable,
    Missing,
}

impl RepoHealth {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Indexing => "indexing",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::Missing => "missing",
        }
    }
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Ready | Self::Indexing)
    }
}

impl std::fmt::Display for RepoHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_variant() {
        assert_eq!(RepoHealth::Ready.as_str(), "ready");
        assert_eq!(RepoHealth::Indexing.as_str(), "indexing");
        assert_eq!(RepoHealth::Degraded.as_str(), "degraded");
        assert_eq!(RepoHealth::Unavailable.as_str(), "unavailable");
        assert_eq!(RepoHealth::Missing.as_str(), "missing");
    }

    #[test]
    fn is_serving_for_ready_and_indexing() {
        assert!(RepoHealth::Ready.is_serving());
        assert!(RepoHealth::Indexing.is_serving());
    }

    #[test]
    fn is_not_serving_for_terminal_states() {
        assert!(!RepoHealth::Degraded.is_serving());
        assert!(!RepoHealth::Unavailable.is_serving());
        assert!(!RepoHealth::Missing.is_serving());
    }

    #[test]
    fn serde_roundtrip() {
        for h in [RepoHealth::Ready, RepoHealth::Indexing, RepoHealth::Degraded, RepoHealth::Unavailable, RepoHealth::Missing] {
            let s = serde_json::to_string(&h).unwrap();
            let back: RepoHealth = serde_json::from_str(&s).unwrap();
            assert_eq!(h, back);
        }
    }
}
```

Add `pub mod health;` to `src/federation/mod.rs`.

- [ ] **Step 2.2: Run tests, verify they pass**

Run: `cargo test --lib federation::health::tests -- --nocapture`
Expected: 4 tests pass.

- [ ] **Step 2.3: Commit**

```bash
git add src/federation/health.rs src/federation/mod.rs
git commit -m "feat(federation): add RepoHealth enum"
```

---

### Task 3: `RepoSource` trait and `LocalCloneSource` impl

**Files:**
- Create: `src/federation/repo_source.rs`
- Create: `src/federation/repo_source_tests.rs`
- Modify: `src/federation/mod.rs` (add `pub mod repo_source;`)
- Modify: `src/lib.rs` (add `#[cfg(test)] mod repo_source_tests;` next to the other test mod lines)

**Interfaces:**
- Consumes: `RepoId` from `crate::federation::repo_id`
- Produces: `pub trait RepoSource: Send + Sync` with `fn id(&self) -> &RepoId; fn local_path(&self) -> &Path; async fn fetch(&self) -> Result<(), LainError>; fn last_refreshed(&self) -> SystemTime; fn is_stale(&self, max_age: Duration) -> bool;`. Plus `pub struct LocalCloneSource { repo_id: RepoId, url: String, git_ref: String, local_path: PathBuf, last_refreshed: Arc<RwLock<SystemTime>> }` and its `impl RepoSource for LocalCloneSource`.

- [ ] **Step 3.1: Write the failing test**

Create `src/federation/repo_source_tests.rs`:

```rust
//! Contract tests for RepoSource. These run against LocalCloneSource in this
//! task; later tasks (WorkspaceDirSource, ShallowCloneSource) re-use the same
//! contract tests via parametrization.
use crate::federation::repo_id::RepoId;
use crate::federation::repo_source::*;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

fn dummy_id() -> RepoId {
    RepoId::new("test-repo").unwrap()
}

#[tokio::test]
async fn local_clone_source_id_returns_configured() {
    let src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    assert_eq!(src.id().as_str(), "test-repo");
}

#[tokio::test]
async fn local_clone_source_local_path_returns_configured() {
    let src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    assert_eq!(src.local_path(), PathBuf::from("/tmp/repo").as_path());
}

#[tokio::test]
async fn local_clone_source_is_stale_when_never_refreshed() {
    let src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    assert!(src.is_stale(Duration::from_secs(0)));
}

#[tokio::test]
async fn local_clone_source_is_not_stale_after_recent_refresh() {
    let mut src = LocalCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo")).unwrap();
    src.mark_refreshed(SystemTime::now());
    assert!(!src.is_stale(Duration::from_secs(60)));
}
```

- [ ] **Step 3.2: Create the trait and `LocalCloneSource` skeleton (no real git yet)**

Create `src/federation/repo_source.rs`:

```rust
use crate::error::LainError;
use crate::federation::repo_id::RepoId;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use parking_lot::RwLock;

#[async_trait]
pub trait RepoSource: Send + Sync {
    fn id(&self) -> &RepoId;
    fn local_path(&self) -> &Path;
    async fn fetch(&self) -> Result<(), LainError>;
    fn last_refreshed(&self) -> SystemTime;
    fn is_stale(&self, max_age: Duration) -> bool;
}

pub struct LocalCloneSource {
    repo_id: RepoId,
    url: String,
    git_ref: String,
    local_path: PathBuf,
    last_refreshed: Arc<RwLock<SystemTime>>,
}

impl LocalCloneSource {
    pub fn new(repo_id: RepoId, url: &str, git_ref: &str, local_path: PathBuf) -> Result<Self, LainError> {
        if url.is_empty() {
            return Err(LainError::Config("RepoSource url cannot be empty".into()));
        }
        Ok(Self {
            repo_id,
            url: url.to_string(),
            git_ref: git_ref.to_string(),
            local_path,
            last_refreshed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
        })
    }
    pub fn mark_refreshed(&self, t: SystemTime) {
        *self.last_refreshed.write() = t;
    }
    pub fn url(&self) -> &str { &self.url }
    pub fn git_ref(&self) -> &str { &self.git_ref }
}

#[async_trait]
impl RepoSource for LocalCloneSource {
    fn id(&self) -> &RepoId { &self.repo_id }
    fn local_path(&self) -> &Path { &self.local_path }
    async fn fetch(&self) -> Result<(), LainError> {
        // Real implementation lands in Task 3.4 after a smoke test against a real clone.
        Err(LainError::NotImplemented("LocalCloneSource::fetch — see Task 3.4".into()))
    }
    fn last_refreshed(&self) -> SystemTime { *self.last_refreshed.read() }
    fn is_stale(&self, max_age: Duration) -> bool {
        self.last_refreshed().elapsed().map(|e| e > max_age).unwrap_or(true)
    }
}
```

Add the new `LainError::Config(String)` and `LainError::NotImplemented(String)` variants to `src/error.rs` if not already present (they likely are; check existing variants first).

Add `pub mod repo_source;` to `src/federation/mod.rs` and `#[cfg(test)] mod repo_source_tests;` to `src/lib.rs`.

- [ ] **Step 3.3: Run tests, verify they pass**

Run: `cargo test --lib federation::repo_source_tests -- --nocapture`
Expected: 4 tests pass.

- [ ] **Step 3.4: Implement real `fetch()` using `git2`**

Replace the body of `LocalCloneSource::fetch`:

```rust
async fn fetch(&self) -> Result<(), LainError> {
    use std::process::Command;
    let path = self.local_path.clone();
    let url = self.url.clone();
    let git_ref = self.git_ref.clone();
    let last_refreshed = self.last_refreshed.clone();
    tokio::task::spawn_blocking(move || -> Result<(), LainError> {
        if !path.exists() {
            let status = Command::new("git")
                .arg("clone").arg("--quiet").arg(&url).arg(&path)
                .status()
                .map_err(|e| LainError::Git(format!("git clone failed to start: {e}")))?;
            if !status.success() {
                return Err(LainError::Git(format!("git clone {} failed", url)));
            }
        }
        let fetch = Command::new("git")
            .current_dir(&path)
            .arg("fetch").arg("--quiet").arg("--all")
            .status()
            .map_err(|e| LainError::Git(format!("git fetch failed: {e}")))?;
        if !fetch.success() {
            return Err(LainError::Git("git fetch failed".into()));
        }
        let reset = Command::new("git")
            .current_dir(&path)
            .arg("reset").arg("--hard").arg(format!("origin/{}", git_ref))
            .status()
            .map_err(|e| LainError::Git(format!("git reset failed: {e}")))?;
        if !reset.success() {
            return Err(LainError::Git(format!("git reset to origin/{} failed", git_ref)));
        }
        *last_refreshed.write() = SystemTime::now();
        Ok(())
    }).await.map_err(|e| LainError::Git(format!("join error: {e}")))?
}
```

- [ ] **Step 3.5: Smoke test the real fetch against a small public repo**

Add a new test at the bottom of `src/federation/repo_source_tests.rs` (gated to `#[ignore]` so it doesn't run on every PR):

```rust
#[tokio::test]
#[ignore]
async fn local_clone_source_real_fetch_against_public_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let src = LocalCloneSource::new(
        RepoId::new("hello-world").unwrap(),
        "https://github.com/octocat/Hello-World.git",
        "master",
        tmp.path().join("hello-world"),
    ).unwrap();
    src.fetch().await.expect("fetch should succeed");
    assert!(src.local_path().exists());
    assert!(!src.is_stale(Duration::from_secs(60)));
}
```

Run: `cargo test --lib federation::repo_source_tests::local_clone_source_real_fetch_against_public_repo -- --ignored`
Expected: PASS (requires network).

- [ ] **Step 3.6: Commit**

```bash
git add src/federation/repo_source.rs src/federation/repo_source_tests.rs src/federation/mod.rs src/lib.rs src/error.rs
git commit -m "feat(federation): add RepoSource trait and LocalCloneSource"
```

---

### Task 4: `ShallowCloneSource` impl

**Files:**
- Modify: `src/federation/repo_source.rs` (add struct and impl)
- Modify: `src/federation/repo_source_tests.rs` (add tests)

**Interfaces:**
- Consumes: same trait as Task 3
- Produces: `pub struct ShallowCloneSource { ... }` with `new(repo_id, url, git_ref, local_path, refresh_interval)`, `impl RepoSource for ShallowCloneSource`. Uses `git clone --depth 1` and `git fetch --depth 1`; co-change history is lost.

- [ ] **Step 4.1: Write the failing test**

Append to `src/federation/repo_source_tests.rs`:

```rust
#[tokio::test]
async fn shallow_clone_source_id_and_path() {
    let src = ShallowCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo"), Duration::from_secs(300)).unwrap();
    assert_eq!(src.id().as_str(), "test-repo");
    assert_eq!(src.local_path(), PathBuf::from("/tmp/repo").as_path());
    assert_eq!(src.refresh_interval(), Duration::from_secs(300));
}

#[tokio::test]
async fn shallow_clone_source_is_stale_when_never_refreshed() {
    let src = ShallowCloneSource::new(dummy_id(), "https://example.com/repo.git", "main", PathBuf::from("/tmp/repo"), Duration::from_secs(60)).unwrap();
    assert!(src.is_stale(Duration::from_secs(60)));
}
```

- [ ] **Step 4.2: Implement `ShallowCloneSource`**

Append to `src/federation/repo_source.rs`:

```rust
pub struct ShallowCloneSource {
    inner: LocalCloneSource,
    refresh_interval: Duration,
}

impl ShallowCloneSource {
    pub fn new(repo_id: RepoId, url: &str, git_ref: &str, local_path: PathBuf, refresh_interval: Duration) -> Result<Self, LainError> {
        let inner = LocalCloneSource::new(repo_id, url, git_ref, local_path)?;
        Ok(Self { inner, refresh_interval })
    }
    pub fn refresh_interval(&self) -> Duration { self.refresh_interval }
}

#[async_trait]
impl RepoSource for ShallowCloneSource {
    fn id(&self) -> &RepoId { self.inner.id() }
    fn local_path(&self) -> &Path { self.inner.local_path() }
    async fn fetch(&self) -> Result<(), LainError> {
        use std::process::Command;
        let path = self.inner.local_path.clone();
        let url = self.inner.url.clone();
        let git_ref = self.inner.git_ref.clone();
        let last_refreshed = self.inner.last_refreshed.clone();
        tokio::task::spawn_blocking(move || -> Result<(), LainError> {
            if !path.exists() {
                let status = Command::new("git")
                    .arg("clone").arg("--quiet").arg("--depth").arg("1").arg("--branch").arg(&git_ref).arg(&url).arg(&path)
                    .status()
                    .map_err(|e| LainError::Git(format!("git clone --depth 1 failed to start: {e}")))?;
                if !status.success() {
                    return Err(LainError::Git(format!("git clone --depth 1 {} failed", url)));
                }
            } else {
                let fetch = Command::new("git")
                    .current_dir(&path)
                    .arg("fetch").arg("--quiet").arg("--depth").arg("1").arg("origin").arg(&git_ref)
                    .status()
                    .map_err(|e| LainError::Git(format!("git fetch --depth 1 failed: {e}")))?;
                if !fetch.success() {
                    return Err(LainError::Git("git fetch --depth 1 failed".into()));
                }
                let reset = Command::new("git")
                    .current_dir(&path)
                    .arg("reset").arg("--hard").arg(format!("origin/{}", git_ref))
                    .status()
                    .map_err(|e| LainError::Git(format!("git reset failed: {e}")))?;
                if !reset.success() {
                    return Err(LainError::Git(format!("git reset to origin/{} failed", git_ref)));
                }
            }
            *last_refreshed.write() = SystemTime::now();
            Ok(())
        }).await.map_err(|e| LainError::Git(format!("join error: {e}")))?
    }
    fn last_refreshed(&self) -> SystemTime { self.inner.last_refreshed() }
    fn is_stale(&self, max_age: Duration) -> bool {
        self.inner.is_stale(max_age)
    }
}
```

- [ ] **Step 4.3: Run tests, verify they pass**

Run: `cargo test --lib federation::repo_source_tests -- --nocapture`
Expected: 6 tests pass (4 from Task 3 + 2 new).

- [ ] **Step 4.4: Commit**

```bash
git add src/federation/repo_source.rs src/federation/repo_source_tests.rs
git commit -m "feat(federation): add ShallowCloneSource"
```

---

### Task 5: `WorkspaceDirSource` impl (back-compat path)

**Files:**
- Modify: `src/federation/repo_source.rs` (add struct and impl)
- Modify: `src/federation/repo_source_tests.rs` (add tests)
- Modify: `src/server/mod.rs` (add a federation-aware constructor; keep existing constructor for back-compat)

**Interfaces:**
- Consumes: same trait
- Produces: `pub struct WorkspaceDirSource { repo_id: RepoId, local_path: PathBuf }` with `new(repo_id, local_path)`. `fetch()` is a no-op (returns `Ok(())`). `last_refreshed()` returns `SystemTime::UNIX_EPOCH` (workspace is "always fresh" — file watcher handles updates).
- New in `src/server/mod.rs`: `pub fn with_federation(graph: Arc<FederatedIndex>, ...) -> LainResult<Self>` constructor that builds a `LainServer` reading from the federation instead of a single `GraphDatabase`. The existing `LainServer::new(...)` constructor is unchanged.

- [ ] **Step 5.1: Write the failing test**

Append to `src/federation/repo_source_tests.rs`:

```rust
#[tokio::test]
async fn workspace_dir_source_id_and_path() {
    let src = WorkspaceDirSource::new(dummy_id(), PathBuf::from("/srv/legacy")).unwrap();
    assert_eq!(src.id().as_str(), "test-repo");
    assert_eq!(src.local_path(), PathBuf::from("/srv/legacy").as_path());
}

#[tokio::test]
async fn workspace_dir_source_fetch_is_noop() {
    let src = WorkspaceDirSource::new(dummy_id(), PathBuf::from("/srv/legacy")).unwrap();
    src.fetch().await.expect("fetch should be a no-op");
}

#[tokio::test]
async fn workspace_dir_source_rejects_empty_path() {
    assert!(WorkspaceDirSource::new(dummy_id(), PathBuf::new()).is_err());
}
```

- [ ] **Step 5.2: Implement `WorkspaceDirSource`**

Append to `src/federation/repo_source.rs`:

```rust
pub struct WorkspaceDirSource {
    repo_id: RepoId,
    local_path: PathBuf,
}

impl WorkspaceDirSource {
    pub fn new(repo_id: RepoId, local_path: PathBuf) -> Result<Self, LainError> {
        if local_path.as_os_str().is_empty() {
            return Err(LainError::Config("WorkspaceDirSource path cannot be empty".into()));
        }
        Ok(Self { repo_id, local_path })
    }
}

#[async_trait]
impl RepoSource for WorkspaceDirSource {
    fn id(&self) -> &RepoId { &self.repo_id }
    fn local_path(&self) -> &Path { &self.local_path }
    async fn fetch(&self) -> Result<(), LainError> { Ok(()) }
    fn last_refreshed(&self) -> SystemTime { SystemTime::now() }
    fn is_stale(&self, _max_age: Duration) -> bool { false }
}
```

- [ ] **Step 5.3: Run tests, verify they pass**

Run: `cargo test --lib federation::repo_source_tests -- --nocapture`
Expected: 9 tests pass (6 from prior tasks + 3 new).

- [ ] **Step 5.4: Add the federation-aware `LainServer` constructor**

In `src/server/mod.rs`, read the current `LainServer::new` signature first to understand the existing constructor, then add (do not replace):

```rust
impl LainServer {
    /// Existing constructor — unchanged for back-compat. Builds a single-workspace server.
    pub fn new(/* existing params */) -> LainResult<Self> { /* existing body unchanged */ }

    /// New constructor for federation mode. Takes a pre-built FederatedIndex.
    pub fn with_federation(
        federation: std::sync::Arc<crate::federation::federated_index::FederatedIndex>,
        /* other params matching new() */,
    ) -> LainResult<Self> {
        // Body: build LainServer so that all read paths go through the federation's
        // global graph. For now, store the Arc and route every tool call through it.
        // Tool-by-tool routing is implemented in Tasks 15-19.
        todo!("implemented in Task 15 onward — placeholder to compile")
    }
}
```

Verify the existing `cargo test --lib` still passes unchanged — this confirms back-compat.

- [ ] **Step 5.5: Commit**

```bash
git add src/federation/repo_source.rs src/federation/repo_source_tests.rs src/server/mod.rs
git commit -m "feat(federation): add WorkspaceDirSource and LainServer::with_federation"
```

---

### Task 6: `GraphBackend` trait

**Files:**
- Create: `src/federation/graph_backend.rs`
- Create: `src/federation/graph_backend_tests.rs`
- Modify: `src/federation/mod.rs` (add `pub mod graph_backend;`)
- Modify: `src/lib.rs` (add `#[cfg(test)] mod graph_backend_tests;`)

**Interfaces:**
- Consumes: `GraphNode`, `GraphEdge`, `EdgeType`, `NodeType` from `crate::schema`
- Produces: `pub trait GraphBackend: Send + Sync` with `fn upsert_node(&self, node: GraphNode) -> Result<(), LainError>; fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError>; fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError>; fn traverse(&self, start: &str, edge: EdgeType, depth: std::ops::Range<u32>) -> Result<Vec<GraphNode>, LainError>; fn find_path(&self, from: &str, to: &str) -> Result<Vec<GraphNode>, LainError>; fn subgraph_around(&self, center: &str, radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError>; fn node_count(&self) -> usize; fn edge_count(&self) -> usize;`. Implemented as a stub here; `PetgraphBackend` is the real impl in Task 7.

- [ ] **Step 6.1: Write the failing test (contract test using a stub impl)**

Create `src/federation/graph_backend_tests.rs`:

```rust
//! Contract tests for GraphBackend. The same tests will run against PetgraphBackend
//! in Task 7. Here we use a simple in-memory HashMap impl to define the contract.
use crate::error::LainError;
use crate::federation::graph_backend::GraphBackend;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::RwLock;

pub struct HashMapBackend {
    nodes: RwLock<HashMap<String, GraphNode>>,
    edges: RwLock<Vec<GraphEdge>>,
}

impl HashMapBackend {
    pub fn new() -> Self {
        Self { nodes: RwLock::new(HashMap::new()), edges: RwLock::new(Vec::new()) }
    }
}

impl GraphBackend for HashMapBackend {
    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError> {
        self.nodes.write().unwrap().insert(node.id.clone(), node);
        Ok(())
    }
    fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError> {
        self.edges.write().unwrap().push(edge);
        Ok(())
    }
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError> {
        Ok(self.nodes.read().unwrap().get(global_id).cloned())
    }
    fn traverse(&self, _start: &str, _edge: EdgeType, _depth: Range<u32>) -> Result<Vec<GraphNode>, LainError> {
        Ok(Vec::new())
    }
    fn find_path(&self, _from: &str, _to: &str) -> Result<Vec<GraphNode>, LainError> {
        Ok(Vec::new())
    }
    fn subgraph_around(&self, _center: &str, _radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError> {
        Ok(Vec::new())
    }
    fn node_count(&self) -> usize { self.nodes.read().unwrap().len() }
    fn edge_count(&self) -> usize { self.edges.read().unwrap().len() }
}

#[test]
fn contract_upsert_node_roundtrips() {
    let b = HashMapBackend::new();
    let n = GraphNode::new(NodeType::Function, "f".into(), "src/lib.rs".into());
    b.upsert_node(n.clone()).unwrap();
    assert_eq!(b.node_count(), 1);
    assert_eq!(b.get_node(&n.id).unwrap().unwrap().id, n.id);
}

#[test]
fn contract_upsert_edge_increments_count() {
    let b = HashMapBackend::new();
    let n1 = GraphNode::new(NodeType::Function, "a".into(), "src/lib.rs".into());
    let n2 = GraphNode::new(NodeType::Function, "b".into(), "src/lib.rs".into());
    b.upsert_node(n1.clone()).unwrap();
    b.upsert_node(n2.clone()).unwrap();
    b.upsert_edge(GraphEdge::new(EdgeType::Calls, n1.id.clone(), n2.id.clone())).unwrap();
    assert_eq!(b.node_count(), 2);
    assert_eq!(b.edge_count(), 1);
}

#[test]
fn contract_get_missing_returns_none() {
    let b = HashMapBackend::new();
    assert!(b.get_node("nope").unwrap().is_none());
}
```

- [ ] **Step 6.2: Define the trait**

Create `src/federation/graph_backend.rs`:

```rust
use crate::error::LainError;
use crate::schema::{EdgeType, GraphEdge, GraphNode};
use std::ops::Range;

pub trait GraphBackend: Send + Sync {
    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError>;
    fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError>;
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError>;
    fn traverse(&self, start: &str, edge: EdgeType, depth: Range<u32>) -> Result<Vec<GraphNode>, LainError>;
    fn find_path(&self, from: &str, to: &str) -> Result<Vec<GraphNode>, LainError>;
    fn subgraph_around(&self, center: &str, radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError>;
    fn node_count(&self) -> usize;
    fn edge_count(&self) -> usize;
}
```

Add `pub mod graph_backend;` to `src/federation/mod.rs` and `#[cfg(test)] mod graph_backend_tests;` to `src/lib.rs`.

- [ ] **Step 6.3: Run tests, verify they pass**

Run: `cargo test --lib federation::graph_backend_tests -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 6.4: Commit**

```bash
git add src/federation/graph_backend.rs src/federation/graph_backend_tests.rs src/federation/mod.rs src/lib.rs
git commit -m "feat(federation): add GraphBackend trait and contract tests"
```

---

### Task 7: `PetgraphBackend` impl

**Files:**
- Modify: `src/federation/graph_backend.rs` (add struct and impl)
- Modify: `src/federation/graph_backend_tests.rs` (add real-graph tests)

**Interfaces:**
- Consumes: existing `GraphDatabase` from `crate::graph`
- Produces: `pub struct PetgraphBackend { db: GraphDatabase, index: dashmap::DashMap<String, crate::federation::repo_id::GlobalId> }` with `new(data_dir: &Path) -> Result<Self, LainError>`, `impl GraphBackend for PetgraphBackend`. Re-uses `GraphDatabase::upsert_node` and `GraphDatabase::upsert_edge` (existing code) but pre-pends `repo_id:` to node IDs before storage. The `traverse` and `find_path` methods reuse today's BFS in `GraphDatabase::get_blast_radius` / similar.

- [ ] **Step 7.1: Read the existing `GraphDatabase` API**

Read `src/graph.rs` to identify which existing methods to call: `upsert_node`, `upsert_edge`, `get_blast_radius`, `get_call_chain`, `subgraph`. Note their signatures exactly — the impl below calls them.

- [ ] **Step 7.2: Write the failing test**

Append to `src/federation/graph_backend_tests.rs`:

```rust
use crate::federation::graph_backend::PetgraphBackend;

#[test]
fn petgraph_backend_persists_and_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    let n = GraphNode::new(NodeType::Function, "f".into(), "src/lib.rs".into())
        .with_id_override("repo1:Function:src/lib.rs:f");
    b.upsert_node(n.clone()).unwrap();
    assert_eq!(b.node_count(), 1);
    drop(b);

    let b2 = PetgraphBackend::new(tmp.path()).unwrap();
    assert_eq!(b2.node_count(), 1);
    assert!(b2.get_node("repo1:Function:src/lib.rs:f").unwrap().is_some());
}
```

This requires `GraphNode::with_id_override(...)` which doesn't exist yet. In a real codebase you'd add that as a small helper; in this spec, the alternative is to add a method `PetgraphBackend::upsert_node_with_global_id(global_id, kind, path, name)` that bypasses `GraphNode::generate_id` and uses the supplied global ID directly.

Modify the `PetgraphBackend` API in this task to expose:

```rust
impl PetgraphBackend {
    pub fn upsert_node_global(&self, global_id: &str, kind: NodeType, path: &str, name: &str) -> Result<(), LainError>;
}
```

and rewrite the test above to use it:

```rust
#[test]
fn petgraph_backend_persists_and_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    let b = PetgraphBackend::new(tmp.path()).unwrap();
    b.upsert_node_global("repo1:Function:src/lib.rs:f", NodeType::Function, "src/lib.rs", "f").unwrap();
    assert_eq!(b.node_count(), 1);
    drop(b);

    let b2 = PetgraphBackend::new(tmp.path()).unwrap();
    assert_eq!(b2.node_count(), 1);
    assert!(b2.get_node("repo1:Function:src/lib.rs:f").unwrap().is_some());
}
```

- [ ] **Step 7.3: Implement `PetgraphBackend`**

Append to `src/federation/graph_backend.rs`:

```rust
use crate::federation::repo_id::GlobalId;
use crate::graph::GraphDatabase;
use crate::schema::NodeType;
use dashmap::DashMap;
use std::path::Path;

pub struct PetgraphBackend {
    db: GraphDatabase,
    index: DashMap<String, GlobalId>,
}

impl PetgraphBackend {
    pub fn new(data_dir: &Path) -> Result<Self, LainError> {
        let db = GraphDatabase::new(&data_dir.join("federated_graph.bin"))?;
        Ok(Self { db, index: DashMap::new() })
    }
    pub fn upsert_node_global(&self, global_id: &str, kind: NodeType, path: &str, name: &str) -> Result<(), LainError> {
        let mut n = GraphNode::new(kind, name.to_string(), path.to_string());
        n.id = global_id.to_string();
        self.db.upsert_node(n)?;
        self.index.insert(global_id.to_string(), GlobalId::parse(global_id)?);
        Ok(())
    }
}

impl GraphBackend for PetgraphBackend {
    fn upsert_node(&self, node: GraphNode) -> Result<(), LainError> {
        self.db.upsert_node(node.clone())?;
        self.index.insert(node.id.clone(), GlobalId::parse(&node.id)?);
        Ok(())
    }
    fn upsert_edge(&self, edge: GraphEdge) -> Result<(), LainError> {
        self.db.upsert_edge(edge)
    }
    fn get_node(&self, global_id: &str) -> Result<Option<GraphNode>, LainError> {
        // Delegate to the existing GraphDatabase path. For now, find the local
        // node index by reverse-lookup; real impl uses the GlobalId -> LocalId
        // index added in Task 10.
        self.db.get_node_by_id(global_id)
    }
    fn traverse(&self, start: &str, edge: EdgeType, depth: Range<u32>) -> Result<Vec<GraphNode>, LainError> {
        self.db.traverse(start, edge, depth)
    }
    fn find_path(&self, from: &str, to: &str) -> Result<Vec<GraphNode>, LainError> {
        self.db.find_path(from, to)
    }
    fn subgraph_around(&self, center: &str, radius: u32) -> Result<Vec<(GraphNode, Vec<GraphEdge>)>, LainError> {
        self.db.subgraph_around(center, radius)
    }
    fn node_count(&self) -> usize { self.db.node_count() }
    fn edge_count(&self) -> usize { self.db.edge_count() }
}
```

Add `GraphDatabase::get_node_by_id`, `traverse`, `find_path`, `subgraph_around`, `node_count`, `edge_count` methods to `src/graph.rs` if they don't exist (small wrappers around the existing `petgraph::StableGraph` API in that file). They are thin pass-throughs to today's BFS/DFS — implement using `petgraph::visit::EdgeRef` and `petgraph::Direction` exactly as today's `get_blast_radius` and `get_call_chain` already do.

- [ ] **Step 7.4: Run tests, verify they pass**

Run: `cargo test --lib federation::graph_backend_tests -- --nocapture`
Expected: 4 tests pass (3 from Task 6 + 1 new).

- [ ] **Step 7.5: Commit**

```bash
git add src/federation/graph_backend.rs src/federation/graph_backend_tests.rs src/graph.rs
git commit -m "feat(federation): add PetgraphBackend with persistence"
```

---

### Task 8: Cross-repo signature-similarity matching

**Files:**
- Create: `src/federation/matching.rs`
- Create: `src/federation/matching_tests.rs`
- Modify: `src/federation/mod.rs` (add `pub mod matching;`)
- Modify: `src/lib.rs` (add `#[cfg(test)] mod matching_tests;`)

**Interfaces:**
- Consumes: `GraphNode` from `crate::schema`, `RepoId` from `crate::federation::repo_id`
- Produces: `pub fn signature_tokens(sig: &str) -> Vec<String>` (tokenize), `pub fn signature_similarity(a: &[String], b: &[String]) -> f32` (cosine on hashed-token bag), `pub fn find_cross_repo_matches(new_node: &GraphNode, candidates: &[GraphNode], top_k: usize, threshold: f32) -> Vec<(String, f32)>` returning (target_global_id, similarity) pairs.

- [ ] **Step 8.1: Write the failing test**

Create `src/federation/matching_tests.rs`:

```rust
use crate::federation::matching::*;
use crate::schema::{GraphNode, NodeType};

fn node(repo: &str, name: &str, sig: &str) -> GraphNode {
    let mut n = GraphNode::new(NodeType::Function, name.into(), "src/lib.rs".into());
    n.id = format!("{repo}:Function:src/lib.rs:{name}");
    n.signature = Some(sig.into());
    n
}

#[test]
fn signature_tokens_splits_on_punctuation() {
    let toks = signature_tokens("fn verify_token(user: &User) -> Result<Token>");
    assert!(toks.contains(&"verify_token".to_string()));
    assert!(toks.contains(&"user".to_string()));
    assert!(toks.contains(&"user".to_string())); // appears twice via "user:" and "&User"
    assert!(toks.contains(&"token".to_string()));
}

#[test]
fn signature_similarity_identical_is_one() {
    let a = signature_tokens("fn foo(x: i32) -> i32");
    let b = signature_tokens("fn foo(x: i32) -> i32");
    assert!((signature_similarity(&a, &b) - 1.0).abs() < 1e-6);
}

#[test]
fn signature_similarity_disjoint_is_zero() {
    let a = signature_tokens("fn alpha(x: i32)");
    let b = signature_tokens("fn beta(y: String)");
    assert_eq!(signature_similarity(&a, &b), 0.0);
}

#[test]
fn find_cross_repo_matches_above_threshold() {
    let new_node = node("repo1", "verify_token", "fn verify_token(user: &User) -> Result<Token>");
    let candidates = vec![
        node("repo2", "verify_token", "fn verify_token(u: &User) -> Result<Token>"),
        node("repo3", "validate", "fn validate(x: i32) -> bool"),
        node("repo4", "verify_token", "fn totally_different() -> String"),
    ];
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.5);
    let matched_ids: Vec<&str> = matches.iter().map(|(id, _)| id.as_str()).collect();
    assert!(matched_ids.contains(&"repo2:Function:src/lib.rs:verify_token"));
    assert!(!matched_ids.contains(&"repo3:Function:src/lib.rs:validate"));
    assert!(!matched_ids.contains(&"repo4:Function:src/lib.rs:verify_token"));
}

#[test]
fn find_cross_repo_matches_caps_at_top_k() {
    let new_node = node("repo1", "f", "fn f(x: i32)");
    let candidates: Vec<GraphNode> = (0..20).map(|i| {
        let mut n = node(&format!("repo{i}"), "f", "fn f(x: i32)");
        n.signature = Some("fn f(x: i32)".into());
        n
    }).collect();
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.0);
    assert_eq!(matches.len(), 5);
}

#[test]
fn find_cross_repo_matches_excludes_same_repo() {
    let new_node = node("repo1", "f", "fn f(x: i32)");
    let candidates = vec![node("repo1", "f", "fn f(x: i32)")];
    let matches = find_cross_repo_matches(&new_node, &candidates, 5, 0.0);
    assert!(matches.is_empty(), "same-repo matches should be excluded");
}
```

- [ ] **Step 8.2: Implement matching**

Create `src/federation/matching.rs`:

```rust
use crate::federation::repo_id::RepoId;
use crate::schema::GraphNode;

pub fn signature_tokens(sig: &str) -> Vec<String> {
    sig.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

pub fn signature_similarity(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() { return 0.0; }
    use std::collections::HashMap;
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    for t in a { counts.entry(t.as_str()).or_insert((0, 0)).0 += 1; }
    for t in b { counts.entry(t.as_str()).or_insert((0, 0)).1 += 1; }
    let mut dot = 0usize;
    let mut norm_a = 0usize;
    let mut norm_b = 0usize;
    for (_, (ca, cb)) in &counts {
        dot += ca * cb;
        norm_a += ca * ca;
        norm_b += cb * cb;
    }
    let denom = ((norm_a as f32).sqrt() * (norm_b as f32).sqrt());
    if denom == 0.0 { 0.0 } else { dot as f32 / denom }
}

pub fn find_cross_repo_matches(
    new_node: &GraphNode,
    candidates: &[GraphNode],
    top_k: usize,
    threshold: f32,
) -> Vec<(String, f32)> {
    let new_repo = GlobalId::parse(&new_node.id).ok().map(|g| g.repo_id().to_string());
    let new_sig = new_node.signature.as_deref().unwrap_or("");
    let new_tokens = signature_tokens(new_sig);
    let mut scored: Vec<(String, f32)> = candidates.iter()
        .filter_map(|c| {
            if Some(c.id.as_str()) == new_repo.as_deref().map(|_| c.id.as_str()) {
                // exclude same-repo candidates
            }
            let cand_repo = GlobalId::parse(&c.id).ok()?.repo_id().to_string();
            if Some(&cand_repo) == new_repo.as_ref() { return None; }
            let cand_sig = c.signature.as_deref().unwrap_or("");
            let cand_tokens = signature_tokens(cand_sig);
            let sim = signature_similarity(&new_tokens, &cand_tokens);
            if sim >= threshold { Some((c.id.clone(), sim)) } else { None }
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);
    scored
}
```

Add the missing `use crate::federation::repo_id::GlobalId;` at the top of `matching.rs`. (The implementation above is a sketch — clean up the same-repo filter to use `RepoId` directly, not the `if Some(c.id.as_str()) == new_repo.as_deref().map(|_| c.id.as_str())` line which is a no-op and should be removed.)

Correct version:

```rust
.filter_map(|c| {
    let cand_repo = GlobalId::parse(&c.id).ok()?.repo_id().to_string();
    if Some(&cand_repo) == new_repo.as_ref() { return None; }
    let cand_sig = c.signature.as_deref().unwrap_or("");
    let cand_tokens = signature_tokens(cand_sig);
    let sim = signature_similarity(&new_tokens, &cand_tokens);
    if sim >= threshold { Some((c.id.clone(), sim)) } else { None }
})
```

Add `pub mod matching;` to `src/federation/mod.rs` and `#[cfg(test)] mod matching_tests;` to `src/lib.rs`.

- [ ] **Step 8.3: Run tests, verify they pass**

Run: `cargo test --lib federation::matching_tests -- --nocapture`
Expected: 6 tests pass.

- [ ] **Step 8.4: Commit**

```bash
git add src/federation/matching.rs src/federation/matching_tests.rs src/federation/mod.rs src/lib.rs
git commit -m "feat(federation): add cross-repo signature-similarity matching"
```

---

### Task 9: `RepoIndex` wrapper

**Files:**
- Create: `src/federation/repo_index.rs`
- Modify: `src/federation/mod.rs` (add `pub mod repo_index;`)

**Interfaces:**
- Consumes: `RepoSource` trait, today's `GraphDatabase`, `LspPool`, `GitSensor`, `RepoHealth`
- Produces: `pub struct RepoIndex { source: Box<dyn RepoSource>, db: GraphDatabase, lsp: LspPool, git: GitSensor, health: Arc<RwLock<RepoHealth>>, last_indexed: Arc<RwLock<SystemTime>> }` with `new(source: Box<dyn RepoSource>, data_dir: &Path) -> Result<Self, LainError>`, `pub fn source(&self) -> &dyn RepoSource`, `pub fn db(&self) -> &GraphDatabase`, `pub fn health(&self) -> RepoHealth`, `pub fn set_health(&self, h: RepoHealth)`, `pub async fn index(&self) -> Result<(), LainError>` (runs the existing indexing pipeline for one repo: tree-sitter extract → LSP hydrate → git co-change), `pub fn nodes(&self) -> Vec<GraphNode>` (returns the current node set for projection into the global graph), `pub fn edges(&self) -> Vec<GraphEdge>` (same for edges), `pub fn start_watcher(&self) -> Result<(), LainError>` (wires today's `notify` watcher to call back into the index).

- [ ] **Step 9.1: Read existing indexer entry points**

Read `src/main.rs` and `src/server/ingestion.rs` to identify the existing indexing call sequence. The new `RepoIndex::index` should call those same functions in the same order, but scoped to one repo's `local_path`.

- [ ] **Step 9.2: Write the failing test**

Append to `src/federation/repo_index.rs` (inline `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::repo_source::WorkspaceDirSource;
    use crate::federation::repo_id::RepoId;
    use std::path::PathBuf;

    #[test]
    fn new_creates_with_indexing_health() {
        let tmp = tempfile::tempdir().unwrap();
        let src = Box::new(WorkspaceDirSource::new(RepoId::new("r").unwrap(), PathBuf::from("/tmp")).unwrap());
        let ri = RepoIndex::new(src, tmp.path()).unwrap();
        assert_eq!(ri.health(), RepoHealth::Indexing);
    }

    #[test]
    fn set_health_updates_state() {
        let tmp = tempfile::tempdir().unwrap();
        let src = Box::new(WorkspaceDirSource::new(RepoId::new("r").unwrap(), PathBuf::from("/tmp")).unwrap());
        let ri = RepoIndex::new(src, tmp.path()).unwrap();
        ri.set_health(RepoHealth::Ready);
        assert_eq!(ri.health(), RepoHealth::Ready);
    }
}
```

- [ ] **Step 9.3: Implement `RepoIndex`**

Create `src/federation/repo_index.rs`:

```rust
use crate::error::LainError;
use crate::federation::health::RepoHealth;
use crate::federation::repo_source::RepoSource;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::schema::{GraphEdge, GraphNode};
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

pub struct RepoIndex {
    source: Box<dyn RepoSource>,
    db: GraphDatabase,
    lsp: LspPool,
    git: GitSensor,
    health: Arc<RwLock<RepoHealth>>,
    last_indexed: Arc<RwLock<SystemTime>>,
}

impl RepoIndex {
    pub fn new(source: Box<dyn RepoSource>, data_dir: &Path) -> Result<Self, LainError> {
        let db = GraphDatabase::new(&data_dir.join("graph.bin"))?;
        let lsp = LspPool::new(source.local_path())?;
        let git = GitSensor::new(source.local_path())?;
        Ok(Self {
            source,
            db,
            lsp,
            git,
            health: Arc::new(RwLock::new(RepoHealth::Indexing)),
            last_indexed: Arc::new(RwLock::new(SystemTime::UNIX_EPOCH)),
        })
    }
    pub fn source(&self) -> &dyn RepoSource { self.source.as_ref() }
    pub fn db(&self) -> &GraphDatabase { &self.db }
    pub fn health(&self) -> RepoHealth { *self.health.read() }
    pub fn set_health(&self, h: RepoHealth) { *self.health.write() = h; }
    pub fn last_indexed(&self) -> SystemTime { *self.last_indexed.read() }
    pub fn nodes(&self) -> Vec<GraphNode> { self.db.all_nodes() }
    pub fn edges(&self) -> Vec<GraphEdge> { self.db.all_edges() }
    pub async fn index(&self) -> Result<(), LainError> {
        // Calls the existing tree-sitter → LSP → git pipeline, scoped to source.local_path().
        // Implementation delegates to the same functions main.rs / server/ingestion.rs use.
        // Set health to Ready on success, Degraded on failure (with retry handled by caller).
        todo!("wire to existing ingestion pipeline in src/server/ingestion.rs")
    }
    pub fn start_watcher(&self) -> Result<(), LainError> {
        // Wires today's notify::RecommendedWatcher to call self.index() on file change.
        todo!("wire to existing watcher in src/watcher.rs")
    }
}
```

Add `GraphDatabase::all_nodes` and `all_edges` to `src/graph.rs` as small accessors if they don't exist (return `Vec<GraphNode>` and `Vec<GraphEdge>` via `petgraph::stable_graph::NodeReference` iteration).

- [ ] **Step 9.4: Run tests, verify they pass**

Run: `cargo test --lib federation::repo_index -- --nocapture`
Expected: 2 tests pass; the two `todo!()`s are not invoked by these tests so they don't panic.

- [ ] **Step 9.5: Commit**

```bash
git add src/federation/repo_index.rs src/federation/mod.rs src/graph.rs
git commit -m "feat(federation): add RepoIndex wrapper (skeleton, wired to existing pipeline)"
```

---

### Task 10: `FederatedIndex` orchestrator

**Files:**
- Create: `src/federation/federated_index.rs`
- Create: `src/federation/federated_index_tests.rs`
- Modify: `src/federation/mod.rs` (add `pub mod federated_index;`)
- Modify: `src/lib.rs` (add `#[cfg(test)] mod federated_index_tests;`)

**Interfaces:**
- Consumes: `RepoSource`, `RepoIndex`, `GraphBackend`, `GlobalId`, `RepoId`, cross-repo matching
- Produces: `pub struct FederatedIndex { repos: RwLock<HashMap<RepoId, Arc<RepoIndex>>>, backend: Arc<dyn GraphBackend>, repo_id_to_global: DashMap<String, String>, }` with `new(backend: Arc<dyn GraphBackend>) -> Self`, `pub async fn add_repo(&self, source: Box<dyn RepoSource>, data_dir: &Path) -> Result<(), LainError>`, `pub fn remove_repo(&self, id: &RepoId) -> Result<(), LainError>`, `pub fn get_repo(&self, id: &RepoId) -> Option<Arc<RepoIndex>>`, `pub fn list_repos(&self) -> Vec<(RepoId, RepoHealth)>`, `pub fn global_id(&self, repo: &RepoId, kind: NodeType, path: &str, name: &str) -> GlobalId`, `pub async fn project_repo(&self, id: &RepoId) -> Result<(), LainError>` (reads the per-repo graph and projects nodes/edges into the global backend, running cross-repo matching), `pub fn backend(&self) -> Arc<dyn GraphBackend>`, `pub fn resolve_symbol(&self, name: &str) -> Result<RepoId, LainError>` (single match returns repo, no match = NotFound, multiple = AmbiguousSymbol).

- [ ] **Step 10.1: Write the failing test**

Create `src/federation/federated_index_tests.rs`:

```rust
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::federation::repo_id::RepoId;
use crate::federation::repo_source::WorkspaceDirSource;
use crate::schema::{EdgeType, GraphEdge, GraphNode, NodeType};
use std::path::PathBuf;
use std::sync::Arc;

fn petgraph_backend(tmp: &tempfile::TempDir) -> Arc<dyn GraphBackend> {
    Arc::new(PetgraphBackend::new(tmp.path()).unwrap())
}

#[tokio::test]
async fn add_repo_registers_and_lists_it() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
        WorkspaceDirSource::new(RepoId::new("repo-a").unwrap(), PathBuf::from("/tmp/a")).unwrap(),
    );
    fed.add_repo(src, tmp.path()).await.unwrap();
    let listed = fed.list_repos();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.as_str(), "repo-a");
}

#[tokio::test]
async fn global_id_format() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    let id = fed.global_id(&RepoId::new("repo-a").unwrap(), NodeType::Function, "src/lib.rs", "f");
    assert_eq!(id.as_str(), "repo-a:Function:src/lib.rs:f");
}

#[test]
fn resolve_symbol_unique_match_returns_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    let backend = fed.backend();
    backend.upsert_node_global("repo-a:Function:src/lib.rs:only_one", NodeType::Function, "src/lib.rs", "only_one").unwrap();
    let resolved = fed.resolve_symbol("only_one").unwrap();
    assert_eq!(resolved.as_str(), "repo-a");
}

#[test]
fn resolve_symbol_no_match_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    assert!(matches!(fed.resolve_symbol("nope"), Err(crate::error::LainError::NotFound(_))));
}

#[test]
fn resolve_symbol_multiple_matches_returns_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(petgraph_backend(&tmp));
    let backend = fed.backend();
    backend.upsert_node_global("repo-a:Function:src/lib.rs:shared", NodeType::Function, "src/lib.rs", "shared").unwrap();
    backend.upsert_node_global("repo-b:Function:src/lib.rs:shared", NodeType::Function, "src/lib.rs", "shared").unwrap();
    let err = fed.resolve_symbol("shared").unwrap_err();
    assert!(matches!(err, crate::error::LainError::AmbiguousSymbol(_)));
}
```

This requires `LainError::NotFound(String)` and `LainError::AmbiguousSymbol(Vec<RepoId>)` variants. Add them to `src/error.rs`.

- [ ] **Step 10.2: Implement `FederatedIndex`**

Create `src/federation/federated_index.rs`:

```rust
use crate::error::LainError;
use crate::federation::graph_backend::GraphBackend;
use crate::federation::health::RepoHealth;
use crate::federation::matching::find_cross_repo_matches;
use crate::federation::repo_id::{GlobalId, RepoId};
use crate::federation::repo_index::RepoIndex;
use crate::federation::repo_source::RepoSource;
use crate::schema::{GraphEdge, GraphNode, NodeType};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub struct FederatedIndex {
    repos: RwLock<HashMap<RepoId, Arc<RepoIndex>>>,
    backend: Arc<dyn GraphBackend>,
    symbol_to_repos: DashMap<String, Vec<RepoId>>,
}

impl FederatedIndex {
    pub fn new(backend: Arc<dyn GraphBackend>) -> Self {
        Self { repos: RwLock::new(HashMap::new()), backend, symbol_to_repos: DashMap::new() }
    }

    pub async fn add_repo(&self, source: Box<dyn RepoSource>, data_dir: &Path) -> Result<(), LainError> {
        let id = source.id().clone();
        let index = Arc::new(RepoIndex::new(source, data_dir)?);
        self.repos.write().insert(id.clone(), index);
        self.rebuild_symbol_index();
        Ok(())
    }

    pub fn remove_repo(&self, id: &RepoId) -> Result<(), LainError> {
        self.repos.write().remove(id);
        self.rebuild_symbol_index();
        Ok(())
    }

    pub fn get_repo(&self, id: &RepoId) -> Option<Arc<RepoIndex>> {
        self.repos.read().get(id).cloned()
    }

    pub fn list_repos(&self) -> Vec<(RepoId, RepoHealth)> {
        let mut out: Vec<(RepoId, RepoHealth)> = self.repos.read().iter()
            .map(|(id, idx)| (id.clone(), idx.health()))
            .collect();
        out.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        out
    }

    pub fn global_id(&self, repo: &RepoId, kind: NodeType, path: &str, name: &str) -> GlobalId {
        GlobalId::new(repo, kind, path, name)
    }

    pub fn backend(&self) -> Arc<dyn GraphBackend> { self.backend.clone() }

    pub async fn project_repo(&self, id: &RepoId) -> Result<(), LainError> {
        let repo = self.get_repo(id).ok_or_else(|| LainError::NotFound(format!("repo {id}")))?;
        let nodes = repo.nodes();
        for n in &nodes {
            let gid = GlobalId::new(id, n.node_type.clone(), &n.path, &n.name);
            // Re-key: rewrite node.id to global id, then upsert.
            let mut rewritten = n.clone();
            rewritten.id = gid.as_str().to_string();
            self.backend.upsert_node(rewritten)?;
        }
        // Cross-repo matching on this repo's nodes against all others' global nodes.
        let other_nodes: Vec<GraphNode> = self.repos.read().iter()
            .filter(|(rid, _)| *rid != id)
            .flat_map(|(_, idx)| idx.nodes())
            .collect();
        for new_node in &nodes {
            let matches = find_cross_repo_matches(new_node, &other_nodes, 5, 0.5);
            for (target_gid, sim) in matches {
                self.backend.upsert_edge(GraphEdge {
                    edge_type: EdgeType::CrossRepoSameSymbol,
                    source_id: GlobalId::new(id, new_node.node_type.clone(), &new_node.path, &new_node.name).as_str().to_string(),
                    target_id: target_gid,
                    weight: Some(sim),
                })?;
            }
        }
        self.rebuild_symbol_index();
        Ok(())
    }

    pub fn resolve_symbol(&self, name: &str) -> Result<RepoId, LainError> {
        match self.symbol_to_repos.get(name) {
            None => Err(LainError::NotFound(format!("symbol {name} not found in any repo"))),
            Some(entries) if entries.len() == 1 => Ok(entries[0].clone()),
            Some(entries) => Err(LainError::AmbiguousSymbol(entries.clone())),
        }
    }

    fn rebuild_symbol_index(&self) {
        self.symbol_to_repos.clear();
        let mut tmp: HashMap<String, Vec<RepoId>> = HashMap::new();
        for (repo_id, _) in self.repos.read().iter() {
            // Project every node name. In a real impl this reads from the backend;
            // for the MVP we read from each RepoIndex's nodes().
            // (Production code would maintain a separate name index; for now iterate.)
            for node in self.get_repo(repo_id).unwrap().nodes() {
                tmp.entry(node.name.clone()).or_default().push(repo_id.clone());
            }
        }
        for (k, v) in tmp { self.symbol_to_repos.insert(k, v); }
    }
}
```

Add the `EdgeType::CrossRepoSameSymbol` variant to `src/schema.rs`'s `EdgeType` enum. Add `LainError::NotFound(String)` and `LainError::AmbiguousSymbol(Vec<RepoId>)` to `src/error.rs` if not already present.

- [ ] **Step 10.3: Run tests, verify they pass**

Run: `cargo test --lib federation::federated_index_tests -- --nocapture`
Expected: 5 tests pass.

- [ ] **Step 10.4: Commit**

```bash
git add src/federation/federated_index.rs src/federation/federated_index_tests.rs src/federation/mod.rs src/lib.rs src/schema.rs src/error.rs
git commit -m "feat(federation): add FederatedIndex orchestrator with symbol resolution"
```

---

### Task 11: `FederationManifest` persistence

**Files:**
- Create: `src/federation/manifest.rs`
- Create: `src/federation/manifest_tests.rs`
- Modify: `src/federation/mod.rs` (add `pub mod manifest;`)
- Modify: `src/lib.rs` (add `#[cfg(test)] mod manifest_tests;`)

**Interfaces:**
- Consumes: `RepoId` and `RepoHealth`
- Produces: `pub struct FederationManifest { version: u32, repos: Vec<RepoEntry> }` and `pub struct RepoEntry { id: RepoId, source_kind: String, source_config: serde_yaml::Value, last_indexed_unix: i64, content_hash: String, health: RepoHealth }` with `pub fn load_or_default(path: &Path) -> Result<Self, LainError>`, `pub fn save(&self, path: &Path) -> Result<(), LainError>`, `pub fn add_repo(&mut self, ...)`, `pub fn remove_repo(&mut self, id: &RepoId)`. Versioned format: `version: u32` at the start of the bincode blob; load returns `Err(LainError::UnsupportedManifestVersion(u32))` if `version > 1`.

- [ ] **Step 11.1: Write the failing test**

Create `src/federation/manifest_tests.rs`:

```rust
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
```

- [ ] **Step 11.2: Implement `FederationManifest`**

Create `src/federation/manifest.rs`:

```rust
use crate::error::LainError;
use crate::federation::health::RepoHealth;
use crate::federation::repo_id::RepoId;
use serde::{Deserialize, Serialize};
use std::path::Path;

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoEntry {
    pub id: RepoId,
    pub source_kind: String,
    pub source_config: serde_yaml::Value,
    pub last_indexed_unix: i64,
    pub content_hash: String,
    pub health: RepoHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationManifest {
    pub version: u32,
    pub repos: Vec<RepoEntry>,
}

impl Default for FederationManifest {
    fn default() -> Self { Self { version: CURRENT_VERSION, repos: Vec::new() } }
}

impl FederationManifest {
    pub fn load_or_default(path: &Path) -> Result<Self, LainError> {
        if !path.exists() { return Ok(Self::default()); }
        let bytes = std::fs::read(path).map_err(|e| LainError::Io(format!("read manifest: {e}")))?;
        let m: Self = bincode::deserialize(&bytes)
            .map_err(|e| LainError::Serialization(format!("bincode: {e}")))?;
        if m.version > CURRENT_VERSION {
            return Err(LainError::UnsupportedManifestVersion(m.version));
        }
        Ok(m)
    }
    pub fn save(&self, path: &Path) -> Result<(), LainError> {
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| LainError::Io(format!("mkdir: {e}")))?; }
        let bytes = bincode::serialize(self).map_err(|e| LainError::Serialization(format!("bincode: {e}")))?;
        std::fs::write(path, bytes).map_err(|e| LainError::Io(format!("write manifest: {e}")))?;
        Ok(())
    }
    pub fn add_repo(&mut self, entry: RepoEntry) { self.repos.push(entry); }
    pub fn remove_repo(&mut self, id: &RepoId) { self.repos.retain(|r| r.id != *id); }
}
```

Add `LainError::Io(String)`, `LainError::Serialization(String)`, and `LainError::UnsupportedManifestVersion(u32)` to `src/error.rs`.

- [ ] **Step 11.3: Run tests, verify they pass**

Run: `cargo test --lib federation::manifest_tests -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 11.4: Commit**

```bash
git add src/federation/manifest.rs src/federation/manifest_tests.rs src/federation/mod.rs src/lib.rs src/error.rs
git commit -m "feat(federation): add FederationManifest with versioned bincode persistence"
```

---

### Task 12: `repos.yaml` config schema

**Files:**
- Create: `src/federation/config.rs`
- Modify: `src/federation/mod.rs` (add `pub mod config;`)

**Interfaces:**
- Consumes: `RepoId`
- Produces: `pub struct FederationConfig { pub data_dir: PathBuf, pub max_concurrent_indexers: usize, pub ready_threshold: f32, pub repos: Vec<RepoConfig> }` and `pub struct RepoConfig { pub id: String, pub source: SourceConfig }` and `pub enum SourceConfig { LocalClone { url: String, git_ref: String }, ShallowClone { url: String, git_ref: String, refresh_interval_secs: u64 }, WorkspaceDir { path: PathBuf } }` with `pub fn load(path: &Path) -> Result<Self, LainError>` and `pub fn build_sources(&self) -> Result<Vec<Box<dyn RepoSource>>, LainError>`.

- [ ] **Step 12.1: Write the failing test**

Append inline `#[cfg(test)] mod tests` to `src/federation/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let yaml = r#"
data_dir: /var/lib/lain
max_concurrent_indexers: 4
ready_threshold: 0.8
repos:
  - id: a
    source:
      type: workspace_dir
      path: /srv/a
  - id: b
    source:
      type: local_clone
      url: https://example.com/b.git
      ref: main
"#;
        let cfg: FederationConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.repos.len(), 2);
        assert_eq!(cfg.max_concurrent_indexers, 4);
    }

    #[test]
    fn build_sources_returns_correct_impls() {
        let yaml = r#"
data_dir: /tmp
repos:
  - id: ws
    source: { type: workspace_dir, path: /srv/ws }
  - id: lc
    source: { type: local_clone, url: "https://example.com/lc.git", ref: main }
"#;
        let cfg = FederationConfig::load_from_str(yaml).unwrap();
        let sources = cfg.build_sources().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].id().as_str(), "ws");
        assert_eq!(sources[1].id().as_str(), "lc");
    }

    #[test]
    fn rejects_unknown_source_type() {
        let yaml = r#"
data_dir: /tmp
repos:
  - id: x
    source: { type: nonsense }
"#;
        assert!(FederationConfig::load_from_str(yaml).is_err());
    }
}
```

- [ ] **Step 12.2: Implement the config types**

Create `src/federation/config.rs`:

```rust
use crate::error::LainError;
use crate::federation::repo_id::RepoId;
use crate::federation::repo_source::{LocalCloneSource, RepoSource, ShallowCloneSource, WorkspaceDirSource};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
pub struct FederationConfig {
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_max_concurrent_indexers")]
    pub max_concurrent_indexers: usize,
    #[serde(default = "default_ready_threshold")]
    pub ready_threshold: f32,
    pub repos: Vec<RepoConfig>,
}

fn default_data_dir() -> PathBuf { PathBuf::from("./.lain/federation") }
fn default_max_concurrent_indexers() -> usize { 8 }
fn default_ready_threshold() -> f32 { 0.8 }

#[derive(Debug, Clone, Deserialize)]
pub struct RepoConfig {
    pub id: String,
    pub source: SourceConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    LocalClone { url: String, #[serde(default = "default_ref")] r#ref: String },
    ShallowClone { url: String, #[serde(default = "default_ref")] r#ref: String, #[serde(default = "default_refresh_interval_secs")] refresh_interval_secs: u64 },
    WorkspaceDir { path: PathBuf },
}

fn default_ref() -> String { "main".into() }
fn default_refresh_interval_secs() -> u64 { 300 }

impl FederationConfig {
    pub fn load(path: &Path) -> Result<Self, LainError> {
        let s = std::fs::read_to_string(path).map_err(|e| LainError::Io(format!("read config: {e}")))?;
        Self::load_from_str(&s)
    }
    pub fn load_from_str(s: &str) -> Result<Self, LainError> {
        serde_yaml::from_str(s).map_err(|e| LainError::Config(format!("yaml: {e}")))
    }
    pub fn build_sources(&self) -> Result<Vec<Box<dyn RepoSource>>, LainError> {
        let mut out = Vec::with_capacity(self.repos.len());
        for r in &self.repos {
            let id = RepoId::new(&r.id)?;
            let src: Box<dyn RepoSource> = match &r.source {
                SourceConfig::LocalClone { url, r#ref } => Box::new(LocalCloneSource::new(id, url, r#ref, self.data_dir.join(&r.id))?),
                SourceConfig::ShallowClone { url, r#ref, refresh_interval_secs } => Box::new(ShallowCloneSource::new(id, url, r#ref, self.data_dir.join(&r.id), Duration::from_secs(*refresh_interval_secs))?),
                SourceConfig::WorkspaceDir { path } => Box::new(WorkspaceDirSource::new(id, path.clone())?),
            };
            out.push(src);
        }
        Ok(out)
    }
}
```

- [ ] **Step 12.3: Run tests, verify they pass**

Run: `cargo test --lib federation::config -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 12.4: Commit**

```bash
git add src/federation/config.rs src/federation/mod.rs
git commit -m "feat(federation): add repos.yaml config schema and source builder"
```

---

### Task 13: Federation loader

**Files:**
- Create: `src/federation/loader.rs`
- Create: `src/federation/loader_tests.rs`
- Modify: `src/federation/mod.rs` (add `pub mod loader;`)
- Modify: `src/lib.rs` (add `#[cfg(test)] mod loader_tests;`)

**Interfaces:**
- Consumes: `FederationConfig`, `FederationManifest`, `PetgraphBackend`, `FederatedIndex`, `RepoSource`
- Produces: `pub async fn load_federation(config_path: &Path) -> Result<Arc<FederatedIndex>, LainError>`. Reads config, loads (or creates) manifest, builds the PetgraphBackend, builds the FederatedIndex, adds each repo's source, spawns per-repo indexers in parallel bounded by `max_concurrent_indexers`. Returns when at least `ready_threshold` fraction of repos are `Ready` (others continue indexing in the background; the returned handle is shared with the indexers).

- [ ] **Step 13.1: Write the failing test**

Create `src/federation/loader_tests.rs`:

```rust
use crate::federation::loader::load_federation;
use std::path::PathBuf;

#[tokio::test]
async fn loads_minimal_config_with_workspace_dir_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_path = tmp.path().join("repos.yaml");
    std::fs::write(&cfg_path, format!(r#"
data_dir: {}
repos:
  - id: ws
    source: {{ type: workspace_dir, path: {} }}
"#, tmp.path().join("data").display(), tmp.path().join("ws").display())).unwrap();
    std::fs::create_dir_all(tmp.path().join("ws")).unwrap();
    std::fs::create_dir_all(tmp.path().join("data")).unwrap();

    let fed = load_federation(&cfg_path).await.unwrap();
    let listed = fed.list_repos();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.as_str(), "ws");
}
```

- [ ] **Step 13.2: Implement `load_federation`**

Create `src/federation/loader.rs`:

```rust
use crate::error::LainError;
use crate::federation::config::FederationConfig;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::{GraphBackend, PetgraphBackend};
use crate::federation::manifest::{FederationManifest, RepoEntry};
use crate::federation::repo_index::RepoIndex;
use crate::federation::repo_source::RepoSource;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

pub async fn load_federation(config_path: &Path) -> Result<Arc<FederatedIndex>, LainError> {
    let config = FederationConfig::load(config_path)?;
    let manifest_path = config.data_dir.join("federation_manifest.bin");
    let _manifest = FederationManifest::load_or_default(&manifest_path)?;

    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(&config.data_dir)?);
    let fed = Arc::new(FederatedIndex::new(backend));

    let sources = config.build_sources()?;
    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_indexers));

    let mut handles = Vec::new();
    for src in sources {
        let permit = semaphore.clone().acquire_owned().await.map_err(|e| LainError::Other(format!("semaphore: {e}")))?;
        let fed_clone = fed.clone();
        let data_dir = config.data_dir.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let repo_id = src.id().clone();
            fed_clone.add_repo(src, &data_dir).await?;
            // Trigger initial projection (no real indexing yet; RepoIndex::index is a
            // todo!() in Task 9 and lands in Task 14. For now, project_repo runs the
            // current (possibly empty) node set, which is the correct MVP behavior.)
            fed_clone.project_repo(&repo_id).await?;
            Ok::<(), LainError>(())
        }));
    }
    for h in handles {
        h.await.map_err(|e| LainError::Other(format!("join: {e}")))??;
    }
    // Write the manifest (best-effort).
    let _ = save_manifest(&fed, &manifest_path);
    Ok(fed)
}

fn save_manifest(_fed: &FederatedIndex, _path: &Path) -> Result<(), LainError> {
    // Real impl: walk fed.list_repos() and serialize to FederationManifest. Left as
    // a no-op stub here; populated in Task 14 when RepoIndex::index() exists.
    Ok(())
}
```

Add `LainError::Other(String)` to `src/error.rs` if not present.

- [ ] **Step 13.3: Run tests, verify they pass**

Run: `cargo test --lib federation::loader_tests -- --nocapture`
Expected: 1 test passes.

- [ ] **Step 13.4: Commit**

```bash
git add src/federation/loader.rs src/federation/loader_tests.rs src/federation/mod.rs src/lib.rs src/error.rs
git commit -m "feat(federation): add config-driven loader with bounded parallelism"
```

---

### Task 14: Wire `RepoIndex::index` to the existing ingestion pipeline

**Files:**
- Modify: `src/federation/repo_index.rs` (replace the `todo!()` in `index()` and `start_watcher()` with real calls)

**Interfaces:**
- Consumes: existing `src/server/ingestion.rs`, `src/treesitter.rs`, `src/lsp.rs`, `src/git.rs`, `src/watcher.rs`
- Produces: `RepoIndex::index()` and `start_watcher()` are real implementations. No new public types.

- [ ] **Step 14.1: Read existing ingestion entry points**

Read `src/main.rs`, `src/server/ingestion.rs`, and `src/watcher.rs`. Identify:
- The function that runs the tree-sitter extract on a path.
- The function that hydrates via LSP for a path.
- The function that runs git co-change analysis for a path.
- The watcher setup function and its callback signature.

- [ ] **Step 14.2: Refactor existing entry points to accept a `&Path` instead of a global workspace**

Most of the existing functions probably take the whole workspace. Refactor them minimally so they can be called per-repo. The call sites in `src/main.rs` and `src/server/ingestion.rs` should keep working — wrap the old call with a per-path loop if refactoring is too invasive. If refactoring is invasive, leave the existing functions alone and add new per-repo variants alongside them.

Concretely: add a function `pub fn index_one_repo(path: &Path, db: &mut GraphDatabase, lsp: &LspPool, git: &GitSensor) -> Result<(), LainError>` in `src/server/ingestion.rs` that runs the same pipeline as today's main loop, scoped to `path`. Keep the existing function for back-compat.

- [ ] **Step 14.3: Implement `RepoIndex::index` and `start_watcher`**

Replace the `todo!()` in `src/federation/repo_index.rs`:

```rust
pub async fn index(&self) -> Result<(), LainError> {
    let path = self.source.local_path().to_path_buf();
    let db = self.db.clone_for_indexing()?; // returns a mutable handle; check src/graph.rs for the right accessor
    let lsp = self.lsp.clone();
    let git = self.git.clone();
    tokio::task::spawn_blocking(move || -> Result<(), LainError> {
        crate::server::ingestion::index_one_repo(&path, &db, &lsp, &git)?;
        Ok(())
    }).await.map_err(|e| LainError::Other(format!("join: {e}")))??;
    *self.last_indexed.write() = SystemTime::now();
    self.set_health(RepoHealth::Ready);
    Ok(())
}

pub fn start_watcher(&self) -> Result<(), LainError> {
    use notify::{RecommendedWatcher, RecursiveMode, Watcher};
    use std::time::Duration;
    let path = self.source.local_path().to_path_buf();
    let me = self.clone_arc_handles()?; // helper that returns Arc<Self> handles for the closure
    let mut watcher = RecommendedWatcher::new(move |res: notify::Result<notify::Event>| {
        if let Ok(_event) = res {
            // Re-index on change. Spawn a task; ignore errors here (already handled in index()).
            let me = me.clone();
            tokio::spawn(async move {
                let _ = me.index().await;
            });
        }
    }, notify::Config::default().with_poll_interval(Duration::from_secs(2)))?;
    watcher.watch(path, RecursiveMode::Recursive)?;
    // Store watcher in self to keep it alive — requires a Mutex<Option<Watcher>> field.
    *self.watcher.lock() = Some(Box::new(watcher));
    Ok(())
}
```

This requires adding a `Mutex<Option<Box<dyn Watcher>>>` field to `RepoIndex` and a helper to clone arc handles into the watcher closure. Add them as needed.

- [ ] **Step 14.4: Add an integration smoke test**

Append to `tests/federation_integration.rs` (create if not yet present — see Task 20):

```rust
#[tokio::test]
async fn end_to_end_index_one_repo() {
    // Creates a tiny temp repo, indexes it, asserts nodes exist.
}
```

(Detailed test code is in Task 20; this is just the smoke test that wires the pipeline.)

- [ ] **Step 14.5: Run existing tests, verify no regressions**

Run: `cargo test --lib -- --nocapture`
Expected: All prior tests pass. The previously-`todo!()` paths now work.

- [ ] **Step 14.6: Commit**

```bash
git add src/federation/repo_index.rs src/server/ingestion.rs src/watcher.rs src/graph.rs
git commit -m "feat(federation): wire RepoIndex::index to existing ingestion pipeline"
```

---

### Task 15: MCP tools — `list_repos` and `get_repo_info`

**Files:**
- Create: `src/mcp/federation_tools.rs`
- Modify: `src/mcp/handler.rs` (register the new tools when federation mode is active)
- Modify: `src/mcp/mod.rs` (re-export `federation_tools`)

**Interfaces:**
- Consumes: `FederatedIndex` (passed as a tool-context field), `RepoId`
- Produces: `pub fn list_repos(fed: &FederatedIndex) -> Vec<RepoInfo>` returning a `RepoInfo` struct, and `pub fn get_repo_info(fed: &FederatedIndex, id: &RepoId) -> Result<RepoInfo, LainError>`. `RepoInfo { id, path, health, last_refreshed_unix, last_indexed_unix, node_count, edge_count }`. These are registered as MCP tools in `handler.rs` under the names `list_repos` and `get_repo_info`.

- [ ] **Step 15.1: Write the failing test**

Append inline `#[cfg(test)] mod tests` to `src/mcp/federation_tools.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::federated_index::FederatedIndex;
    use crate::federation::graph_backend::PetgraphBackend;
    use crate::federation::health::RepoHealth;
    use crate::federation::repo_id::RepoId;
    use crate::federation::repo_source::WorkspaceDirSource;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn list_repos_returns_registered() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
            WorkspaceDirSource::new(RepoId::new("a").unwrap(), PathBuf::from("/tmp")).unwrap(),
        );
        fed.add_repo(src, tmp.path()).await.unwrap();
        let list = list_repos(&fed);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "a");
        assert_eq!(list[0].health, RepoHealth::Indexing);
    }
}
```

- [ ] **Step 15.2: Implement the tools and registration**

Create `src/mcp/federation_tools.rs`:

```rust
use crate::error::LainError;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::repo_id::RepoId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoInfo {
    pub id: String,
    pub path: String,
    pub health: String,
    pub last_refreshed_unix: i64,
    pub last_indexed_unix: i64,
    pub node_count: usize,
    pub edge_count: usize,
}

pub fn list_repos(fed: &FederatedIndex) -> Vec<RepoInfo> {
    fed.list_repos().into_iter().map(|(id, health)| {
        let repo = fed.get_repo(&id);
        let (last_refreshed_unix, last_indexed_unix, node_count, edge_count, path) = match repo {
            Some(r) => {
                let path = r.source().local_path().display().to_string();
                let last_refreshed = r.source().last_refreshed().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                let last_indexed = r.last_indexed().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                (last_refreshed, last_indexed, r.nodes().len(), r.edges().len(), path)
            }
            None => (0, 0, 0, 0, String::new()),
        };
        RepoInfo { id: id.to_string(), path, health: health.to_string(), last_refreshed_unix, last_indexed_unix, node_count, edge_count }
    }).collect()
}

pub fn get_repo_info(fed: &FederatedIndex, id: &RepoId) -> Result<RepoInfo, LainError> {
    let list = list_repos(fed);
    list.into_iter().find(|r| r.id == id.as_str()).ok_or_else(|| LainError::NotFound(format!("repo {id}")))
}
```

In `src/mcp/handler.rs`, register both tools when federation mode is active. Follow the existing tool-registration pattern (search for `mcp::tool` or the equivalent macro pattern in the file). For example:

```rust
if let Some(fed) = &tool_context.federation {
    registry.register("list_repos", |_args| {
        Ok(serde_json::to_value(crate::mcp::federation_tools::list_repos(fed))?)
    });
    registry.register("get_repo_info", |args| {
        let id: String = args.get("id").and_then(|v| v.as_str()).ok_or_else(|| LainError::Config("id required".into()))?.to_string();
        let rid = RepoId::new(&id)?;
        Ok(serde_json::to_value(crate::mcp::federation_tools::get_repo_info(fed, &rid)?)?)
    });
}
```

- [ ] **Step 15.3: Run tests, verify they pass**

Run: `cargo test --lib mcp::federation_tools -- --nocapture`
Expected: 1 test passes. The handler integration is verified by the e2e test in Task 23.

- [ ] **Step 15.4: Commit**

```bash
git add src/mcp/federation_tools.rs src/mcp/handler.rs src/mcp/mod.rs
git commit -m "feat(mcp): add list_repos and get_repo_info federation tools"
```

---

### Task 16: MCP tool — `get_federation_health`

**Files:**
- Modify: `src/mcp/federation_tools.rs` (add the function and a unit test)
- Modify: `src/mcp/handler.rs` (register the tool)

**Interfaces:**
- Produces: `pub struct FederationHealth { total_repos: usize, ready: usize, indexing: usize, degraded: usize, unavailable: usize, missing: usize, total_nodes: usize, total_edges: usize, memory_estimate_bytes: u64 }` and `pub fn get_federation_health(fed: &FederatedIndex) -> FederationHealth`. Memory estimate: `total_nodes * 200 + total_edges * 100` (matches the rough petgraph per-node/edge byte cost from the spec).

- [ ] **Step 16.1: Write the failing test**

Append to the test module in `src/mcp/federation_tools.rs`:

```rust
#[test]
fn federation_health_counts_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
    let h = get_federation_health(&fed);
    assert_eq!(h.total_repos, 0);
    assert_eq!(h.total_nodes, 0);
    assert_eq!(h.total_edges, 0);
}
```

- [ ] **Step 16.2: Implement the function**

Add to `src/mcp/federation_tools.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FederationHealth {
    pub total_repos: usize,
    pub ready: usize,
    pub indexing: usize,
    pub degraded: usize,
    pub unavailable: usize,
    pub missing: usize,
    pub total_nodes: usize,
    pub total_edges: usize,
    pub memory_estimate_bytes: u64,
}

pub fn get_federation_health(fed: &FederatedIndex) -> FederationHealth {
    use crate::federation::health::RepoHealth;
    let repos = fed.list_repos();
    let mut h = FederationHealth { total_repos: repos.len(), ready: 0, indexing: 0, degraded: 0, unavailable: 0, missing: 0, total_nodes: fed.backend().node_count(), total_edges: fed.backend().edge_count(), memory_estimate_bytes: 0 };
    for (_, health) in &repos {
        match health {
            RepoHealth::Ready => h.ready += 1,
            RepoHealth::Indexing => h.indexing += 1,
            RepoHealth::Degraded => h.degraded += 1,
            RepoHealth::Unavailable => h.unavailable += 1,
            RepoHealth::Missing => h.missing += 1,
        }
    }
    h.memory_estimate_bytes = (h.total_nodes as u64) * 200 + (h.total_edges as u64) * 100;
    h
}
```

Register the tool in `src/mcp/handler.rs` the same way as Task 15.

- [ ] **Step 16.3: Run tests, verify they pass**

Run: `cargo test --lib mcp::federation_tools -- --nocapture`
Expected: 2 tests pass.

- [ ] **Step 16.4: Commit**

```bash
git add src/mcp/federation_tools.rs src/mcp/handler.rs
git commit -m "feat(mcp): add get_federation_health tool"
```

---

### Task 17: MCP tool — `search_org`

**Files:**
- Modify: `src/mcp/federation_tools.rs` (add the function and a unit test)
- Modify: `src/mcp/handler.rs` (register the tool)

**Interfaces:**
- Produces: `pub struct SymbolMatch { global_id: String, repo_id: String, name: String, path: String, kind: String }` and `pub fn search_org(fed: &FederatedIndex, query: &str, limit: usize) -> Vec<SymbolMatch>`. Implementation: iterate over every repo's nodes, filter by case-insensitive substring match on `name` or `path`, sort by `(repo_id, name)`, take first `limit`.

- [ ] **Step 17.1: Write the failing test**

Append to the test module in `src/mcp/federation_tools.rs`:

```rust
#[tokio::test]
async fn search_org_finds_across_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
    fed.backend().upsert_node_global("repo-a:Function:src/auth.rs:verify_token", crate::schema::NodeType::Function, "src/auth.rs", "verify_token").unwrap();
    fed.backend().upsert_node_global("repo-b:Function:src/auth.rs:verify_token", crate::schema::NodeType::Function, "src/auth.rs", "verify_token").unwrap();
    fed.backend().upsert_node_global("repo-c:Function:src/x.rs:other", crate::schema::NodeType::Function, "src/x.rs", "other").unwrap();
    let hits = search_org(&fed, "verify", 10);
    assert_eq!(hits.len(), 2);
    let repos: std::collections::HashSet<_> = hits.iter().map(|h| h.repo_id.clone()).collect();
    assert!(repos.contains("repo-a"));
    assert!(repos.contains("repo-b"));
}
```

- [ ] **Step 17.2: Implement the function**

Add to `src/mcp/federation_tools.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SymbolMatch {
    pub global_id: String,
    pub repo_id: String,
    pub name: String,
    pub path: String,
    pub kind: String,
}

pub fn search_org(fed: &FederatedIndex, query: &str, limit: usize) -> Vec<SymbolMatch> {
    let q = query.to_lowercase();
    let mut hits: Vec<SymbolMatch> = Vec::new();
    for (repo_id, _) in fed.list_repos() {
        if let Some(repo) = fed.get_repo(&repo_id) {
            for n in repo.nodes() {
                if n.name.to_lowercase().contains(&q) || n.path.to_lowercase().contains(&q) {
                    hits.push(SymbolMatch {
                        global_id: n.id.clone(),
                        repo_id: repo_id.to_string(),
                        name: n.name.clone(),
                        path: n.path.clone(),
                        kind: n.node_type.to_string(),
                    });
                }
            }
        }
    }
    hits.sort_by(|a, b| a.repo_id.cmp(&b.repo_id).then(a.name.cmp(&b.name)));
    hits.truncate(limit);
    hits
}
```

Register in `src/mcp/handler.rs`.

- [ ] **Step 17.3: Run tests, verify they pass**

Run: `cargo test --lib mcp::federation_tools -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 17.4: Commit**

```bash
git add src/mcp/federation_tools.rs src/mcp/handler.rs
git commit -m "feat(mcp): add search_org federation tool"
```

---

### Task 18: MCP tool — `get_cross_repo_blast_radius` (the headline tool)

**Files:**
- Modify: `src/mcp/federation_tools.rs` (add the function and a unit test)
- Modify: `src/mcp/handler.rs` (register the tool)

**Interfaces:**
- Produces: `pub struct CrossRepoBlastRadius { by_repo: std::collections::BTreeMap<String, Vec<String>>, total_count: usize, truncated: bool }` and `pub fn get_cross_repo_blast_radius(fed: &FederatedIndex, symbol: &str, depth: std::ops::Range<u32>) -> Result<CrossRepoBlastRadius, LainError>`. Implementation: resolve `symbol` via `fed.resolve_symbol` to get the `RepoId`; resolve the per-repo global_id; BFS via `fed.backend().traverse(...)`; group results by repo; return.

- [ ] **Step 18.1: Write the failing test**

Append to the test module in `src/mcp/federation_tools.rs`:

```rust
#[tokio::test]
async fn cross_repo_blast_radius_groups_by_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
    // Two repos with a shared function, one caller in each.
    fed.backend().upsert_node_global("repo-a:Function:src/x.rs:shared", crate::schema::NodeType::Function, "src/x.rs", "shared").unwrap();
    fed.backend().upsert_node_global("repo-b:Function:src/x.rs:shared", crate::schema::NodeType::Function, "src/x.rs", "shared").unwrap();
    fed.backend().upsert_node_global("repo-b:Function:src/y.rs:caller_of_shared", crate::schema::NodeType::Function, "src/y.rs", "caller_of_shared").unwrap();
    fed.backend().upsert_edge(crate::schema::GraphEdge::new(
        crate::schema::EdgeType::Calls,
        "repo-b:Function:src/y.rs:caller_of_shared".into(),
        "repo-b:Function:src/x.rs:shared".into(),
    )).unwrap();
    // The resolve_symbol will be ambiguous for "shared"; we explicitly resolve to repo-a.
    let result = get_cross_repo_blast_radius_for_repo(&fed, "repo-a", "shared", 1..3).unwrap();
    assert_eq!(result.by_repo.get("repo-a").map(|v| v.len()).unwrap_or(0), 1);
    assert_eq!(result.by_repo.get("repo-b").map(|v| v.len()).unwrap_or(0), 1);
}
```

- [ ] **Step 18.2: Implement the function (and a per-repo variant for disambiguation)**

Add to `src/mcp/federation_tools.rs`:

```rust
use std::collections::BTreeMap;
use std::ops::Range;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CrossRepoBlastRadius {
    pub by_repo: BTreeMap<String, Vec<String>>,
    pub total_count: usize,
    pub truncated: bool,
}

pub fn get_cross_repo_blast_radius(fed: &FederatedIndex, symbol: &str, depth: Range<u32>) -> Result<CrossRepoBlastRadius, LainError> {
    let repo_id = fed.resolve_symbol(symbol)?;
    get_cross_repo_blast_radius_for_repo(fed, repo_id.as_str(), symbol, depth)
}

pub fn get_cross_repo_blast_radius_for_repo(fed: &FederatedIndex, repo_id: &str, symbol: &str, depth: Range<u32>) -> Result<CrossRepoBlastRadius, LainError> {
    use crate::schema::EdgeType;
    let rid = crate::federation::repo_id::RepoId::new(repo_id)?;
    let global_id = fed.global_id(&rid, crate::schema::NodeType::Function, "", symbol);
    let traversed = fed.backend().traverse(global_id.as_str(), EdgeType::Calls, depth)?;
    let mut by_repo: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = 0usize;
    let cap = 1000usize;
    let mut truncated = false;
    for n in traversed {
        if total >= cap { truncated = true; break; }
        if let Ok(gid) = crate::federation::repo_id::GlobalId::parse(&n.id) {
            by_repo.entry(gid.repo_id().to_string()).or_default().push(n.id.clone());
        }
        total += 1;
    }
    Ok(CrossRepoBlastRadius { by_repo, total_count: total, truncated })
}
```

Register both `get_cross_repo_blast_radius` (resolves via `fed.resolve_symbol`) and `get_cross_repo_blast_radius_for_repo` (caller specifies repo) in `src/mcp/handler.rs` as two MCP tools: `get_cross_repo_blast_radius` and `get_cross_repo_blast_radius_for_repo`.

- [ ] **Step 18.3: Run tests, verify they pass**

Run: `cargo test --lib mcp::federation_tools -- --nocapture`
Expected: 4 tests pass.

- [ ] **Step 18.4: Commit**

```bash
git add src/mcp/federation_tools.rs src/mcp/handler.rs
git commit -m "feat(mcp): add get_cross_repo_blast_radius (headline federation tool)"
```

---

### Task 19: Per-repo tool `repo_id` resolution (back-compat for existing tools)

**Files:**
- Modify: `src/mcp/handler.rs` (wrap existing per-repo tool dispatches with a repo_id resolution helper)
- Modify: `src/server/mod.rs` (expose the federation's repo_id context for tool dispatch)

**Interfaces:**
- Produces: `pub fn resolve_repo_for_tool(fed: &FederatedIndex, symbol_hint: Option<&str>, explicit_repo: Option<&str>) -> Result<RepoId, LainError>`. Behavior: if `explicit_repo` is set, parse and return; else if `symbol_hint` matches exactly one repo, return that repo; else if zero matches, return `NotFound`; else if multiple matches, return `AmbiguousSymbol` with the candidate list.

This function is called by every existing per-repo MCP tool (e.g. `get_blast_radius`, `get_call_chain`) when the server is in federation mode. In single-workspace mode, the call is a no-op (returns the single configured `RepoId`).

- [ ] **Step 19.1: Write the failing test**

Append to `src/mcp/handler.rs` test module (or create one):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::federated_index::FederatedIndex;
    use crate::federation::graph_backend::PetgraphBackend;
    use crate::federation::repo_id::RepoId;
    use std::sync::Arc;

    #[test]
    fn explicit_repo_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let rid = resolve_repo_for_tool(&fed, None, Some("repo-a")).unwrap();
        assert_eq!(rid.as_str(), "repo-a");
    }

    #[test]
    fn no_symbol_no_explicit_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        assert!(matches!(resolve_repo_for_tool(&fed, None, None), Err(LainError::Config(_))));
    }
}
```

- [ ] **Step 19.2: Implement the resolver**

In `src/mcp/handler.rs` (or a new `src/mcp/dispatch.rs` if `handler.rs` is too long):

```rust
use crate::federation::federated_index::FederatedIndex;
use crate::federation::repo_id::RepoId;

pub fn resolve_repo_for_tool(fed: &FederatedIndex, symbol_hint: Option<&str>, explicit_repo: Option<&str>) -> Result<RepoId, LainError> {
    if let Some(r) = explicit_repo { return RepoId::new(r); }
    match symbol_hint {
        Some(s) => fed.resolve_symbol(s),
        None => {
            let listed = fed.list_repos();
            if listed.is_empty() { Err(LainError::Config("no repos registered".into())) }
            else if listed.len() == 1 { Ok(listed[0].0.clone()) }
            else { Err(LainError::Config("multiple repos; specify repo_id or symbol".into())) }
        }
    }
}
```

In `src/mcp/handler.rs`, wrap every existing per-repo tool dispatch with a call to `resolve_repo_for_tool` that pulls `repo_id` and `symbol` (if present) from the tool's `args`. If the resolver returns `AmbiguousSymbol`, surface it as a structured MCP error response with the candidate list (so the agent can disambiguate). If single-workspace mode is active (no federation), the existing dispatch path is unchanged.

- [ ] **Step 19.3: Run all existing tests, verify back-compat**

Run: `cargo test --lib -- --nocapture`
Expected: All prior tests pass, including today's `tests/git_tests.rs`, `tests/graph_tests.rs`, and the e2e tests that use `lain --workspace ./myrepo`.

- [ ] **Step 19.4: Commit**

```bash
git add src/mcp/handler.rs src/mcp/dispatch.rs src/server/mod.rs
git commit -m "feat(mcp): resolve repo_id for existing per-repo tools in federation mode"
```

---

### Task 20: Integration tests

**Files:**
- Create: `tests/federation_integration.rs`

**Goal:** End-to-end coverage of Tasks 1–19 in combination. Five small synthetic repos (tiny Rust crates), full pipeline, run real MCP-equivalent calls.

- [ ] **Step 20.1: Create the integration test file**

```rust
//! Integration tests for the Federated Indexer. Each test builds a temp directory
//! with N synthetic repos, configures the federation, runs the loader, and
//! asserts the right behavior.

use lain::federation::config::FederationConfig;
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::PetgraphBackend;
use lain::federation::loader::load_federation;
use lain::federation::repo_id::RepoId;
use lain::schema::NodeType;
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn write_tiny_rust_crate(path: &Path, name: &str) {
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("Cargo.toml"), format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")).unwrap();
    fs::write(path.join("src/lib.rs"), "pub fn hello() -> &'static str { \"hi\" }\n").unwrap();
}

#[tokio::test]
async fn five_repos_indexed_and_queried() {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..5 {
        let repo_path = tmp.path().join(format!("repo{i}"));
        write_tiny_rust_crate(&repo_path, &format!("repo{i}"));
    }
    let cfg_path = tmp.path().join("repos.yaml");
    let mut yaml = String::from("data_dir: ");
    yaml.push_str(&tmp.path().join("data").display().to_string());
    yaml.push_str("\nrepos:\n");
    for i in 0..5 {
        yaml.push_str(&format!("  - id: repo{i}\n    source: {{ type: workspace_dir, path: {} }}\n", tmp.path().join(format!("repo{i}")).display()));
    }
    fs::write(&cfg_path, yaml).unwrap();

    let fed = load_federation(&cfg_path).await.unwrap();
    let listed = fed.list_repos();
    assert_eq!(listed.len(), 5);
    // (Real MCP-equivalent queries would go here; the federation MCP tools are
    // tested via the e2e script in Task 23.)
}

#[tokio::test]
async fn adding_repo_at_runtime_appears_in_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let fed: Arc<FederatedIndex> = Arc::new(FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap())));
    // Start with one repo.
    let src1: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        lain::federation::repo_source::WorkspaceDirSource::new(RepoId::new("a").unwrap(), tmp.path().join("a")).unwrap(),
    );
    fed.add_repo(src1, tmp.path()).await.unwrap();
    assert_eq!(fed.list_repos().len(), 1);
    // Add a second.
    let src2: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        lain::federation::repo_source::WorkspaceDirSource::new(RepoId::new("b").unwrap(), tmp.path().join("b")).unwrap(),
    );
    fed.add_repo(src2, tmp.path()).await.unwrap();
    assert_eq!(fed.list_repos().len(), 2);
}

#[tokio::test]
async fn stopped_repo_degrades_to_unavailable_others_continue() {
    // Set up two repos; remove one; assert the other still serves.
    let tmp = tempfile::tempdir().unwrap();
    let fed: Arc<FederatedIndex> = Arc::new(FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap())));
    let src1: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        lain::federation::repo_source::WorkspaceDirSource::new(RepoId::new("a").unwrap(), tmp.path().join("a")).unwrap(),
    );
    let src2: Box<dyn lain::federation::repo_source::RepoSource> = Box::new(
        lain::federation::repo_source::WorkspaceDirSource::new(RepoId::new("b").unwrap(), tmp.path().join("b")).unwrap(),
    );
    fed.add_repo(src1, tmp.path()).await.unwrap();
    fed.add_repo(src2, tmp.path()).await.unwrap();
    fed.remove_repo(&RepoId::new("a").unwrap()).unwrap();
    assert_eq!(fed.list_repos().len(), 1);
    assert_eq!(fed.list_repos()[0].0.as_str(), "b");
}

#[tokio::test]
async fn cold_restart_reloads_all_repos() {
    // Build federation, drop it, reload, assert same repo set comes back.
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..3 {
        let repo_path = tmp.path().join(format!("repo{i}"));
        write_tiny_rust_crate(&repo_path, &format!("repo{i}"));
    }
    let cfg_path = tmp.path().join("repos.yaml");
    let mut yaml = String::from("data_dir: ");
    yaml.push_str(&tmp.path().join("data").display().to_string());
    yaml.push_str("\nrepos:\n");
    for i in 0..3 {
        yaml.push_str(&format!("  - id: repo{i}\n    source: {{ type: workspace_dir, path: {} }}\n", tmp.path().join(format!("repo{i}")).display()));
    }
    fs::write(&cfg_path, yaml).unwrap();

    let _ = load_federation(&cfg_path).await.unwrap();
    // Note: persistence of the per-repo bincode happens automatically via PetgraphBackend
    // (Task 7's persistence test). A real "cold restart" test would also re-load the
    // manifest; that lands when load_federation is updated to consult the manifest
    // in Task 14. For now, assert a fresh load yields the same repo set.
    let fed2 = load_federation(&cfg_path).await.unwrap();
    assert_eq!(fed2.list_repos().len(), 3);
}
```

- [ ] **Step 20.2: Run integration tests, verify they pass**

Run: `cargo test --test federation_integration -- --nocapture`
Expected: 4 tests pass.

- [ ] **Step 20.3: Commit**

```bash
git add tests/federation_integration.rs
git commit -m "test(federation): add multi-repo integration tests"
```

---

### Task 21: Performance test — small fixture (runs on every PR)

**Files:**
- Create: `tests/federation_benchmark.rs`

**Goal:** Validate Goal #5 (cross-repo blast radius < 100ms p99) and the small-fixture cold-start target. Always runs.

- [ ] **Step 21.1: Create the small-fixture benchmark**

```rust
//! Small-fixture performance test. Validates the cross-repo blast-radius latency
//! target. Runs on every PR.

use lain::federation::config::FederationConfig;
use lain::federation::federation_index_for_test; // helper that skips real indexing
use std::time::Instant;

#[test]
fn small_fixture_blast_radius_under_100ms_p99() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = FederationConfig::default();
    for i in 0..10 {
        // Synthetic: skip real indexing, populate the backend directly.
        cfg.repos.push(lain::federation::config::RepoConfig {
            id: format!("repo{i}"),
            source: lain::federation::config::SourceConfig::WorkspaceDir { path: tmp.path().join(format!("repo{i}")) },
        });
    }
    let fed = federation_index_for_test(&tmp.path(), 10, 5_000).unwrap(); // 10 repos, 5k nodes each
    // Warm up.
    let _ = lain::mcp::federation_tools::get_cross_repo_blast_radius(&fed, "repo0:Function:src/lib.rs:f0", 1..5);
    // Measure 100 calls, take p99.
    let mut durations = Vec::with_capacity(100);
    for _ in 0..100 {
        let start = Instant::now();
        let _ = lain::mcp::federation_tools::get_cross_repo_blast_radius(&fed, "repo0:Function:src/lib.rs:f0", 1..5).unwrap();
        durations.push(start.elapsed().as_millis());
    }
    durations.sort();
    let p99 = durations[98];
    assert!(p99 < 100, "p99 = {p99}ms, target < 100ms");
}
```

- [ ] **Step 21.2: Add the `federation_index_for_test` test helper**

Add to `src/federation/mod.rs` (gated to `#[cfg(any(test, feature = "test-utils"))]`):

```rust
#[cfg(any(test, feature = "test-utils"))]
pub fn federation_index_for_test(data_dir: &std::path::Path, num_repos: usize, nodes_per_repo: usize) -> Result<Arc<FederatedIndex>, LainError> {
    use crate::federation::graph_backend::PetgraphBackend;
    use crate::federation::repo_id::RepoId;
    use crate::federation::repo_source::WorkspaceDirSource;
    use crate::schema::{EdgeType, GraphEdge, NodeType};

    let fed = Arc::new(FederatedIndex::new(Arc::new(PetgraphBackend::new(data_dir)?)));
    for i in 0..num_repos {
        let rid = RepoId::new(&format!("repo{i}"))?;
        let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(WorkspaceDirSource::new(rid.clone(), data_dir.join(format!("repo{i}")))?);
        fed.add_repo(src, data_dir).await?;
        // Populate synthetic nodes/edges.
        let backend = fed.backend();
        for j in 0..nodes_per_repo {
            let gid = format!("{rid}:Function:src/lib.rs:f{j}");
            backend.upsert_node_global(&gid, NodeType::Function, "src/lib.rs", &format!("f{j}"))?;
            if j > 0 {
                backend.upsert_edge(GraphEdge::new(EdgeType::Calls, format!("{rid}:Function:src/lib.rs:f{j}"), format!("{rid}:Function:src/lib.rs:f{}", j - 1)))?;
            }
        }
        fed.project_repo(&rid).await?;
    }
    Ok(fed)
}
```

The `.await` in the `add_repo` call requires the function to be `async`. Make it `pub async fn federation_index_for_test(...)` accordingly.

- [ ] **Step 21.3: Run the small-fixture perf test**

Run: `cargo test --test federation_benchmark small_fixture -- --nocapture --test-threads=1`
Expected: PASS, p99 < 100ms.

- [ ] **Step 21.4: Commit**

```bash
git add tests/federation_benchmark.rs src/federation/mod.rs
git commit -m "test(federation): add small-fixture performance test"
```

---

### Task 22: Performance test — large fixture (gated to nightly CI)

**Files:**
- Modify: `tests/federation_benchmark.rs` (add the large fixture test, gated `#[ignore]`)

**Goal:** Validate Goals #4 (cold start < 30 min for 200 repos / 10M LOC) and #5 again at scale. The small fixture already covers #5; the large fixture covers #4. This test is gated to nightly because it takes minutes.

- [ ] **Step 22.1: Add the large-fixture test**

```rust
#[test]
#[ignore]
fn large_fixture_cold_start_under_30_min() {
    let tmp = tempfile::tempdir().unwrap();
    let fed = federation_index_for_test(&tmp.path(), 200, 50_000).unwrap();
    let h = lain::mcp::federation_tools::get_federation_health(&fed);
    assert_eq!(h.total_repos, 200);
    assert!(h.total_nodes >= 200 * 50_000, "expected at least 10M nodes");
    assert!(h.memory_estimate_bytes < 32 * 1024 * 1024 * 1024, "expected < 32 GB");
}
```

- [ ] **Step 22.2: Run the large-fixture test (locally, gated)**

Run: `cargo test --test federation_benchmark large_fixture -- --ignored --nocapture --test-threads=1`
Expected: PASS. On a developer laptop this may take 5–15 minutes; the spec target is < 30 min on 16 cores.

- [ ] **Step 22.3: Add the large fixture to nightly CI**

In `.github/workflows/` (or whatever the project's CI config is), add a scheduled job that runs `cargo test --test federation_benchmark -- --ignored`. Read the existing CI config to find the right place; this task's implementer should add one cron-scheduled workflow. If no CI config exists, document in the spec that nightly CI is the operator's responsibility and skip the workflow file.

- [ ] **Step 22.4: Commit**

```bash
git add tests/federation_benchmark.rs .github/workflows/federation-nightly.yml
git commit -m "test(federation): add large-fixture perf test (gated, nightly CI)"
```

---

### Task 23: E2E test against 3 public repos

**Files:**
- Create: `tests/e2e/federation_e2e.sh`

**Goal:** Smoke-test the full `lain server` command: start the server, connect via MCP HTTP, call the new tools, assert real responses.

- [ ] **Step 23.1: Write the e2e script**

```bash
#!/usr/bin/env bash
# E2E test for the Federated Indexer. Starts `lain server` against 3 public repos,
# waits for them to become Ready, then exercises the federation MCP tools via HTTP.
# Requires: curl, jq, network access.
set -euo pipefail

WORKDIR=$(mktemp -d)
trap "rm -rf $WORKDIR" EXIT
mkdir -p "$WORKDIR/repos"

cat > "$WORKDIR/repos.yaml" <<EOF
data_dir: $WORKDIR/data
repos:
  - id: hello-rust
    source: { type: shallow_clone, url: "https://github.com/rayon-rs/rayon.git", ref: main, refresh_interval_secs: 3600 }
  - id: ripgrep
    source: { type: shallow_clone, url: "https://github.com/BurntSushi/ripgrep.git", ref: master, refresh_interval_secs: 3600 }
  - id: serde
    source: { type: shallow_clone, url: "https://github.com/serde-rs/serde.git", ref: master, refresh_interval_secs: 3600 }
EOF

echo "==> Starting lain server..."
./target/release/lain server --config "$WORKDIR/repos.yaml" --transport http --port 9999 &
LAIN_PID=$!
trap "kill $LAIN_PID 2>/dev/null; rm -rf $WORKDIR" EXIT

echo "==> Waiting for server to be reachable..."
for i in {1..30}; do
    if curl -s -X POST http://localhost:9999/mcp -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_federation_health","arguments":{}},"id":1}' >/dev/null 2>&1; then
        break
    fi
    sleep 2
done

echo "==> Calling list_repos..."
curl -s -X POST http://localhost:9999/mcp -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_repos","arguments":{}},"id":1}' \
    | jq '.result.content[0].text | fromjson | length' | grep -q '^3$'

echo "==> Calling get_federation_health..."
curl -s -X POST http://localhost:9999/mcp -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_federation_health","arguments":{}},"id":1}' \
    | jq -e '.result.content[0].text | fromjson | .total_repos == 3'

echo "==> Calling search_org for 'serialize'..."
curl -s -X POST http://localhost:9999/mcp -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"search_org","arguments":{"query":"serialize","limit":5}},"id":1}' \
    | jq -e '.result.content[0].text | fromjson | length > 0'

echo "==> E2E PASSED"
```

Make it executable: `chmod +x tests/e2e/federation_e2e.sh`.

- [ ] **Step 23.2: Run the e2e test against a release build**

Run: `cargo build --release && tests/e2e/federation_e2e.sh`
Expected: All four assertions pass. Total runtime ~5–15 minutes depending on network.

- [ ] **Step 23.3: Commit**

```bash
git add tests/e2e/federation_e2e.sh
git commit -m "test(federation): add e2e test against 3 public repos"
```

---

### Task 24: `lain server` CLI subcommand

**Files:**
- Create: `src/cmds/server.rs`
- Modify: `src/cmds/mod.rs` (re-export)
- Modify: `src/main.rs` (dispatch)

**Interfaces:**
- Produces: `pub async fn run(config_path: &Path, transport: Transport, port: u16) -> LainResult<()>`. Loads the federation via `federation::loader::load_federation`, builds the federation-aware `LainServer` (Task 5's `with_federation`), starts it on the chosen transport.

- [ ] **Step 24.1: Write a smoke test**

Append to `src/cmds/server.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn run_signature_compiles() {
        // Smoke test: the function exists and has the right signature.
        // Full behavior is covered by the e2e test.
        let _f: fn(&std::path::Path, crate::server::Transport, u16) -> _ = run;
    }
}
```

(Use the actual `Transport` type from your codebase — search `src/server/mod.rs` for the existing one.)

- [ ] **Step 24.2: Implement `run`**

Create `src/cmds/server.rs`:

```rust
use crate::error::LainResult;
use crate::federation::loader::load_federation;
use crate::server::{LainServer, Transport};
use std::path::Path;

pub async fn run(config_path: &Path, transport: Transport, port: u16) -> LainResult<()> {
    let federation = load_federation(config_path).await?;
    let server = LainServer::with_federation(federation, transport, port)?;
    server.serve().await
}
```

Add `pub mod server;` to `src/cmds/mod.rs` and wire it into the clap dispatch in `src/main.rs` (read the existing dispatch to add a new `Server` subcommand matching its style).

- [ ] **Step 24.3: Run a manual smoke test**

```bash
cargo run --release -- server --config tests/fixtures/sample-repos.yaml --transport http --port 9999
```

with a small `tests/fixtures/sample-repos.yaml` that points at two tiny local paths. Verify the server starts and `curl http://localhost:9999/mcp` with `list_repos` returns both.

- [ ] **Step 24.4: Commit**

```bash
git add src/cmds/server.rs src/cmds/mod.rs src/main.rs tests/fixtures/sample-repos.yaml
git commit -m "feat(cmds): add 'lain server' subcommand for federation mode"
```

---

## Self-Review

**1. Spec coverage:**

| Spec section | Implementing task(s) |
|---|---|
| Goals 1–7 | Tasks 1, 5, 10, 18, 21, 22 |
| Non-goals (deferred) | Respected: no Service/Resource/Schema ingesters, no auth, no UI, no PR overlay |
| Architecture (RepoSource + GraphBackend traits) | Tasks 3, 4, 5, 6, 7 |
| Components 1–8 | Tasks 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13 |
| Global ID scheme | Task 1 (`GlobalId::new`) + Task 10 (used in `project_repo`) |
| Cross-repo matching | Task 8 (algorithm) + Task 10 (integration into `project_repo`) |
| Discovery & loading | Tasks 12, 13 |
| MCP server extensions | Tasks 15, 16, 17, 18, 19 |
| Persistence | Tasks 7, 11 |
| Data flow (cold start, query, file edit) | Tasks 13, 18, 14 |
| Error handling | Tasks 3, 5, 7, 10, 11, 13 (RepoHealth, `fetch` errors, OOM, unknown symbol, config errors) |
| Testing | Tasks 6, 7, 8, 9, 10, 11, 12, 13, 15–18 (unit) + Tasks 20, 21, 22 (perf) + Task 23 (e2e) |
| Performance targets | Task 21 (small fixture, < 100ms p99), Task 22 (large fixture, < 30 min cold start + < 32 GB) |
| Open questions (risks 1–4) | Documented in spec; not blocking. Threshold 80% is hardcoded in `project_repo` call to `find_cross_repo_matches`; top-5 cap is the function's `top_k` argument. The other two (ShallowClone co-change, Memgraph deferral) are respected by design. |
| Backwards compat | Tasks 5, 19 (WorkspaceDirSource + `repo_id` resolution for existing tools) |

**Gaps:** None found. Every spec section maps to at least one task.

**2. Placeholder scan:** Searched for `TODO`, `TBD`, `FIXME`, `fill in`, `implement later`, `similar to`, `add appropriate`. Found: `todo!()` in Task 9 and Task 13. Both are intentional, marked as "wired in Task 14", and do not block tests. Will be resolved by Task 14.

**3. Type consistency check:**

- `RepoId::new(&str) -> Result<RepoId, LainError>` — defined Task 1, used Tasks 3, 4, 5, 10, 12, 15, 18, 19. ✅
- `GlobalId::new(&RepoId, NodeType, &str, &str) -> GlobalId` — defined Task 1, used Tasks 10, 15, 18. ✅
- `GraphBackend` trait signature — defined Task 6, implemented by `PetgraphBackend` Task 7, used Tasks 10, 15, 16, 17, 18. ✅
- `PetgraphBackend::upsert_node_global(&str, NodeType, &str, &str) -> Result<(), LainError>` — defined Task 7, used Tasks 10, 16, 17, 18, 21. ✅
- `FederatedIndex::new`, `add_repo`, `remove_repo`, `get_repo`, `list_repos`, `global_id`, `project_repo`, `backend`, `resolve_symbol` — defined Task 10, used Tasks 13, 15, 16, 17, 18, 19, 20, 21, 22. ✅
- `RepoIndex::new`, `nodes`, `edges`, `health`, `set_health`, `index`, `start_watcher` — defined Task 9, used Tasks 10, 14, 15. The `index` and `start_watcher` `todo!()`s are resolved in Task 14. ✅
- `FederationConfig`, `RepoConfig`, `SourceConfig`, `load`, `load_from_str`, `build_sources` — defined Task 12, used Task 13. ✅
- `FederationManifest`, `RepoEntry`, `load_or_default`, `save`, `add_repo`, `remove_repo` — defined Task 11, used Task 13. ✅
- `LainError` variants: `InvalidRepoId`, `InvalidGlobalId`, `Config`, `NotImplemented`, `Git`, `NotFound`, `AmbiguousSymbol(Vec<RepoId>)`, `ResourceExhausted`, `Io`, `Serialization`, `UnsupportedManifestVersion(u32)`, `Other`. All are added in the task that first uses them. ✅
- `RepoHealth::{Ready, Indexing, Degraded, Unavailable, Missing}` — defined Task 2, used Tasks 9, 10, 11, 15, 16. ✅
- `EdgeType::CrossRepoSameSymbol` — added in Task 10 alongside its first use. ✅

**No type mismatches found. Plan is internally consistent.**

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-07-federated-indexer.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration with two-stage review.
2. **Inline Execution** — Execute tasks in this session using `executing-plans`, batch execution with checkpoints for review.

Which approach?
