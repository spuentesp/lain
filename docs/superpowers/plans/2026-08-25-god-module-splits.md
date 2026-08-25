# God-Module Splits Implementation Plan

> **For agentic workers:** REQUIRED SUB-KILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split three god modules into focused files with no behavior changes — `src/server/graph.rs` (1,768 LoC) → `graph/{mod,co_change,anchors,depth}.rs`; `src/server/ingest/ingestion.rs` (693 LoC) → `ingest/{pipeline,single_workspace,federation}.rs`; `src/cli/hooks.rs` (946 LoC) → `cli/hooks/{mod,session,filesystem_lock,git_ref}.rs`. Delete six pieces of dead code surfaced during the split.

**Architecture:** Pure file-organization work — every public function keeps its current name, signature, semantics, and visibility. The split is gated by visibility changes (some helper methods on `GraphDatabase` consumed across the new sub-modules become `pub(crate)` so `graph/co_change.rs` can call them through the database). No call sites outside the affected modules change. No new public API on `GraphDatabase`, `LainServer`, `HookSession`, or anything else.

**Tech Stack:** Rust 1.75+, `petgraph::StableGraph`, `bincode`, `serde`, `clap`, `reqwest::blocking`, `anyhow`. No new dependencies. Module system = standard `pub mod foo;` in a directory module (`mod.rs`).

**Source spec:** `docs/superpowers/reviews/2026-08-25-solid-dry-simplification.md` § P0-7, P0-9, P1-9 (and the dead-code subset of P0-7 / P1-10).

**Note on Plan 2 overlap:** `cli::hooks::McpRequest` / `McpResponse` / `post_mcp` (P1-7) and `cli::hooks::server_reachable` URL parsing (P1-23) are moved to `src/cli/mcp_client.rs` by plan `2026-08-25-cli-dedup.md`. This plan does **not** duplicate that work — it only restructures `hooks.rs` into sub-modules and leaves a `// See plan 2026-08-25-cli-dedup.md` comment where those pieces used to live.

## Global Constraints

- **No behavior changes.** Every move preserves the existing function body byte-for-byte where feasible; when edits are required (e.g. to add `pub(crate)` or to remove a now-orphan `use` statement), they are pure visibility / hygiene.
- **Preserve every existing public API.** `GraphDatabase`, `LainServer`, `HookSession`, `HooksAction` and every other public type keep their full public surface after the split. The diff against `cargo doc --no-deps` is empty.
- **Match repo test conventions.** Unit tests live in `#[cfg(test)] mod tests` blocks at the bottom of source files; integration tests live in `tests/`. After every file move, the same test names run from the same scope (e.g. `cargo test --lib server::graph::` must pass before and after each graph split task).
- **Frequent commits.** Each task ends with a single `git commit`. Commit messages follow the imperative-mood, period-free style used by the rest of the repo (e.g. `Extract graph::anchors into its own module`).
- **No `git push` and no PR creation** unless the user explicitly asks.
- **Bite-sized tasks.** Each sub-module move is its own task. Each task ends with `cargo check` + `cargo test` of the relevant module passing.

---

## File Structure

| Path | Change | Responsibility |
|---|---|---|
| `src/server/graph.rs` | **Delete** (replaced by `graph/` directory) | — |
| `src/server/graph/mod.rs` | **Create** (~1,100 LoC) | `GraphDatabase` core: `new`, `open_read_only`, insert/replace/freshness/prune, queries (`get_node`, `all_nodes`, `find_*`, `subgraph_around`, `traverse`, `get_neighbors`), metadata (`get_last_commit`, `set_last_commit`, `get_stats`, `edge_counts_by_type`, `get_node_at_location`, `has_references_from`), persistence (`save_to_disk`, `save_to_disk_sync`, `load_from_disk`, `export_to_json`), `PATH_FORMAT_VERSION`, `graph_path`, `Freshness` enum, unit tests |
| `src/server/graph/co_change.rs` | **Create** (~50 LoC) | Co-change analysis: `insert_co_change_edges`, `get_co_change_partners` + their tests |
| `src/server/graph/anchors.rs` | **Create** (~170 LoC) | Anchor scoring: `calculate_anchor_scores`, `find_anchors` + their tests |
| `src/server/graph/depth.rs` | **Create** (~90 LoC) | Depth-from-main BFS: `calculate_depths`, `find_entry_points`, `bfs_from` + their tests |
| `src/server/ingest/ingestion.rs` | **Delete** (replaced by 3 new files) | — |
| `src/server/ingest/mod.rs` | **Modify** | Replace `pub mod ingestion;` with three `pub mod` lines |
| `src/server/ingest/pipeline.rs` | **Create** (~80 LoC) | Shared phases: `insert_edges_reporting`, `insert_edges_best_effort`, `sweep_orphans`, plus a new `PipelineLimits` enum (`SingleWorkspace`, `Federation`) |
| `src/server/ingest/single_workspace.rs` | **Create** (~340 LoC) | `LainServer::build_core_memory` + its helpers |
| `src/server/ingest/federation.rs` | **Create** (~210 LoC) | `pub async fn index_one_repo` + its helpers |
| `src/cli/hooks.rs` | **Delete** (replaced by `hooks/` directory) | — |
| `src/cli/hooks/mod.rs` | **Create** (~140 LoC) | Clap `HooksAction` enum, `pub fn claim`, `pub fn release`, `pub fn overlap_check`, plus re-exports of `lock` / `unlock` from `filesystem_lock` |
| `src/cli/hooks/session.rs` | **Create** (~60 LoC) | `HookSession` struct, `sanitize_agent_name`, `session_path`, `read_session`, `write_session` |
| `src/cli/hooks/filesystem_lock.rs` | **Create** (~150 LoC) | `claim_filesystem`, `release_filesystem`, `find_workspace_root`, `pub fn lock`, `pub fn unlock` |
| `src/cli/hooks/git_ref.rs` | **Create** (~30 LoC) | `git_rev_parse_full` |

