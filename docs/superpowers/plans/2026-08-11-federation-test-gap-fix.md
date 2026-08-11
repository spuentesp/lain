# Federation Test Gap Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the federation's `project_repo` actually project per-repo edges into the global backend (Pass A) and produce cross-repo `Calls` edges for unambiguous references to functions in other repos (Pass B), then prove both via per-PR and nightly test fixtures.

**Architecture:** Two-PR sequence. PR 1 modifies `src/federation/federated_index.rs::project_repo` to add the two passes plus the new `RepoIndex::external_calls()` accessor, and adds two failing tests in `tests/federation_integration.rs` that prove both passes work. PR 2 adds the full D fixture (`tests/federation_cross_repo_e2e.rs` with 8 tests), extends `tests/e2e/federation_e2e.sh` with OTel Demo polyglot assertions, and updates docs.

**Tech Stack:** Rust (cargo test), bash (e2e shell), `git2` (test fixture git init), `tempfile` (test tempdirs), Docusaurus-style markdown (docs).

---

## Global Constraints

From the spec (every task implicitly includes these):

- `--features test-utils` flag is required to build the integration tests that call `load_federation` and `RepoIndex::new`. Same as `tests/federation_benchmark.rs:12`.
- Conservative semantics: do NOT fabricate cross-repo `Calls` edges when `symbol_to_repos.get(name)` returns ≥2 entries (ambiguous) or no entries (not found). Pass B only acts on the unambiguous case.
- Backward compat: existing `tests/federation_integration.rs` and `tests/federation_benchmark.rs` must still pass after PR 1 lands. The benchmark uses `federation_index_for_test` which inserts directly into the backend (bypassing `project_repo`), so it is unaffected by the engine change.
- Bash: requires `python3` (no `jq`); use `python3 -c '...'` for JSON parsing in the e2e shell.
- Network: A fixture requires cloning `https://github.com/open-telemetry/opentelemetry-demo.git` with `--depth 1`. Only runs in nightly / manual triggers.
- `LainError` variants used in test assertions: `NotFound(String)` and `AmbiguousSymbol(Vec<RepoId>)` (`src/error.rs:39, 65`).
- Per-repo edges from `RepoIndex::edges()` are `Vec<GraphEdge>` (`src/federation/repo_index.rs:106`). `GraphEdge` carries only `(edge_type, source_id, target_id, weight)` — to re-key an edge to global ids, the implementation must look up the source's and target's `(node_type, path, name)` from `RepoIndex::nodes()`.

---

## File Structure

**PR 1 — Engine change:**
- Modify: `src/federation/federated_index.rs` (add Pass A and Pass B to `project_repo`)
- Modify: `src/federation/repo_index.rs` (add `external_calls()` accessor returning `(global_source_id, target_name)` tuples for imported `Calls` edges)
- Modify: `tests/federation_integration.rs` (add two failing tests: `project_repo_projects_intra_repo_calls_edges`, `project_repo_produces_cross_repo_calls_edges`)

**PR 2 — Test fixtures + docs:**
- Create: `tests/federation_cross_repo_e2e.rs` (new file, 8 tests)
- Modify: `tests/e2e/federation_e2e.sh` (add OTel Demo clone + 12 workspace_dir entries + 4 new assertion blocks)
- Modify: `docs/FEDERATION.md` (smoke-test and performance sections)

No new files in `src/` other than what PR 1 modifies. No new CI workflows.

---

## PR 1 — Engine Change

### Task 1: Add the failing test for Pass A (intra-repo Calls projection)

**Files:**
- Modify: `tests/federation_integration.rs` (append a single test at the bottom)

**Interfaces:**
- Consumes: existing `load_federation`, `GraphBackend::find_path`
- Produces: a `#[tokio::test] fn project_repo_projects_intra_repo_calls_edges` that fails today

- [ ] **Step 1: Build the 2-crate fixture builder inline**

Append to `tests/federation_integration.rs`:

```rust
#[tokio::test]
async fn project_repo_projects_intra_repo_calls_edges() {
    use lain::federation::repo_id::RepoId;
    use lain::federation::repo_source::WorkspaceDirSource;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let shared = root.join("shared");
    let auth_svc = root.join("auth-svc");

    for sub in [&shared, &auth_svc] {
        std::fs::create_dir_all(sub.join("src")).unwrap();
        git2::Repository::init(sub).expect("git init");
    }
    std::fs::write(
        shared.join("Cargo.toml"),
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ).unwrap();
    std::fs::write(
        shared.join("src/lib.rs"),
        "pub fn inner_hash(s: &str) -> u64 { 0 }\n\
         pub fn hash(s: &str) -> u64 { inner_hash(s) }\n",
    ).unwrap();
    std::fs::write(
        auth_svc.join("Cargo.toml"),
        "[package]\nname = \"auth-svc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         [dependencies]\nshared = { path = \"../shared\" }\n",
    ).unwrap();
    std::fs::write(
        auth_svc.join("src/lib.rs"),
        "pub fn auth(s: &str) -> bool { shared::hash(s) > 0 }\n",
    ).unwrap();

    let cfg_path = root.join("repos.yaml");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(&cfg_path, format!(
        "data_dir: {}\nrepos:\n  - id: shared\n    source: {{ type: workspace_dir, path: {} }}\n  - id: auth-svc\n    source: {{ type: workspace_dir, path: {} }}\n",
        data_dir.display(), shared.display(), auth_svc.display(),
    )).unwrap();

    let fed = load_federation(&cfg_path).await.unwrap();

    // Pass A: per-repo Calls edges must be projected to the global backend.
    // hash (in shared) calls inner_hash (in shared) — this is an intra-repo
    // Calls edge that must exist in the global graph after project_repo.
    let hash_global = "shared:Function:src/lib.rs:hash".to_string();
    let inner_global = "shared:Function:src/lib.rs:inner_hash".to_string();
    let path = fed.backend().find_path(&hash_global, &inner_global).unwrap();
    assert!(
        !path.is_empty(),
        "expected non-empty path from shared::hash to shared::inner_hash; \
         Pass A (project per-repo edges) not yet implemented"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --features test-utils --test federation_integration project_repo_projects_intra_repo_calls_edges -- --nocapture --test-threads=1`