---

## Task 1: Convert `src/server/graph.rs` into `src/server/graph/mod.rs`

**Files:**
- Modify: `src/server/graph.rs` (rename to `src/server/graph/mod.rs` via `git mv`)
- `src/server/mod.rs` already declares `pub mod graph;` — no change needed

**Interfaces:**
- Consumes: existing public API of `GraphDatabase` (no change)
- Produces: same public API, now exported from `crate::server::graph::GraphDatabase`

### Step 1: Verify the baseline

```bash
cd /home/sebastian/lain
cargo check
cargo test --lib server::graph:: -- --nocapture 2>&1 | tail -20
```

Expected: clean build, all tests pass. This is the baseline; the same tests must pass after every task in this plan.

### Step 2: Move the file via `git mv`

```bash
cd /home/sebastian/lain
mkdir -p src/server/graph
git mv src/server/graph.rs src/server/graph/mod.rs
```

The `pub mod graph;` line in `src/server/mod.rs` keeps working unchanged — Rust treats `graph.rs` and `graph/mod.rs` as equivalent.

### Step 3: Verify the rename is a no-op

```bash
cd /home/sebastian/lain
cargo check
cargo test --lib server::graph:: -- --nocapture 2>&1 | tail -20
```

Expected: identical build output, identical test results.

### Step 4: Commit

```bash
git add src/server/graph.rs src/server/graph/mod.rs src/server/mod.rs
git commit -m "Rename graph.rs to graph/mod.rs in preparation for split"
```

---

## Task 2: Extract `graph/co_change.rs`

**Files:**
- Create: `src/server/graph/co_change.rs` (lines 1163-1210 of original `graph.rs`)
- Modify: `src/server/graph/mod.rs` — delete the moved functions, add `pub mod co_change;`

**Interfaces:**
- Consumes: existing public API of `GraphDatabase` (no change; co-change methods remain on `GraphDatabase`, just implemented in a sibling module)
- Produces: the same `pub fn insert_co_change_edges` and `pub fn get_co_change_partners` on `GraphDatabase`, now defined in `crate::server::graph::co_change`

### Step 1: Add the failing cross-module visibility test

Append to `src/server/graph/mod.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn co_change_module_is_reachable_from_graph_crate() {
    use crate::server::graph::co_change;
    let _phantom: fn(&crate::server::graph::GraphDatabase, &str)
        -> Result<Vec<(String, usize)>, crate::error::LainError>
        = GraphDatabase::get_co_change_partners;
    let _ = co_change;
}
```

### Step 2: Run test, verify it fails

```bash
cargo test --lib server::graph::tests::co_change_module_is_reachable -- --nocapture
```

Expected: `error[E0583]: file not found for module 'co_change'`.

### Step 3: Create `src/server/graph/co_change.rs` with bodies copied verbatim from `graph.rs:1163-1210`

```rust
//! Co-change analysis: which files tend to change together in the
//! repository's commit history. Implemented as methods on `GraphDatabase`
//! so callers continue to invoke `db.insert_co_change_edges(...)`.

use crate::error::LainError;
use crate::schema::GraphEdge;
use crate::server::graph::GraphDatabase;

impl GraphDatabase {
    /// Insert edges that record "these files changed together N times".
    pub fn insert_co_change_edges(
        &self,
        pairs: &[(String, String, usize)],
    ) -> Result<(), LainError> {
        // Body copied verbatim from graph.rs:1163-1178.
        ...
    }

    /// Return the files that have co-change edges pointing at `file_path`,
    /// sorted by co-change frequency descending.
    pub fn get_co_change_partners(
        &self,
        file_path: &str,
    ) -> Result<Vec<(String, usize)>, LainError> {
        // Body copied verbatim from graph.rs:1180-1210.
        ...
    }
}

#[cfg(test)]
mod tests {
    // Move the test bodies from graph.rs that exercise these two
    // functions verbatim.
    ...
}
```

### Step 4: Update `src/server/graph/mod.rs`

Add `pub mod co_change;`. Delete the old `insert_co_change_edges` / `get_co_change_partners` impl blocks and any tests local to them.

### Step 5: Verify

```bash
cargo check
cargo test --lib server::graph:: -- --nocapture 2>&1 | tail -20
```

Expected: clean build; same test count as before.

### Step 6: Commit

```bash
git add src/server/graph/co_change.rs src/server/graph/mod.rs
git commit -m "Extract graph::co_change into its own module"
```

---

## Task 3: Extract `graph/anchors.rs`

**Files:**
- Create: `src/server/graph/anchors.rs` (lines 940-1108 of original `graph.rs` — `calculate_anchor_scores` + `find_anchors` + their unit tests at 1600-1767)

**Interfaces:**
- Consumes: `pub(crate)` access to `GraphDatabase::all_nodes` and `GraphDatabase::all_edges` (already `pub`)
- Produces: the same `pub fn calculate_anchor_scores` and `pub fn find_anchors` on `GraphDatabase`, now defined in `crate::server::graph::anchors`

### Step 1: Add the failing test

Append to `src/server/graph/mod.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn anchors_module_is_reachable_from_graph_crate() {
    use crate::server::graph::anchors;
    let _phantom: fn(&crate::server::graph::GraphDatabase, usize)
        -> Result<Vec<crate::schema::GraphNode>, crate::error::LainError>
        = GraphDatabase::find_anchors;
    let _ = anchors;
}
```

### Step 2: Run test, verify failure

```bash
cargo test --lib server::graph::tests::anchors_module_is_reachable -- --nocapture
```

Expected: `error[E0583]: file not found for module 'anchors'`.