Expected: FAIL with "Pass A (project per-repo edges) not yet implemented" or similar assertion panic.

- [ ] **Step 3: Commit the failing test**

```bash
git add tests/federation_integration.rs
git commit -m "test(federation): add failing test for intra-repo Calls projection (Pass A)"
```

---

### Task 2: Implement Pass A — project per-repo edges in `project_repo`

**Files:**
- Modify: `src/federation/federated_index.rs::project_repo`

**Interfaces:**
- Consumes: `repo_index.edges()` (existing), `repo_index.nodes()` (existing), `GlobalId::new` (existing)
- Produces: every per-repo edge upserted into the global backend with global-id source/target

- [ ] **Step 1: Read `src/federation/federated_index.rs::project_repo` (lines 84–123) and `src/schema.rs::GraphEdge` (lines 163–181)**

Identify:
- `GraphEdge` carries only `(edge_type, source_id, target_id, weight)` — no `(kind, path, name)`.
- `GlobalId::new(repo, kind, path, name)` produces the global id string.
- To re-key an edge, look up the source's and target's `(node_type, path, name)` via the per-repo `nodes()` set, indexed by their local id.

- [ ] **Step 2: Build a local-id → (kind, path, name) lookup**

In `project_repo`, after fetching `let nodes = repo.nodes();` and before the existing cross-repo matching loop, build:

```rust
let mut local_id_to_triple: HashMap<String, (NodeType, String, String)> = HashMap::new();
for n in &nodes {
    local_id_to_triple.insert(n.id.clone(), (n.node_type.clone(), n.path.clone(), n.name.clone()));
}
```

(`HashMap` and `NodeType` are already imported in the file.)

- [ ] **Step 3: Add Pass A — iterate per-repo edges and upsert with re-keyed ids**

After building `local_id_to_triple` and before the existing `find_cross_repo_matches` loop, insert:

```rust
// Pass A: project per-repo edges into the global backend.
for edge in repo.edges() {
    let Some((src_kind, src_path, src_name)) = local_id_to_triple.get(&edge.source_id) else {
        tracing::debug!(source_id = %edge.source_id, "skipping edge: source node not in local index");
        continue;
    };
    let Some((tgt_kind, tgt_path, tgt_name)) = local_id_to_triple.get(&edge.target_id) else {
        tracing::debug!(target_id = %edge.target_id, "skipping edge: target node not in local index");
        continue;
    };
    let global_source = GlobalId::new(id, src_kind.clone(), src_path, src_name).as_str().to_string();
    let global_target = GlobalId::new(id, tgt_kind.clone(), tgt_path, tgt_name).as_str().to_string();
    let mut rewritten = edge.clone();
    rewritten.source_id = global_source;
    rewritten.target_id = global_target;
    self.backend.upsert_edge(rewritten)?;
}
```

- [ ] **Step 4: Run the failing test from Task 1; verify it passes**

Run: `cargo test --features test-utils --test federation_integration project_repo_projects_intra_repo_calls_edges -- --nocapture --test-threads=1`
Expected: PASS

- [ ] **Step 5: Commit Pass A**

```bash
git add src/federation/federated_index.rs
git commit -m "feat(federation): Pass A — project per-repo edges into global backend"
```

---

### Task 3: Add the failing test for Pass B (cross-repo Calls resolution)

**Files:**
- Modify: `tests/federation_integration.rs` (append one more test)

**Interfaces:**
- Consumes: existing `load_federation`, `GraphBackend::find_path`
- Produces: a `#[tokio::test] fn project_repo_produces_cross_repo_calls_edges` that fails today

- [ ] **Step 1: Add the failing test**

Append to `tests/federation_integration.rs`:

```rust
#[tokio::test]
async fn project_repo_produces_cross_repo_calls_edges() {
    // Same 2-crate fixture as the Pass A test.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let shared = root.join("shared");
    let auth_svc = root.join("auth-svc");

    for sub in [&shared, &auth_svc] {
        std::fs::create_dir_all(sub.join("src")).unwrap();
        git2::Repository::init(sub).expect("git init");
    }
    std::fs::write(
        shared.join("Cargo.toml"),
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ).unwrap();
    std::fs::write(
        shared.join("src/lib.rs"),
        "pub fn hash(s: &str) -> u64 { 0 }\n",
    ).unwrap();
    std::fs::write(
        auth_svc.join("Cargo.toml"),
        "[package]\nname = \"auth-svc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         [dependencies]\nshared = { path = \"../shared\" }\n",
    ).unwrap();
    std::fs::write(
        auth_svc.join("src/lib.rs"),
        "pub fn auth(s: &str) -> bool { shared::hash(s) > 0 }\n",
    ).unwrap();

    let cfg_path = root.join("repos.yaml");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(&cfg_path, format!(
        "data_dir: {}\nrepos:\n  - id: shared\n    source: {{ type: workspace_dir, path: {} }}\n  - id: auth-svc\n    source: {{ type: workspace_dir, path: {} }}\n",
        data_dir.display(), shared.display(), auth_svc.display(),
    )).unwrap();

    let fed = load_federation(&cfg_path).await.unwrap();

    // Pass B: auth-svc::auth calls shared::hash. After Pass A projects the
    // intra-repo Calls (none here, since auth's call target is in another repo),
    // Pass B must insert a cross-repo Calls edge from auth-svc::auth to
    // shared::hash.
    let auth_global = "auth-svc:Function:src/lib.rs:auth".to_string();
    let hash_global = "shared:Function:src/lib.rs:hash".to_string();
    let path = fed.backend().find_path(&auth_global, &hash_global).unwrap();
    assert!(
        !path.is_empty(),
        "expected non-empty path from auth-svc::auth to shared::hash; \
         Pass B (cross-repo Calls resolution) not yet implemented"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --features test-utils --test federation_integration project_repo_produces_cross_repo_calls_edges -- --nocapture --test-threads=1`
Expected: FAIL with "Pass B (cross-repo Calls resolution) not yet implemented" or similar.

- [ ] **Step 3: Commit the failing test**

```bash
git add tests/federation_integration.rs
git commit -m "test(federation): add failing test for cross-repo Calls resolution (Pass B)"
```

---

### Task 4: Add `RepoIndex::external_calls()` accessor

**Files:**
- Modify: `src/federation/repo_index.rs` (add `external_calls()` method)

**Interfaces:**
- Consumes: existing `RepoIndex::nodes()`, `RepoIndex::edges()`
- Produces: `pub fn external_calls(&self) -> Vec<(String, String)>` — for every `Calls` edge whose target is NOT defined in this repo, return `(source_local_id, target_name)`. The source is included so the caller can re-key it to a global id; the target's local id is replaced by name because the global id requires a target repo (which the caller determines via `symbol_to_repos`).

- [ ] **Step 1: Read `src/federation/repo_index.rs` to see the existing accessors (lines 102–110)**

- [ ] **Step 2: Add the accessor after `edges()`**

```rust
/// For every `Calls` edge in this repo's per-repo graph whose target is
/// NOT a function defined in this repo (i.e., the target is an imported
/// reference name), return `(source_local_id, target_name)`.
///
/// Used by `FederatedIndex::project_repo` Pass B to resolve cross-repo
/// `Calls` edges via the federation's `symbol_to_repos` index.
pub fn external_calls(&self) -> Vec<(String, String)> {
    let local_node_names: std::collections::HashSet<String> = self
        .nodes()
        .into_iter()
        .map(|n| n.name)
        .collect();
    let mut out = Vec::new();
    for edge in self.edges() {
        if edge.edge_type != EdgeType::Calls {
            continue;
        }
        // The target's local id encodes the name in many per-repo graphs
        // (the per-repo GraphDatabase uses `name` as part of its id).
        // If the target's name isn't in the local node set, it's external.
        let target_name = edge.target_id.clone();
        if !local_node_names.contains(&target_name) {
            out.push((edge.source_id, target_name));
        }
    }
    out
}
```