### Step 3: Create `src/server/graph/anchors.rs` with bodies copied verbatim from `graph.rs:940-1108`

```rust
//! Anchor scoring: rank files in the graph by "how central are they to
//! the codebase?" — out-degree × PageRank-style dampening, plus a small
//! penalty for leaf utilities and test code.

use crate::error::LainError;
use crate::schema::GraphNode;
use crate::server::graph::is_test_path;
use crate::server::graph::GraphDatabase;

impl GraphDatabase {
    /// Recompute and cache anchor scores for every node in the graph.
    pub fn calculate_anchor_scores(&self) -> Result<(), LainError> {
        // Body copied verbatim from graph.rs:940-1083.
        ...
    }

    /// Return the top-`limit` nodes by anchor score, descending.
    pub fn find_anchors(&self, limit: usize) -> Result<Vec<GraphNode>, LainError> {
        // Body copied verbatim from graph.rs:1085-1108.
        ...
    }
}

#[cfg(test)]
mod tests {
    // Move the six test bodies from graph.rs:1600-1767 here verbatim.
    ...
}
```

### Step 4: Update `src/server/graph/mod.rs`

Add `pub mod anchors;`. Delete `calculate_anchor_scores`, `find_anchors`, and the six anchor-related test functions.

### Step 5: Verify

```bash
cargo check
cargo test --lib server::graph:: -- --nocapture 2>&1 | tail -20
```

Expected: clean build; same test count as before.

### Step 6: Commit

```bash
git add src/server/graph/anchors.rs src/server/graph/mod.rs
git commit -m "Extract graph::anchors into its own module"
```

---

## Task 4: Extract `graph/depth.rs`

**Files:**
- Create: `src/server/graph/depth.rs` (lines 882-918 (`bfs_from`), 1111-1161 (`calculate_depths`, `find_entry_points`))

**Interfaces:**
- Consumes: existing public API of `GraphDatabase` (`all_nodes`, `get_node`)
- Produces: the same `pub fn bfs_from`, `pub fn calculate_depths`, `pub fn find_entry_points` on `GraphDatabase`, now defined in `crate::server::graph::depth`

### Step 1: Add the failing test

Append to `src/server/graph/mod.rs`:

```rust
#[test]
fn depth_module_is_reachable_from_graph_crate() {
    use crate::server::graph::depth;
    let _phantom: fn(&crate::server::graph::GraphDatabase, &str, u32) -> Vec<String>
        = GraphDatabase::bfs_from;
    let _ = depth;
}
```

### Step 2: Run test, verify failure

```bash
cargo test --lib server::graph::tests::depth_module_is_reachable -- --nocapture
```

Expected: `error[E0583]: file not found for module 'depth'`.

### Step 3: Create `src/server/graph/depth.rs` with bodies copied verbatim from `graph.rs:882-918, 1111-1161`

```rust
//! Depth-from-main BFS: assign each node a depth relative to the entry
//! points (typically `main`, `lib`, top-level bin targets).
//!
//! `bfs_from` is the BFS primitive both `calculate_depths` and
//! `find_entry_points` build on.

use crate::error::LainError;
use crate::schema::GraphNode;
use crate::server::graph::GraphDatabase;
use std::collections::{HashSet, VecDeque};

impl GraphDatabase {
    /// BFS from `start`, returning every node id reachable within
    /// `max_depth` edges (0 means just the start node).
    pub fn bfs_from(&self, start: &str, max_depth: u32) -> Vec<String> {
        // Body copied verbatim from graph.rs:882-918.
        ...
    }

    /// Compute and cache a depth-from-main number for every node in the
    /// graph.
    pub fn calculate_depths(&self) -> Result<(), LainError> {
        // Body copied verbatim from graph.rs:1111-1153.
        ...
    }

    /// Return the entry points — nodes with the smallest depth value
    /// (typically `main`, `lib`, top-level bins).
    pub fn find_entry_points(&self) -> Result<Vec<GraphNode>, LainError> {
        // Body copied verbatim from graph.rs:1155-1161.
        ...
    }
}

#[cfg(test)]
mod tests {
    // Move the depth-related tests from graph.rs verbatim.
    ...
}
```

### Step 4: Update `src/server/graph/mod.rs`

Add `pub mod depth;`. Delete `bfs_from`, `calculate_depths`, `find_entry_points` (and any depth-related tests).

### Step 5: Verify

```bash
cargo check
cargo test --lib server::graph:: -- --nocapture 2>&1 | tail -20
```

Expected: clean build; same test count as before.

### Step 6: Commit

```bash
git add src/server/graph/depth.rs src/server/graph/mod.rs
git commit -m "Extract graph::depth into its own module"
```

---

## Task 5: Create `src/server/ingest/pipeline.rs` (shared phases)

**Files:**
- Create: `src/server/ingest/pipeline.rs` (the helpers `insert_edges_reporting`, `insert_edges_best_effort`, `sweep_orphans` from `ingestion.rs:436-484`, plus a new `PipelineLimits` enum)
- Modify: `src/server/ingest/mod.rs` — add `pub mod pipeline;`

**Interfaces:**
- Consumes: `crate::schema::GraphEdge`, `crate::git::GitSensor`, `crate::graph::GraphDatabase`
- Produces:
  ```rust
  /// Configures whether a pipeline run uses the single-workspace or
  /// federation batch sizes / orphan-sweep gating. Today the two
  /// callers diverge on these knobs; the enum makes the divergence
  /// explicit at the call site.
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum PipelineLimits {
      SingleWorkspace,
      Federation,
  }

  pub fn insert_edges_reporting(path: &Path, db: &GraphDatabase, edges: &[GraphEdge], label: &str);
  pub fn insert_edges_best_effort(db: &GraphDatabase, edges: &[GraphEdge], label: &str);
  pub fn sweep_orphans(path: &Path, db: &GraphDatabase, git: &GitSensor);
  ```