**Important caveat for the implementer:** the per-repo `GraphDatabase` may use UUID v5 ids (not raw names) for nodes — see `src/schema.rs::GraphNode::generate_id`. If so, `target_name` will be a UUID, not a human-readable name, and the comparison logic above will be wrong. The implementer must inspect the actual per-repo id format (read `src/graph.rs` and `src/treesitter.rs` to understand how per-repo edges are emitted) and adapt the accessor to extract the target's `name` field rather than its raw id. The simplest correct shape: for each `Calls` edge, look up the source's `name` in the local nodes map by its source_id, and pass that name as the second tuple element. Adjust as needed.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --features test-utils`
Expected: compiles with no errors.

---

### Task 5: Implement Pass B — cross-repo Calls resolution in `project_repo`

**Files:**
- Modify: `src/federation/federated_index.rs::project_repo`

**Interfaces:**
- Consumes: `repo_index.external_calls()` (added in Task 4), `self.symbol_to_repos` (existing), `self.global_id` (existing)
- Produces: cross-repo `Calls` edges in the global backend for unambiguous external references

- [ ] **Step 1: Add Pass B to `project_repo` after Pass A**

After the Pass A loop and before the existing `find_cross_repo_matches` loop, insert:

```rust
// Pass B: resolve per-repo Calls edges that target functions in other repos.
for (source_local_id, target_name) in repo.external_calls() {
    // Look up the target's owning repos in symbol_to_repos.
    let owners = match self.symbol_to_repos.get(&target_name) {
        Some(entries) => entries.clone(),
        None => {
            tracing::debug!(name = %target_name, "skipping cross-repo Calls: not in symbol_to_repos");
            continue;
        }
    };
    if owners.len() != 1 {
        tracing::debug!(name = %target_name, count = owners.len(), "skipping cross-repo Calls: ambiguous");
        continue;
    }
    let target_repo = &owners[0];
    if target_repo == id {
        // Already an intra-repo call (target defined in this repo); Pass A handled it.
        continue;
    }
    // Re-key the source local id to a global id.
    let Some((src_kind, src_path, src_name)) = local_id_to_triple.get(&source_local_id) else {
        tracing::debug!(source_id = %source_local_id, "skipping cross-repo Calls: source not in local index");
        continue;
    };
    let global_source = GlobalId::new(id, src_kind.clone(), src_path, src_name).as_str().to_string();
    // Build the global id for the target in the other repo.
    let global_target = GlobalId::new(target_repo, crate::schema::NodeType::Function, "", &target_name)
        .as_str()
        .to_string();
    self.backend.upsert_edge(GraphEdge::new(
        EdgeType::Calls,
        global_source,
        global_target,
    ))?;
}
```

(Note: the `path: ""` placeholder is conservative — Pass B doesn't know the target function's path. The implementer may refine this by also looking up the target's `(kind, path, name)` in the federation's symbol index if a path-resolved lookup is needed. For Pass B's correctness, the `(kind, path, name)` triple must match what `project_repo` upserted in Pass A; if the target's actual path is `src/lib.rs`, the edge won't match. Fix in the implementer if `find_path` shows mismatches.)

- [ ] **Step 2: Run the failing test from Task 3; verify it passes**

Run: `cargo test --features test-utils --test federation_integration project_repo_produces_cross_repo_calls_edges -- --nocapture --test-threads=1`
Expected: PASS

- [ ] **Step 3: If it fails, debug the path lookup**

The most likely failure is the `path: ""` placeholder issue noted above. Inspect `find_path`'s input format and `GraphBackend::find_nodes_by_name`'s output to determine the correct path to use. Fix Pass B accordingly, re-run.

- [ ] **Step 4: Commit Pass B**

```bash
git add src/federation/federated_index.rs src/federation/repo_index.rs
git commit -m "feat(federation): Pass B — cross-repo Calls resolution via symbol_to_repos"
```

---

### Task 6: Verify no regression to existing federation tests

**Files:**
- No code changes

- [ ] **Step 1: Run the full federation integration tests**

Run: `cargo test --features test-utils --test federation_integration -- --nocapture --test-threads=1`
Expected: ALL tests pass (the existing 6 tests in the file plus the 2 added in Tasks 1 and 3).

- [ ] **Step 2: Run the federation benchmark tests**

Run: `cargo test --features test-utils --test federation_benchmark -- --nocapture --test-threads=1`
Expected: small fixture p99 < 100ms (latency budget unchanged); large fixture is `#[ignore]`d (don't run).

- [ ] **Step 3: If any test regresses, investigate and fix**

Most likely cause: Pass A's `upsert_edge` for every per-repo edge is now doing N writes per repo, where N = per-repo edge count. The benchmark fixture inserts edges directly via `insert_edges_batch` so it's unaffected; the integration tests use real per-repo indexing which is small enough to be unaffected.

If a test fails, the failure mode will be a count mismatch or a timeout. Diagnose by running the failing test in isolation with `--nocapture`.

- [ ] **Step 4: Commit any fixes if needed (no commit if all green)**

```bash
git add <files>
git commit -m "fix(federation): <description of fix>"
```

---

### Task 7: Tag PR 1 as ready

**Files:**
- No code changes

- [ ] **Step 1: Confirm all PR 1 commits are in place**

```bash
git log --oneline -10
```

Expected to see, in order:
- `test(federation): add failing test for intra-repo Calls projection (Pass A)`
- `feat(federation): Pass A — project per-repo edges into global backend`
- `test(federation): add failing test for cross-repo Calls resolution (Pass B)`
- `feat(federation): Pass B — cross-repo Calls resolution via symbol_to_repos`
- (any fix commits)

- [ ] **Step 2: Open the PR**

If the user has a PR-creation flow, open PR 1 against `main` with title `feat(federation): project per-repo + cross-repo Calls edges in project_repo`. Description should reference the spec (`docs/superpowers/specs/2026-08-11-federation-test-gap-fix-design.md`) and note that PR 2 follows.

**Stop here. Do not proceed to PR 2 tasks until PR 1 has been reviewed and merged.**

---

## PR 2 — Test Fixtures + Docs (after PR 1 merges)

### Task 8: Create the D fixture file with the 3-crate builder and the first 3 tests

**Files:**
- Create: `tests/federation_cross_repo_e2e.rs`

**Interfaces:**
- Consumes: `load_federation` (`src/federation/loader.rs`), `FederatedIndex::resolve_symbol`, `mcp::federation_tools::search_org`
- Produces: 3 `#[tokio::test]` functions covering resolver + search_org

- [ ] **Step 1: Create the file with imports, fixture builder, and helper**

```rust
//! Cross-repo federation reasoning end-to-end tests.
//!
//! Builds a 3-crate fixture (shared, auth-svc, db-client) in tempdirs and
//! exercises the federation's semantic contracts end-to-end. Runs on every PR
//! via `cargo test --test federation_cross_repo_e2e`. Requires
//! `--features test-utils` like the existing benchmark file.

use lain::federation::loader::load_federation;
use lain::federation::repo_id::RepoId;
use lain::mcp::federation_tools::search_org;
use std::path::Path;

const SHARED_LIB: &str = "\
pub fn verify_token(s: &str) -> bool { !s.is_empty() }
pub fn hash(s: &str) -> u64 {
    let inner = inner_hash(s);
    inner
}
pub fn inner_hash(s: &str) -> u64 {
    let mut h: u64 = 0;
    for b in s.bytes() { h = h.wrapping_mul(31).wrapping_add(b as u64); }
    h
}
";

const DB_CLIENT_LIB: &str = "\
pub fn connect() -> bool {
    crate::verify_token(\"...\")
}
pub fn verify_token(s: &str) -> bool { false } // duplicate for AmbiguousSymbol
";

const AUTH_SVC_LIB: &str = "\
pub fn auth(s: &str) -> bool {
    shared::hash(s) > 0
}
";

fn write_three_dependent_crates(root: &Path) {
    let shared = root.join("shared");
    let db_client = root.join("db-client");
    let auth_svc = root.join("auth-svc");
    for (sub, name, lib) in [
        (&shared, "shared", SHARED_LIB),
        (&db_client, "db-client", DB_CLIENT_LIB),
        (&auth_svc, "auth-svc", AUTH_SVC_LIB),
    ] {
        std::fs::create_dir_all(sub.join("src")).unwrap();
        let mut cargo = format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
        );
        if name != "shared" {
            cargo.push_str("[dependencies]\nshared = { path = \"../shared\" }\n");
        }
        std::fs::write(sub.join("Cargo.toml"), cargo).unwrap();
        std::fs::write(sub.join("src/lib.rs"), lib).unwrap();
        git2::Repository::init(sub).expect("git init");
    }
}

fn write_repos_yaml(root: &Path) -> std::path::PathBuf {
    let cfg = root.join("repos.yaml");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let yaml = format!(
        "data_dir: {}\nrepos:\n  - id: shared\n    source: {{ type: workspace_dir, path: {} }}\n\
         - id: db-client\n    source: {{ type: workspace_dir, path: {} }}\n\
         - id: auth-svc\n    source: {{ type: workspace_dir, path: {} }}\n",
        data.display(),
        root.join("shared").display(),
        root.join("db-client").display(),
        root.join("auth-svc").display(),
    );
    std::fs::write(&cfg, yaml).unwrap();
    cfg
}

async fn build_federation() -> (
    tempfile::TempDir,
    std::sync::Arc<lain::federation::federated_index::FederatedIndex>,
) {
    let tmp = tempfile::tempdir().unwrap();
    write_three_dependent_crates(tmp.path());
    let cfg = write_repos_yaml(tmp.path());
    let fed = load_federation(&cfg).await.unwrap();
    (tmp, fed)
}

#[tokio::test]
async fn cross_repo_resolver_unique_owner() {
    let (_tmp, fed) = build_federation().await;
    let result = fed.resolve_symbol("hash");
    let repo_id = result.expect("hash should resolve to a unique repo");
    assert_eq!(repo_id.as_str(), "shared");
}

#[tokio::test]
async fn cross_repo_resolver_ambiguous() {
    let (_tmp, fed) = build_federation().await;
    let result = fed.resolve_symbol("verify_token");
    let err = result.expect_err("verify_token exists in 2 repos; should be ambiguous");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("shared") && msg.contains("db-client"),
        "expected AmbiguousSymbol mentioning shared + db-client, got: {msg}"
    );
}

#[tokio::test]
async fn cross_repo_search_org_finds_shared_concepts() {
    let (_tmp, fed) = build_federation().await;
    let hits = search_org(&fed, "verify", 10).expect("search_org should succeed");
    let distinct_repos: std::collections::HashSet<_> =
        hits.iter().map(|h| h.repo_id.clone()).collect();
    assert!(
        distinct_repos.len() >= 2,
        "expected search_org('verify') to hit >=2 repos, got {distinct_repos:?}"
    );
    assert!(distinct_repos.contains("shared"));
    assert!(distinct_repos.contains("db-client"));
}
```