### Step 1: Add the failing cross-module test

Append to `src/server/ingest/mod.rs` (create `#[cfg(test)] mod tests` if not present):

```rust
#[cfg(test)]
mod pipeline_smoke {
    use crate::server::ingest::pipeline::PipelineLimits;

    #[test]
    fn pipeline_limits_distinguishes_workspace_and_federation() {
        assert_ne!(PipelineLimits::SingleWorkspace, PipelineLimits::Federation);
    }
}
```

### Step 2: Run test, verify failure

```bash
cargo test --lib server::ingest::pipeline_smoke -- --nocapture
```

Expected: `error[E0583]: file not found for module 'pipeline'`.

### Step 3: Create `src/server/ingest/pipeline.rs` with bodies copied verbatim from `ingestion.rs:436-484`

```rust
//! Shared phases of the ingest pipeline: edge insertion (with reporting
//! and best-effort variants) and orphan-sweep. Both `single_workspace`
//! and `federation` ingest paths call into this module so the
//! edge-drop and orphan-sweep behavior stays identical across them.

use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::schema::GraphEdge;
use std::path::Path;
use tracing::{debug, warn};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipelineLimits {
    SingleWorkspace,
    Federation,
}

/// Insert `edges`, logging a `warn!` per dropped edge on failure.
/// Used by the single-workspace path where every edge drop is observable
/// by the user.
pub fn insert_edges_reporting(path: &Path, db: &GraphDatabase, edges: &[GraphEdge], label: &str) {
    // Body copied verbatim from ingestion.rs:436-454.
    ...
}

/// Insert `edges`, swallowing errors. Used by the federation path where
/// a single failed batch must not abort the whole repo re-index.
pub fn insert_edges_best_effort(db: &GraphDatabase, edges: &[GraphEdge], label: &str) {
    // Body copied verbatim from ingestion.rs:456-460.
    ...
}

/// Sweep nodes that are no longer in the working tree.
pub fn sweep_orphans(path: &Path, db: &GraphDatabase, git: &GitSensor) {
    // Body copied verbatim from ingestion.rs:462-484.
    ...
}
```

### Step 4: Update `src/server/ingest/mod.rs`

Add `pub mod pipeline;` alongside `pub mod ingestion;`. (Tasks 6 and 7 delete `ingestion.rs` — for now both modules coexist.)

### Step 5: Verify

```bash
cargo check
cargo test --lib server::ingest::pipeline -- --nocapture
```

Expected: clean build; `pipeline_smoke` passes.

### Step 6: Commit

```bash
git add src/server/ingest/pipeline.rs src/server/ingest/mod.rs
git commit -m "Extract ingest::pipeline shared phases (edge insert + orphan sweep)"
```

---

## Task 6: Extract `ingest/single_workspace.rs` (build_core_memory)

**Files:**
- Create: `src/server/ingest/single_workspace.rs` (lines 20-361 of original `ingestion.rs` — `LainServer::build_core_memory` + private helpers)
- Modify: `src/server/ingest/mod.rs` — add `pub mod single_workspace;`
- Modify: `src/server/ingest/ingestion.rs` — delete the `build_core_memory` impl block

**Interfaces:**
- Consumes: `crate::server::ingest::pipeline::{insert_edges_reporting, insert_edges_best_effort, sweep_orphans, PipelineLimits}`
- Produces: the same `pub async fn build_core_memory(&self)` on `LainServer`, now defined in `crate::server::ingest::single_workspace`

### Step 1: Add the failing test that the new module re-exports the function

Append to `src/server/ingest/mod.rs`:

```rust
#[cfg(test)]
mod single_workspace_smoke {
    use crate::server::ingest::single_workspace;
    fn _path() {
        // The function exists at the path; confirm via module import.
        let _ = single_workspace;
    }
}
```

### Step 2: Run test, verify failure

```bash
cargo test --lib server::ingest::single_workspace_smoke -- --nocapture
```

Expected: `error[E0583]: file not found for module 'single_workspace'`.

### Step 3: Create `src/server/ingest/single_workspace.rs` with body copied verbatim from `ingestion.rs:20-361`

```rust
//! Single-workspace ingest path: `LainServer::build_core_memory`.
//!
//! This is the pre-federation entry point. It runs the same five
//! stages as `federation::index_one_repo` but with different batch
//! sizes (driven by `PipelineLimits::SingleWorkspace`) and a different
//! persistence gate.

use crate::error::LainError;
use crate::server::ingest::pipeline::{
    insert_edges_best_effort, insert_edges_reporting, sweep_orphans,
};
use crate::server::LainServer;

impl LainServer {
    /// The "Sane" Ingestion Pipeline: Map -> Reduce -> Resolve -> Enrich.
    pub async fn build_core_memory(&self) -> Result<(), LainError> {
        // Body copied verbatim from ingestion.rs:20-361.
        // Replace inline calls to insert_edges_reporting /
        // insert_edges_best_effort / sweep_orphans with the
        // crate::server::ingest::pipeline:: paths (same functions,
        // resolved through the new module — no behavior change).
        ...
    }
}
```

### Step 4: Update `src/server/ingest/mod.rs` and delete the impl block from `ingestion.rs`

Add `pub mod single_workspace;`. Delete the `build_core_memory` impl block from `ingestion.rs` (Task 7 will do the same for `index_one_repo`, after which `ingestion.rs` is empty and gets deleted).

### Step 5: Verify

```bash
cargo check
cargo test --lib server:: -- --nocapture 2>&1 | tail -30
```

Expected: clean build; the existing `ingestion.rs` integration tests still pass because `build_core_memory` is still callable via `LainServer::build_core_memory` (the method is now defined in `single_workspace.rs`, but Rust resolves it identically).

### Step 6: Commit

```bash
git add src/server/ingest/single_workspace.rs src/server/ingest/ingestion.rs src/server/ingest/mod.rs
git commit -m "Extract build_core_memory into ingest::single_workspace"
```

---

## Task 7: Extract `ingest/federation.rs` (index_one_repo) + delete `ingestion.rs`

**Files:**
- Create: `src/server/ingest/federation.rs` (lines 486-693 of original `ingestion.rs` — `index_one_repo` + helpers)
- Modify: `src/server/ingest/mod.rs` — add `pub mod federation;`, delete `pub mod ingestion;`
- Delete: `src/server/ingest/ingestion.rs` (now empty)

**Interfaces:**
- Consumes: `crate::server::ingest::pipeline::*`
- Produces: the same `pub async fn index_one_repo(...)` at module path `crate::server::ingest::federation::index_one_repo`

### Step 1: Add the failing cross-module test

Append to `src/server/ingest/mod.rs`:

```rust
#[cfg(test)]
mod federation_smoke {
    use crate::server::ingest::federation;
    fn _path() {
        let _ = federation::index_one_repo;
    }
}
```

### Step 2: Run test, verify failure

```bash
cargo test --lib server::ingest::federation_smoke -- --nocapture
```

Expected: `error[E0583]: file not found for module 'federation'`.

### Step 3: Create `src/server/ingest/federation.rs` with body copied verbatim from `ingestion.rs:486-693`

```rust
//! Federation ingest path: `index_one_repo`.
//!
//! Called once per repo from the federation orchestration layer. Runs
//! the same five stages as `single_workspace::build_core_memory` but
//! with `PipelineLimits::Federation` — larger batches, no `partial`
//! timeout, and a stricter orphan-sweep gate.

use crate::error::LainError;
use crate::git::GitSensor;
use crate::graph::GraphDatabase;
use crate::lsp::LspPool;
use crate::server::ingest::pipeline::{
    insert_edges_best_effort, insert_edges_reporting, sweep_orphans,
};
use crate::server::overlay::VolatileOverlay;
use std::path::Path;
use std::sync::Arc;

pub async fn index_one_repo(
    repo_path: &Path,
    repo_id: &str,
    db: &GraphDatabase,
    lsp: &LspPool,
    git: &mut GitSensor,
    cache_dir: &Path,
    overlay: &Arc<VolatileOverlay>,
) -> Result<(), LainError> {
    // Body copied verbatim from ingestion.rs:486-693.
    ...
}
```

### Step 4: Delete `src/server/ingest/ingestion.rs` and update `mod.rs`

```bash
git rm src/server/ingest/ingestion.rs
```

In `src/server/ingest/mod.rs`, replace `pub mod ingestion;` with `pub mod federation;`.

### Step 5: Verify

```bash
cargo check
cargo test --lib server:: -- --nocapture 2>&1 | tail -30
cargo test --tests 2>&1 | tail -30
```

Expected: clean build; all existing tests pass (test files in `tests/` call `LainServer::build_core_memory` and `index_one_repo` through fully-qualified paths, which now resolve through `single_workspace` and `federation`).

### Step 6: Run the federation integration tests

```bash
cargo test --test federation_integration 2>&1 | tail -10
cargo test --test federation_blast_radius_regression 2>&1 | tail -10
```

Expected: pass.

### Step 7: Commit

```bash
git add src/server/ingest/federation.rs src/server/ingest/ingestion.rs src/server/ingest/mod.rs
git commit -m "Extract index_one_repo into ingest::federation and remove ingestion.rs"
```

---

## Task 8: Convert `src/cli/hooks.rs` into `src/cli/hooks/mod.rs`

**Files:**
- Modify: `src/cli/hooks.rs` (rename to `src/cli/hooks/mod.rs` via `git mv`)
- `src/cli/mod.rs` already declares `pub mod hooks;` — no change needed

**Interfaces:**
- Consumes: existing public API of `HooksAction`, `pub fn claim`, `pub fn release`, `pub fn overlap_check`, `pub fn lock`, `pub fn unlock`
- Produces: same public API, now exported from `crate::cli::hooks::*` (path unchanged)

This is the mechanical prerequisite for Tasks 9–11 — once `hooks/mod.rs` exists, sibling modules can be declared inside `hooks/`.

### Step 1: Verify the baseline

```bash
cargo check
cargo test --lib cli::hooks:: -- --nocapture 2>&1 | tail -20
```

Expected: clean build, tests pass.

### Step 2: Move the file

```bash
mkdir -p src/cli/hooks
git mv src/cli/hooks.rs src/cli/hooks/mod.rs
```

### Step 3: Verify the rename

```bash
cargo check
cargo test --lib cli::hooks:: -- --nocapture 2>&1 | tail -20
```

Expected: identical build, identical tests.

### Step 4: Add a breadcrumb for the MCP-over-HTTP + URL parsing pieces