(Replace the brittle `err.to_string().contains(...)` pattern with `format!("{err:?}")` and `msg.contains(...)` so the test doesn't depend on the exact `Display` formatting of `LainError::AmbiguousSymbol`.)

- [ ] **Step 2: Run the 3 tests; verify they pass**

Run: `cargo test --features test-utils --test federation_cross_repo_e2e -- --nocapture --test-threads=1`
Expected: 3 passed.

- [ ] **Step 3: Commit the fixture file with 3 tests**

```bash
git add tests/federation_cross_repo_e2e.rs
git commit -m "test(federation): add D fixture with 3 resolver + search_org tests"
```

---

### Task 9: Add the remaining 5 tests to the D fixture

**Files:**
- Modify: `tests/federation_cross_repo_e2e.rs`

**Interfaces:**
- Consumes: existing imports + `build_federation()` helper
- Produces: 5 more tests covering the cross-repo `Calls` edge existence, blast radius (intra-repo, cross-repo, ambiguous, not found)

- [ ] **Step 1: Append the cross-repo `Calls` edge existence test**

```rust
#[tokio::test]
async fn cross_repo_calls_edge_resolves_to_global_node() {
    let (_tmp, fed) = build_federation().await;
    let auth_global = "auth-svc:Function:src/lib.rs:auth".to_string();
    let hash_global = "shared:Function:src/lib.rs:hash".to_string();
    let path = fed
        .backend()
        .find_path(&auth_global, &hash_global)
        .expect("find_path should succeed");
    assert!(
        !path.is_empty(),
        "expected non-empty path from auth-svc::auth to shared::hash; \
         Pass B may not be working — engine change regression?"
    );
}
```

- [ ] **Step 2: Append the intra-repo blast radius test**

```rust
#[tokio::test]
async fn cross_repo_blast_radius_within_owning_repo() {
    use lain::mcp::federation_tools::get_cross_repo_blast_radius;
    let (_tmp, fed) = build_federation().await;
    let result = get_cross_repo_blast_radius(&fed, "hash", 1..3)
        .expect("blast radius on unique-owner symbol should succeed");
    let shared_bucket = result
        .by_repo
        .get("shared")
        .map(|v| v.len())
        .unwrap_or(0);
    assert!(
        shared_bucket >= 1,
        "expected shared bucket >= 1 node (hash -> inner_hash), got {result:?}"
    );
    assert!(!result.truncated);
}
```

- [ ] **Step 3: Append the cross-repo blast radius test**

```rust
#[tokio::test]
async fn cross_repo_blast_radius_walks_into_other_repos() {
    use lain::mcp::federation_tools::get_cross_repo_blast_radius;
    let (_tmp, fed) = build_federation().await;
    let result = get_cross_repo_blast_radius(&fed, "auth", 1..3)
        .expect("blast radius on unique-owner symbol should succeed");
    let shared_bucket = result
        .by_repo
        .get("shared")
        .map(|v| v.len())
        .unwrap_or(0);
    assert!(
        shared_bucket >= 1,
        "expected shared bucket >= 1 (auth -> shared::hash -> inner_hash), got {result:?}"
    );
    assert!(!result.truncated);
}
```

- [ ] **Step 4: Append the ambiguous blast radius test**

```rust
#[tokio::test]
async fn cross_repo_blast_radius_ambiguous_for_tool() {
    use lain::mcp::federation_tools::get_cross_repo_blast_radius;
    let (_tmp, fed) = build_federation().await;
    let result = get_cross_repo_blast_radius(&fed, "verify_token", 1..3);
    let err = result.expect_err("verify_token is ambiguous; should error");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("Ambiguous") || msg.contains("ambiguous"),
        "expected AmbiguousSymbol error, got: {msg}"
    );
}
```

- [ ] **Step 5: Append the not-found blast radius test**

```rust
#[tokio::test]
async fn cross_repo_blast_radius_not_found() {
    use lain::mcp::federation_tools::get_cross_repo_blast_radius;
    let (_tmp, fed) = build_federation().await;
    let result = get_cross_repo_blast_radius(&fed, "does_not_exist", 1..3);
    let err = result.expect_err("unknown symbol should error");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("does_not_exist"),
        "expected error mentioning 'does_not_exist', got: {msg}"
    );
}
```

- [ ] **Step 6: Run all 8 tests; verify they pass**

Run: `cargo test --features test-utils --test federation_cross_repo_e2e -- --nocapture --test-threads=1`
Expected: 8 passed.

- [ ] **Step 7: Commit**

```bash
git add tests/federation_cross_repo_e2e.rs
git commit -m "test(federation): add 5 blast-radius + cross-repo Calls tests to D fixture"
```

---

### Task 10: Extend the e2e shell script with OTel Demo

**Files:**
- Modify: `tests/e2e/federation_e2e.sh` (add OTel Demo clone + 12 workspace_dir entries + 4 new assertion blocks)

**Interfaces:**
- Consumes: existing script structure (lines 1–125, see `tests/e2e/federation_e2e.sh`)
- Produces: extended script that clones OTel Demo, indexes 12 services, asserts cross-repo tool behavior

- [ ] **Step 1: Add the OTel clone and repos.yaml extension**

After the existing heredoc that closes `repos.yaml` (around line 43 in `tests/e2e/federation_e2e.sh`), and before the "Starting lain server" line (around line 45), insert:

```bash
# Clone OpenTelemetry Demo (Astronomy Shop) — 12 polyglot microservices
git clone --depth 1 https://github.com/open-telemetry/opentelemetry-demo.git \
    "${WORKDIR}/opentelemetry-demo" \
    || { echo "ERROR: failed to clone opentelemetry-demo" >&2; exit 1; }

OTEL_DIR="${WORKDIR}/opentelemetry-demo/src"
for svc in adservice cartservice checkoutservice currencyservice \
           emailservice frontend loadgenerator paymentservice \
           productcatalogservice recommendationservice shippingservice \
           accountingservice; do
    if [[ -d "${OTEL_DIR}/${svc}" ]]; then
        cat >> "${WORKDIR}/repos.yaml" <<EOF
  - id: otel-${svc}
    source:
      type: workspace_dir
      path: ${OTEL_DIR}/${svc}
EOF
    else
        echo "WARN: otel-${svc} not present in opentelemetry-demo/src — skipping" >&2
    fi
done
```

- [ ] **Step 2: Replace the existing `total_repos` check with the tolerant one**

Find the existing `total_repos="$(...)"` extraction (around line 105 in the script). Replace the hardcoded `if [[ "${total_repos}" != "3" ]]` check with:

```bash
if [[ "${total_repos}" -lt 12 ]]; then
    echo "ERROR: total_repos=${total_repos}, expected >= 12 (3 famous + >=9 otel)" >&2
    echo "    Payload: ${health_text}" >&2
    exit 1
fi
```

- [ ] **Step 3: Add the ready-threshold wait after the health check**

After the `total_repos` assertion, add:

```bash
# Wait until at least 8 OTel repos are ready (tolerate up to 4 degraded)
for i in $(seq 1 150); do
    health_text="$(call_tool "get_federation_health" '{}' | mcp_text)"
    ready_count="$(printf '%s' "${health_text}" | python3 -c \
      'import json,sys; print(json.load(sys.stdin).get("ready", 0))')"
    [[ "${ready_count}" -ge 8 ]] && break
    sleep 2
done
if [[ "${ready_count}" -lt 8 ]]; then
    echo "ERROR: only ${ready_count} repos ready after 300s, expected >= 8" >&2
    exit 1
fi
echo "    ready_count = ${ready_count}"
```

- [ ] **Step 4: Add the search_org shared-concept assertions**

After the existing `search_org "serialize"` block, add:

```bash
echo "==> Calling search_org for 'Product'..."
product_text="$(call_tool "search_org" '{"query":"Product","limit":20}' | mcp_text)"
product_distinct="$(printf '%s' "${product_text}" | python3 -c \
  'import json,sys; d=json.load(sys.stdin); print(len({h["repo_id"] for h in d}))')"
if [[ "${product_distinct}" -lt 2 ]]; then
    echo "ERROR: search_org('Product') only hit ${product_distinct} repos, expected >= 2" >&2
    echo "    Payload: ${product_text}" >&2
    exit 1
fi
echo "    search_org 'Product': ${product_distinct} distinct repos"

echo "==> Calling search_org for 'Money'..."
money_text="$(call_tool "search_org" '{"query":"Money","limit":20}' | mcp_text)"
money_distinct="$(printf '%s' "${money_text}" | python3 -c \
  'import json,sys; d=json.load(sys.stdin); print(len({h["repo_id"] for h in d}))')"
if [[ "${money_distinct}" -lt 2 ]]; then
    echo "ERROR: search_org('Money') only hit ${money_distinct} repos, expected >= 2" >&2
    echo "    Payload: ${money_text}" >&2
    exit 1
fi
echo "    search_org 'Money': ${money_distinct} distinct repos"
```

- [ ] **Step 5: Add the get_repo_info and get_cross_repo_blast_radius assertions**

After the search_org blocks, add:

```bash
echo "==> Calling get_repo_info for 'otel-productcatalogservice'..."
otel_info="$(call_tool "get_repo_info" '{"id":"otel-productcatalogservice"}' | mcp_text)"
otel_id="$(printf '%s' "${otel_info}" | python3 -c \
  'import json,sys; print(json.load(sys.stdin).get("id",""))')"
if [[ "${otel_id}" != "otel-productcatalogservice" ]]; then
    echo "ERROR: get_repo_info('otel-productcatalogservice') returned id='${otel_id}'" >&2
    echo "    Payload: ${otel_info}" >&2
    exit 1
fi
echo "    get_repo_info 'otel-productcatalogservice': ok"

echo "==> Calling get_cross_repo_blast_radius for 'GetProduct'..."
blast_text="$(call_tool "get_cross_repo_blast_radius" \
  '{"symbol":"GetProduct","depth":"1..3"}' | mcp_text)"
has_by_repo="$(printf '%s' "${blast_text}" | python3 -c \
  'import json,sys; print("by_repo" in json.load(sys.stdin))')"
if [[ "${has_by_repo}" != "True" ]]; then
    echo "ERROR: get_cross_repo_blast_radius('GetProduct','1..3') missing 'by_repo' key" >&2
    echo "    Payload: ${blast_text}" >&2
    exit 1
fi
echo "    get_cross_repo_blast_radius 'GetProduct': ok"
```

- [ ] **Step 6: Run the script; verify it passes end-to-end**

Run: `cargo build --release && tests/e2e/federation_e2e.sh`
Expected: `==> E2E PASSED` printed at the end. Runtime: 5–10 minutes (OTel clone dominates).

- [ ] **Step 7: If the script fails, debug the failing assertion**

Each `echo "ERROR: ..."` line prints the actual MCP response payload. Read the payload to identify what's wrong. Common failure modes:
- `GetProduct` renamed upstream: switch to `PlaceOrder` on `checkoutservice`
- An OTel service's LSP isn't installed: tolerated by `ready >= 8`
- GitHub rate-limit on the OTel clone: retry or pre-clone in CI cache (out of scope)

- [ ] **Step 8: Commit the e2e extensions**

```bash
git add tests/e2e/federation_e2e.sh
git commit -m "test(e2e): add OTel Demo polyglot assertions to federation e2e"
```

---

### Task 11: Update docs/FEDERATION.md

**Files:**
- Modify: `docs/FEDERATION.md`

**Interfaces:**
- Consumes: existing docs structure
- Produces: updated "Smoke test" and "Performance" sections pointing at the new fixtures

- [ ] **Step 1: Update the "Smoke test" section (around line 547 of FEDERATION.md)**

Find the section that ends with the existing smoke-test bash block (the `awk '/^\`\`\`bash$/,/^\`\`\`$/' docs/FEDERATION.md | bash -n` syntax check). Add after the existing block:

```markdown
### Per-PR cross-repo reasoning tests

The federation's semantic contracts (`resolve_symbol`, `AmbiguousSymbol`,
`NotFound`, search across repos, `get_cross_repo_blast_radius` bucketing,
cross-repo `Calls` edge production) are exercised in
`tests/federation_cross_repo_e2e.rs`. This test builds a 3-crate fixture
(`shared`, `db-client`, `auth-svc`) with real `path = "../shared"`
dependencies and asserts the federation produces and traverses cross-repo
`Calls` edges end-to-end. Runs on every PR.

```bash
cargo test --features test-utils --test federation_cross_repo_e2e -- --nocapture --test-threads=1
```
```

- [ ] **Step 2: Update the "Performance" section (around line 326 of FEDERATION.md)**

Find the "Throughput caveats" subsection. Add a new bullet:

```markdown
- Cross-repo `Calls` traversal requires `project_repo` to have produced
  cross-repo edges via the symbol-index resolution pass. This happens
  automatically at federation load time; if `get_cross_repo_blast_radius`
  returns empty for a symbol you expect to have callers, check
  `list_repos` to confirm the repos are `ready`.
```

- [ ] **Step 3: Verify the docs read coherently**

Read both edited sections. Confirm:
- Tone matches the surrounding doc
- Code blocks are syntactically valid bash / markdown
- Links to `tests/federation_cross_repo_e2e.rs` are accurate

- [ ] **Step 4: Commit docs**

```bash
git add docs/FEDERATION.md
git commit -m "docs(federation): point at new D fixture + cross-repo Calls caveat"
```

---

### Task 12: Final verification + tag PR 2 as ready

**Files:**
- No code changes

- [ ] **Step 1: Run the full per-PR test suite**

Run: `cargo test --features test-utils -- --nocapture --test-threads=1`
Expected: all tests pass — `federation_integration` (8 tests), `federation_cross_repo_e2e` (8 tests), `federation_benchmark` (1 test, large ignored).

- [ ] **Step 2: Run the e2e shell script one more time**

Run: `cargo build --release && tests/e2e/federation_e2e.sh`
Expected: `==> E2E PASSED`

- [ ] **Step 3: Confirm all PR 2 commits are in place**

```bash
git log --oneline -15
```

Expected to see, in order:
- `test(federation): add D fixture with 3 resolver + search_org tests`
- `test(federation): add 5 blast-radius + cross-repo Calls tests to D fixture`
- `test(e2e): add OTel Demo polyglot assertions to federation e2e`
- `docs(federation): point at new D fixture + cross-repo Calls caveat`

- [ ] **Step 4: Open PR 2 against `main`**

Title: `test(federation): add cross-repo reasoning fixtures + OTel Demo e2e`. Description references the spec and notes that this is the follow-up to PR 1.

**Stop here. PR 2 is complete.**

---

## Self-Review Notes

After writing this plan against the v3 spec, I checked:

1. **Spec coverage:**
   - Pass A (project per-repo edges): covered in Tasks 2, 6
   - Pass B (cross-repo Calls resolution): covered in Tasks 4, 5
   - D fixture with 8 tests: covered in Tasks 8, 9
   - A fixture with OTel Demo polyglot assertions: covered in Task 10
   - Docs updates: covered in Task 11
   - Backward compatibility (existing tests pass): covered in Task 6
   - Two-PR migration: covered by the explicit `PR 1` / `PR 2` task grouping
   - All 9 spec Definition-of-Done items covered

2. **Placeholder scan:** No TBD / TODO / "fill in" / "appropriate" placeholders. The one caveat (Pass B's `path: ""` placeholder for the target's path) is explicitly flagged with a debug step (Task 5 Step 3) to resolve if the test fails.

3. **Type consistency:**
   - `LainError::NotFound(String)` and `LainError::AmbiguousSymbol(Vec<RepoId>)` match `src/error.rs:39, 65`
   - `GraphBackend::find_path(&self, from: &str, to: &str) -> Result<Vec<GraphNode>, LainError>` matches `src/federation/graph_backend.rs:27`
   - `search_org` and `get_cross_repo_blast_radius` function signatures referenced as in `src/mcp/federation_tools.rs`
   - `RepoIndex::nodes()` and `RepoIndex::edges()` return `Vec<GraphNode>` and `Vec<GraphEdge>` per `src/federation/repo_index.rs:102, 106`
   - `GlobalId::new(repo, kind, path, name)` signature matches `src/federation/repo_id.rs`

4. **Risk areas flagged for the implementer:**
   - Task 4 Step 2: the `external_calls()` accessor assumes the per-repo edge target's id encodes the target's name; this may be wrong if per-repo `GraphDatabase` uses UUID v5 ids. Implementer must verify.
   - Task 5 Step 1: Pass B uses `path: ""` for the global target id; if this breaks `find_path` matching, the implementer must look up the target's actual path.