The original `hooks.rs:207-314` (`McpRequest` / `McpResponse` / `post_mcp`) and `hooks.rs:516-548` (`server_reachable`'s URL parser) are moved to `src/cli/mcp_client.rs` by plan `2026-08-25-cli-dedup.md`. After that plan lands, this comment becomes redundant; until then, leave a breadcrumb so future readers know why those symbols are missing.

In `src/cli/hooks/mod.rs`, immediately above where the MCP client was defined:

```rust
// The MCP-over-HTTP client (McpRequest, McpResponse, post_mcp) and the
// URL parser used by `server_reachable` were moved to
// `crate::cli::mcp_client` by plan 2026-08-25-cli-dedup.md (P1-7, P1-23).
// After that plan lands, remove this comment.
```

(If Plan 2 has not yet landed, leave the inline code in place — `hooks/mod.rs` continues to compile against the local definitions until Plan 2 deletes them.)

### Step 5: Commit

```bash
git add src/cli/hooks.rs src/cli/hooks/mod.rs src/cli/mod.rs
git commit -m "Rename cli/hooks.rs to cli/hooks/mod.rs in preparation for split"
```

---

## Task 9: Extract `cli/hooks/session.rs`

**Files:**
- Create: `src/cli/hooks/session.rs` (lines 152-205 of original `hooks.rs` — `HookSession` struct, `sanitize_agent_name`, `session_path`, `read_session`, `write_session`)
- Modify: `src/cli/hooks/mod.rs` — add `pub mod session;`, delete the moved functions, add a `use session::*;` line so the rest of `mod.rs` is unaffected

**Interfaces:**
- Consumes: `crate::config::hooks_dir`
- Produces:
  ```rust
  pub struct HookSession {
      pub session_id: String,
      pub agent_name: String,
      pub agent_kind: String,
      pub created_at_unix: u64,
      pub last_heartbeat_unix: u64,
      pub parent_session_id: Option<String>,
  }

  pub fn sanitize_agent_name(name: &str) -> String;
  pub fn read_session(agent_name: &str) -> Option<HookSession>;
  pub fn write_session(agent_name: &str, sess: &HookSession) -> Result<()>;
  ```

### Step 1: Add the failing test

Append to `src/cli/hooks/mod.rs` inside `#[cfg(test)] mod tests`:

```rust
#[test]
fn session_module_is_reachable_from_hooks_crate() {
    use crate::cli::hooks::session;
    let _ = session::sanitize_agent_name;
}
```

### Step 2: Run test, verify failure

```bash
cargo test --lib cli::hooks::tests::session_module_is_reachable -- --nocapture
```

Expected: `error[E0583]: file not found for module 'session'`.

### Step 3: Create `src/cli/hooks/session.rs` with bodies copied verbatim from `hooks.rs:152-205`

```rust
//! Per-agent session file I/O for `lain hooks claim|release`.
//!
//! The session file lives at `~/.config/lain/hooks/<agent_name>.session`
//! and stores the `session_id` (a UUID minted by `register_agent`),
//! agent kind, and timestamps.

use crate::config::hooks_dir;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSession {
    pub session_id: String,
    pub agent_name: String,
    pub agent_kind: String,
    pub created_at_unix: u64,
    pub last_heartbeat_unix: u64,
    pub parent_session_id: Option<String>,
}

/// Sanitize an agent name for use as a filename component: lowercase,
/// non-alphanumeric → `-`, strip leading/trailing `-`.
pub fn sanitize_agent_name(name: &str) -> String { /* copied from hooks.rs:168-185 */ ... }
fn session_path(agent_name: &str) -> PathBuf { /* copied from hooks.rs:187-189 */ ... }
pub fn read_session(agent_name: &str) -> Option<HookSession> { /* copied from hooks.rs:191-196 */ ... }
pub fn write_session(agent_name: &str, sess: &HookSession) -> Result<()> { /* copied from hooks.rs:198-205 */ ... }
```

### Step 4: Update `src/cli/hooks/mod.rs`

Add `pub mod session;`. Delete `HookSession`, `sanitize_agent_name`, `session_path`, `read_session`, `write_session`. Add the re-export so the rest of the file is unaffected:

```rust
mod session;
use session::{read_session, sanitize_agent_name, write_session, HookSession};
```

### Step 5: Verify

```bash
cargo check
cargo test --lib cli::hooks:: -- --nocapture 2>&1 | tail -20
```

Expected: clean build; same test count as before.

### Step 6: Commit

```bash
git add src/cli/hooks/session.rs src/cli/hooks/mod.rs
git commit -m "Extract HookSession into cli/hooks::session"
```

---

## Task 10: Extract `cli/hooks/filesystem_lock.rs`

**Files:**
- Create: `src/cli/hooks/filesystem_lock.rs` (lines 559-648 (`find_workspace_root`, `claim_filesystem`, `release_filesystem`) + 721-797 (`pub fn lock`, `pub fn unlock`) of original `hooks.rs`)
- Modify: `src/cli/hooks/mod.rs` — add `pub mod filesystem_lock;`, delete the moved functions, re-export `lock` / `unlock`

**Interfaces:**
- Consumes: `crate::server::presence_lock`, `crate::cli::hooks::session::{HookSession, sanitize_agent_name, ...}`
- Produces:
  ```rust
  pub fn find_workspace_root(path: &Path) -> PathBuf;
  pub fn claim_filesystem(workspace_root: &str, path: &str, agent_name: &str,
                          intent: &str, symbol: &str) -> Result<()>;
  pub fn release_filesystem(path: &str) -> Result<()>;
  pub fn lock(workspace_root: &str, path: &str, agent_name: &str) -> Result<()>;
  pub fn unlock(workspace_root: &str, path: &str, agent_name: &str) -> Result<()>;
  ```

### Step 1: Add the failing test

Append to `src/cli/hooks/mod.rs`:

```rust
#[test]
fn filesystem_lock_module_is_reachable_from_hooks_crate() {
    use crate::cli::hooks::filesystem_lock;
    let _ = filesystem_lock::lock;
}
```

### Step 2: Run test, verify failure

```bash
cargo test --lib cli::hooks::tests::filesystem_lock_module_is_reachable -- --nocapture
```

Expected: `error[E0583]: file not found for module 'filesystem_lock'`.

### Step 3: Create `src/cli/hooks/filesystem_lock.rs` with bodies copied verbatim

```rust
//! Filesystem presence-lock orchestration for `lain hooks lock|unlock`.

use crate::cli::hooks::session::{read_session, sanitize_agent_name};
use crate::server::presence_lock;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn find_workspace_root(path: &Path) -> PathBuf { /* copied from hooks.rs:559-585 */ ... }
pub fn claim_filesystem(workspace_root: &str, path: &str,
                        agent_name: &str, intent: &str, symbol: &str) -> Result<()> {
    /* copied from hooks.rs:587-638 */
    ...
}
pub fn release_filesystem(path: &str) -> Result<()> { /* copied from hooks.rs:640-653 */ ... }
pub fn lock(workspace_root: &str, path: &str, agent_name: &str) -> Result<()> {
    /* copied from hooks.rs:721-777 */
    ...
}
pub fn unlock(workspace_root: &str, path: &str, agent_name: &str) -> Result<()> {
    /* copied from hooks.rs:779-797 */
    ...
}
```

(Note: the public top-level `pub fn release` stays in `mod.rs` because it does MCP-over-HTTP orchestration; only the filesystem half `release_filesystem` moves.)

### Step 4: Update `src/cli/hooks/mod.rs`

Add `pub mod filesystem_lock;`. Delete `find_workspace_root`, `claim_filesystem`, `release_filesystem`, `pub fn lock`, `pub fn unlock`. Add the re-exports:

```rust
mod filesystem_lock;
pub use filesystem_lock::{lock, unlock};
```

Internal callers in `mod.rs` (`claim_filesystem`, `release_filesystem`, `find_workspace_root`) use `crate::cli::hooks::filesystem_lock::*` directly.

### Step 5: Verify

```bash
cargo check
cargo test --lib cli::hooks:: -- --nocapture 2>&1 | tail -20
cargo test --test presence_lock 2>&1 | tail -10
```

Expected: clean build; `presence_lock` integration test passes (it exercises `lock` / `unlock` end-to-end).

### Step 6: Commit

```bash
git add src/cli/hooks/filesystem_lock.rs src/cli/hooks/mod.rs
git commit -m "Extract filesystem presence-lock glue into cli/hooks::filesystem_lock"
```

---

## Task 11: Extract `cli/hooks/git_ref.rs`

**Files:**
- Create: `src/cli/hooks/git_ref.rs` (lines 655-668 of original `hooks.rs` — `git_rev_parse_full`)
- Modify: `src/cli/hooks/mod.rs` — add `pub mod git_ref;`, delete `git_rev_parse_full`, add `use git_ref::git_rev_parse_full;`

**Interfaces:**
- Consumes: `std::process::Command`
- Produces: `pub fn git_rev_parse_full(ref_str: &str) -> Result<String>;`

### Step 1: Add the failing test

Append to `src/cli/hooks/mod.rs`:

```rust
#[test]
fn git_ref_module_is_reachable_from_hooks_crate() {
    use crate::cli::hooks::git_ref;
    let _ = git_ref::git_rev_parse_full;
}
```

### Step 2: Run test, verify failure

```bash
cargo test --lib cli::hooks::tests::git_ref_module_is_reachable -- --nocapture
```

Expected: `error[E0583]: file not found for module 'git_ref'`.

### Step 3: Create `src/cli/hooks/git_ref.rs` with body copied verbatim from `hooks.rs:655-668`

```rust
//! `git rev-parse` helpers for the hooks CLI.

use anyhow::{Context, Result};
use std::process::Command;

pub fn git_rev_parse_full(ref_str: &str) -> Result<String> {
    /* copied from hooks.rs:655-668 — Command::new("git").args(["rev-parse", ref_str]).output() */
    ...
}
```

### Step 4: Update `src/cli/hooks/mod.rs`

Add `pub mod git_ref;`, delete the local `git_rev_parse_full`, and add `use git_ref::git_rev_parse_full;`. The `use std::process::Command;` line at the top of `mod.rs` can now be deleted because `git_rev_parse_full` was its only consumer.

### Step 5: Verify

```bash
cargo check
cargo test --lib cli::hooks:: -- --nocapture 2>&1 | tail -20
```

Expected: clean build; same test count.

### Step 6: Commit

```bash
git add src/cli/hooks/git_ref.rs src/cli/hooks/mod.rs
git commit -m "Extract git_rev_parse into cli/hooks::git_ref"
```

---

## Task 12: Delete dead code surfaced by the splits

**Files:**
- Modify: `src/server/graph/mod.rs` — delete `find_node_by_name`, `insert_node`, `upsert_nodes_batch(Vec<GraphNode>)`, `get_all_nodes`
- Modify: `src/server/ingest/single_workspace.rs` — delete `sync_volatile_overlay`, `process_change`

**Interfaces:**
- Consumes: existing public API (these symbols are dead, so removing them is non-breaking)
- Produces: a smaller public API surface on `GraphDatabase` and `LainServer`

### Step 1: Verify each function has zero production callers

```bash
cd /home/sebastian/lain
grep -rn "find_node_by_name\|\.insert_node(\|upsert_nodes_batch\|get_all_nodes" src/ --include="*.rs" \
    | grep -v "graph/mod.rs\|graph_tests.rs"
grep -rn "sync_volatile_overlay\|process_change" src/ --include="*.rs" \
    | grep -v "single_workspace.rs"
```

Expected: zero matches. If any match exists outside the listed files, **stop and ask the user** — the plan assumed these are dead but the codebase may have evolved.

### Step 2: Replace any surviving callers with the documented replacement

For `find_node_by_name`: replace with `find_all_nodes_by_name(name).into_iter().next()`. For `get_all_nodes`: replace with `all_nodes()`. For `upsert_nodes_batch(Vec<GraphNode>)`: replace with the `&[GraphNode]` overload. For `insert_node`: replace with `upsert_node`. For `sync_volatile_overlay` / `process_change`: zero callers — delete outright.

### Step 3: Add the regression-marker test

Append to `src/server/graph/mod.rs`:

```rust
#[test]
fn dead_alias_methods_are_gone() {
    use crate::server::graph::GraphDatabase;
    // Negative compile-time assertion — uncomment if any of these
    // methods is re-added by mistake.
    // let _: fn(&GraphDatabase, &str) -> Option<GraphNode> = GraphDatabase::find_node_by_name;
}
```

The test passes trivially today; it serves as a regression marker.

### Step 4: Delete the functions

In `src/server/graph/mod.rs`, remove the following impl blocks (and any unit tests local to them):

- `pub fn find_node_by_name(&self, name: &str) -> Option<GraphNode>` (~line 795)
- `pub fn insert_node(&self, node: &GraphNode) -> Result<(), LainError>` (~line 165)
- `pub fn upsert_nodes_batch(&self, new_nodes: Vec<GraphNode>) -> Result<(), LainError>` (~line 542)
- `pub fn get_all_nodes(&self) -> Vec<GraphNode>` (~line 769)

In `src/server/ingest/single_workspace.rs`, remove:

- `pub async fn sync_volatile_overlay(&mut self) -> Result<(), LainError>` (~old line 363)
- `async fn process_change(&mut self, path: &Path) -> Result<(), LainError>` (~old line 380)

### Step 5: Verify

```bash
cargo check
cargo test --lib -- --nocapture 2>&1 | tail -30
cargo test --tests 2>&1 | tail -30
```

Expected: clean build; all existing tests pass (the deletions only remove dead code; no test currently exercises them).

### Step 6: Verify the full pre-existing test surface

```bash
cargo test --test presence_e2e 2>&1 | tail -10
cargo test --test federation_integration 2>&1 | tail -10
cargo test --test presence_lock 2>&1 | tail -10
```

Expected: all pass.

### Step 7: Commit

```bash
git add src/server/graph/mod.rs src/server/ingest/single_workspace.rs
git commit -m "Delete dead code surfaced by god-module splits"
```

---

## Self-Review (do before handing to user)

After writing this plan, verify:

1. **Spec coverage:** P0-7 → Tasks 1–4. P0-7 dead code (`find_node_by_name`, `insert_node`, `upsert_nodes_batch`, `get_all_nodes`) → Task 12. P0-9 → Tasks 5–7. P1-9 → Tasks 8–11. P1-10 ingestion dead code (`sync_volatile_overlay`, `process_change`) → Task 12. ✅
2. **Placeholder scan:** No "TODO" / "TBD" / "fill in" outside `...` markers in code blocks. Each `...` is paired with a `// Body copied verbatim from <file>:<lines>` citation that names the exact source range — the implementer has a single, unambiguous source of truth for each paste. ✅
3. **Type consistency:** `PipelineLimits` defined in Task 5 is referenced by Tasks 6 (single-workspace) and 7 (federation). `pub mod co_change` declared in Task 2 is used by Tasks 3, 4 via the `GraphDatabase` impl-method pattern. `pub mod session` declared in Task 9 is used by Task 10 (`filesystem_lock` re-imports `read_session` / `sanitize_agent_name`). `pub mod git_ref` declared in Task 11 is a leaf. ✅
4. **Bite-sized steps:** Each task is a single `git mv` + `pub mod` + paste operation, plus the canonical `cargo check` + `cargo test --lib <scope>` + commit. Largest step is Task 6 Step 3 (~340 LoC paste); mitigated by being a single search-and-paste from a known source range. ✅
5. **Repo conventions:** TDD where the move changes module visibility — each split task has a one-line cross-module reachability test that fails before the file is created and passes after. ✅
6. **No behavior changes:** Verified by running the same `cargo test --lib server::graph::` / `server::ingest::` / `cli::hooks::` commands before and after each task and asserting identical pass/fail counts. ✅
7. **Plan 2 non-duplication:** Task 8 explicitly references plan `2026-08-25-cli-dedup.md` for the MCP-over-HTTP client and URL parser moves. ✅

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-25-god-module-splits.md`.

**Estimated total effort:** 12 tasks, ~3–5 working days for one engineer familiar with the codebase. The mechanical moves (Tasks 1, 2, 4, 5, 8, 9, 11) are each <30 minutes; the paste-heavy moves (Tasks 3, 6, 7, 10) are each ~1 hour; the dead-code deletion (Task 12) is ~30 minutes once the `grep` confirms zero callers.

**Risks:**

- **Tasks 3, 4, 6, 7, 10 paste errors.** The verbatim-copy pattern is the right move for "no behavior changes", but it relies on the implementer copying the right line range. Mitigation: each paste step cites the exact source line range (e.g. `graph.rs:940-1108`) and `cargo check` after the paste catches any missing import / closure / type annotation.
- **Task 12 false-positive on "zero callers".** If the codebase has grown a caller for one of the dead aliases since the report was written, the deletion breaks compilation. Mitigation: Step 1's `grep -rn` is the explicit gate — if any match exists outside the listed files, the implementer stops and asks.
- **`pub(crate)` visibility drift.** Methods on `GraphDatabase` that are `pub` today but only consumed inside `graph/` will technically work after the split without any visibility change (the methods stay on the same struct, just defined in a sibling module). Mitigation: no visibility changes are planned; if `cargo check` surfaces an `unused_pub` warning or an `E0603` "function is private" error, the implementer adds `pub(crate)` at the call site.

Two execution options:

**1. Subagent-Driven (recommended)** — Dispatch a fresh subagent per task with this plan in hand; review between tasks. Best for this plan because the file moves are independent (Task 3 doesn't need Task 2 to be merged) and a per-task subagent can hold the full plan + the relevant source range in working memory without context churn.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints. Best if you want to do the review yourself and would rather not orchestrate subagents.

Which approach?
