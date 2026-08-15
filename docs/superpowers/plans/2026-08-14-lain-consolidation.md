# Lain Consolidation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate `lain` so the headline is a local MCP server that indexes repos and workspaces; drop the multi-user coordination surface; keep every analytical feature (NLP, embedders, cross-encoders, sensors, watcher, overlay, build integration, query language); expand the HTML dashboard into a full command center; add hot-reload of `repos.yaml` / `workspaces.yaml`.

**Architecture:** Single `lain` binary. The crate is split into `src/server/` (the MCP server + federation + workspaces + all the analytical engines) and `src/cli/` (thin subcommands for config + query). The MCP server is one of the subcommands (`lain server`). Config types live in `src/config/`. The HTML dashboard is reorganized into a Command Center SPA served at `GET /` with workspace/project switchers, config view, D3 graph, repos table, query runner, MCP tool tester, and live status bar. The server hot-reloads `repos.yaml` / `workspaces.yaml` via a CLI signal on a Unix socket AND a file watcher for hand-edits.

**Tech Stack:** Rust 1.75+, `rust-mcp-sdk` 1.0.1, `hyper` 1.9, `petgraph`, `bincode`, `serde_yaml`, `tree-sitter`, `ort` (ONNX), `tokenizers`, `git2`, `notify`, `clap` 4.4. HTML/JS: vanilla, D3 v7 vendored.

**Reference design:** `docs/superpowers/specs/2026-08-14-lain-consolidation-design.md` (the brainstorming/spec that produced this plan).

---

## Global Constraints

- **Cargo workspace:** `Cargo.toml` at repo root declares `members = [".", "crates/lain-mcp-probe"]`. After PR 2, `crates/lain-mcp-probe` is removed.
- **Single binary:** `lain` is the only binary. The former `lain-server` idea is internalized into `lain server`.
- **Rust edition:** 2021. Floor: 1.75.
- **Path handling:** all paths are absolute after `dunce::canonicalize`; never hand-rolled joining unless `fs::create_dir_all` is in the same call.
- **Atomic YAML writes:** every CLI that writes `repos.yaml` or `workspaces.yaml` must write to a temp file in the same directory, then `rename` (so the file watcher either sees the old contents or the new contents, never partial).
- **Version floor:** `version = "0.5.0"` in `Cargo.toml` is the source of truth. `server.json`, `README.md`, `Formula/lain.rb`, `npm-shim/package.json` follow.
- **YAML library:** `serde_yaml` 0.9 stays. `serde_yaml` is used for both `repos.yaml` and `workspaces.yaml`.
- **Tests:** every task has a failing-test-first step. `cargo test --workspace` must pass at the end of each PR.
- **Commits:** conventional commits (`feat:`, `fix:`, `chore:`, `refactor:`, `test:`, `docs:`). Each task ends with a commit.

---

## File Structure (final)

```
src/
├── main.rs                       # dispatch: server / workspaces / repos / query / ask
├── lib.rs                        # re-exports server, cli, config
├── server/                       # the MCP server (architectural boundary)
│   ├── mod.rs                    # LainServer, transports, ingest pipeline
│   ├── federation/               # moved from src/federation/
│   ├── mcp/                      # moved from src/mcp/
│   │   ├── command_center/       # new SPA: index.html, app.js, styles.css, assets/
│   │   ├── handler.rs
│   │   ├── federation_tools.rs
│   │   └── mod.rs
│   ├── tools/                    # moved from src/tools/
│   ├── graph.rs                  # moved from src/
│   ├── schema.rs                 # moved from src/
│   ├── git.rs                    # moved from src/
│   ├── treesitter.rs             # moved from src/
│   ├── nlp.rs                    # moved from src/
│   ├── toolchains.rs             # moved from src/
│   ├── sensors/                  # moved from src/sensors/
│   ├── watcher.rs                # moved from src/ (extended to watch repos/workspaces.yaml)
│   ├── overlay.rs                # moved from src/ (volatile overlay)
│   ├── overlay/stream.rs         # moved from src/overlay/
│   ├── tuning.rs                 # moved from src/
│   ├── error.rs                  # moved from src/
│   └── reload.rs                 # new: ReloadBus, hot-reload orchestrator
├── cli/                          # thin CLI over the lib
│   ├── mod.rs
│   ├── workspaces.rs
│   ├── repos.rs
│   ├── query.rs
│   ├── ask.rs
│   └── signal.rs                 # new: writes YAML atomically, sends reload to socket
└── config/
    ├── mod.rs
    └── recent_projects.rs        # new: ~/.config/lain/recent_projects tracking
```

---

## PR 1 — Restructure the source tree

### Task 1.1: Create the new directory skeleton

**Files:**
- Create: `src/server/mod.rs` (placeholder)
- Create: `src/server/federation/.gitkeep`
- Create: `src/server/mcp/command_center/.gitkeep`
- Create: `src/server/tools/.gitkeep`
- Create: `src/server/sensors/.gitkeep`
- Create: `src/server/ingest/.gitkeep`
- Create: `src/cli/mod.rs` (placeholder)
- Create: `src/config/mod.rs` (placeholder)

**Interfaces:**
- Consumes: nothing
- Produces: empty modules that `cargo build` will accept once the moves in 1.2–1.7 land.

- [ ] **Step 1: Create the new directories and placeholder files**

```bash
mkdir -p src/server/federation src/server/mcp/command_center src/server/tools src/server/sensors src/server/ingest src/cli src/config
touch src/server/mod.rs src/server/federation/.gitkeep src/server/mcp/command_center/.gitkeep src/server/tools/.gitkeep src/server/sensors/.gitkeep src/server/ingest/.gitkeep src/cli/mod.rs src/config/mod.rs
```

- [ ] **Step 2: Commit**

```bash
git add src/server src/cli src/config
git commit -m "chore: scaffold server/cli/config directory layout"
```

---

### Task 1.2: Move `src/federation/` to `src/server/federation/`

**Files:**
- Move: `src/federation/*.rs` → `src/server/federation/`
- Delete: `src/federation/`
- Modify: `src/lib.rs` (update `pub mod federation` to `pub mod server`; add `pub use server::federation;`)

**Interfaces:**
- Consumes: `pub mod federation` re-export in `src/lib.rs`
- Produces: `pub mod federation` reachable as `lain::server::federation` and `lain::federation` (re-export)

- [ ] **Step 1: Move the files**

```bash
git mv src/federation/config.rs       src/server/federation/config.rs
git mv src/federation/federated_index.rs src/server/federation/federated_index.rs
git mv src/federation/graph_backend.rs   src/server/federation/graph_backend.rs
git mv src/federation/health.rs       src/server/federation/health.rs
git mv src/federation/loader.rs       src/server/federation/loader.rs
git mv src/federation/manifest.rs     src/server/federation/manifest.rs
git mv src/federation/matching.rs     src/server/federation/matching.rs
git mv src/federation/mod.rs          src/server/federation/mod.rs
git mv src/federation/repo_id.rs      src/server/federation/repo_id.rs
git mv src/federation/repo_index.rs   src/server/federation/repo_index.rs
git mv src/federation/repo_source.rs  src/server/federation/repo_source.rs
git mv src/federation/workspace.rs    src/server/federation/workspace.rs
git mv src/federation/federated_index_tests.rs src/server/federation/federated_index_tests.rs
git mv src/federation/graph_backend_tests.rs   src/server/federation/graph_backend_tests.rs
git mv src/federation/loader_tests.rs         src/server/federation/loader_tests.rs
git mv src/federation/manifest_tests.rs       src/server/federation/manifest_tests.rs
git mv src/federation/matching_tests.rs       src/server/federation/matching_tests.rs
git mv src/federation/repo_source_tests.rs    src/server/federation/repo_source_tests.rs
```

- [ ] **Step 2: Update `src/server/federation/mod.rs`**

Remove the `#[path]` overrides for the test files (they live next to the source now). Replace the file content with:

```rust
//! Federation engine — multi-repo coordination.
//!
//! Moved from src/federation/ in PR 1 of the consolidation plan.

pub mod config;
pub mod federated_index;
pub mod graph_backend;
pub mod health;
pub mod loader;
pub mod manifest;
pub mod matching;
pub mod repo_id;
pub mod repo_index;
pub mod repo_source;
pub mod workspace;

#[cfg(test)]
mod federated_index_tests;
#[cfg(test)]
mod graph_backend_tests;
#[cfg(test)]
mod loader_tests;
#[cfg(test)]
mod manifest_tests;
#[cfg(test)]
mod matching_tests;
#[cfg(test)]
mod repo_source_tests;
```

- [ ] **Step 3: Update `src/server/mod.rs`**

Replace the placeholder with:

```rust
//! MCP server — the headline of `lain`.
//!
//! Owns the federation engine, the workspace layer, all analytical tools,
//! the ingest pipeline, the watcher, and the volatile overlay.

pub mod federation;
pub mod mcp;
pub mod tools;

// Core
pub mod graph;
pub mod schema;
pub mod git;
pub mod treesitter;
pub mod tuning;
pub mod error;

// Analytical side
pub mod nlp;
pub mod toolchains;
pub mod sensors;
pub mod watcher;
pub mod overlay;
pub mod reload;

pub mod ingest;
```

- [ ] **Step 4: Update `src/lib.rs` to re-export under the old paths**

```rust
//! Lain — local MCP server for cross-repo and per-repo code analysis.

pub mod server;
pub mod cli;
pub mod config;

pub mod error;
pub mod federation {
    //! Re-export the federation engine from `server::federation`.
    pub use crate::server::federation::*;
}
pub mod graph {
    pub use crate::server::graph::*;
}
pub mod git {
    pub use crate::server::git::*;
}
pub mod lsp {
    pub use crate::server::lsp::*;
}
pub mod schema {
    pub use crate::server::schema::*;
}
pub mod tuning {
    pub use crate::server::tuning::*;
}

pub use server::LainServer;
```

- [ ] **Step 5: Verify `cargo build` compiles**

```bash
cargo build --workspace 2>&1 | head -200
```

Expected: errors only on references to old paths in `src/server/`, `src/cli/`, `src/cmds/`, `src/main.rs`. Tasks 1.3–1.10 fix those.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: move federation/ under server/"
```

---

### Task 1.3: Move `src/mcp/` to `src/server/mcp/` (drop the HTML dashboards temporarily)

**Files:**
- Move: `src/mcp/{handler.rs,federation_tools.rs,mod.rs}` → `src/server/mcp/`
- Delete: `src/mcp/federation_dashboard.html`, `src/mcp/front_end_monitor.html` (PR 4 restores them as the command center)
- Modify: `src/lib.rs` (already references `server::mcp` after Task 1.2)

**Interfaces:**
- Consumes: `pub mod mcp` at `src/server/`
- Produces: `mod.rs`, `handler.rs`, `federation_tools.rs` reachable as `lain::server::mcp::*`

- [ ] **Step 1: Move the files**

```bash
git mv src/mcp/handler.rs         src/server/mcp/handler.rs
git mv src/mcp/federation_tools.rs src/server/mcp/federation_tools.rs
git mv src/mcp/mod.rs              src/server/mcp/mod.rs
rm src/mcp/federation_dashboard.html src/mcp/front_end_monitor.html
rmdir src/mcp
```

- [ ] **Step 2: Update `src/server/mcp/mod.rs` to remove HTML constants**

```rust
//! MCP handler and federation tools.

pub mod federation_tools;
pub mod handler;
```

- [ ] **Step 3: Grep for stragglers referencing the old HTML paths**

```bash
grep -r "federation_dashboard\|front_end_monitor" src/ tests/
```

Expected: no matches. If any are found, edit them out (they're in `mcp/handler.rs` which builds the HTTP response).

- [ ] **Step 4: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -100
```

Expected: only remaining errors are references to `src/mcp` from old code paths (mostly in `src/main.rs` and `src/server/mod.rs`).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: move mcp/ under server/, drop old HTML dashboards"
```

---

### Task 1.4: Move `src/tools/` to `src/server/tools/`

**Files:**
- Move: `src/tools/*.rs` → `src/server/tools/`
- Delete: `src/tools/`

- [ ] **Step 1: Move the files**

```bash
git mv src/tools/architecture.rs        src/server/tools/architecture.rs
git mv src/tools/architecture_tests.rs  src/server/tools/architecture_tests.rs
git mv src/tools/context.rs             src/server/tools/context.rs
git mv src/tools/context_tests.rs       src/server/tools/context_tests.rs
git mv src/tools/cross_runtime.rs       src/server/tools/cross_runtime.rs
git mv src/tools/cross_runtime_tests.rs src/server/tools/cross_runtime_tests.rs
git mv src/tools/definitions.rs         src/server/tools/definitions.rs
git mv src/tools/enrichment.rs          src/server/tools/enrichment.rs
git mv src/tools/enrichment_tests.rs    src/server/tools/enrichment_tests.rs
git mv src/tools/execution.rs           src/server/tools/execution.rs
git mv src/tools/filesystem.rs          src/server/tools/filesystem.rs
git mv src/tools/filesystem_tests.rs    src/server/tools/filesystem_tests.rs
git mv src/tools/gitops.rs              src/server/tools/gitops.rs
git mv src/tools/gitops_tests.rs        src/server/tools/gitops_tests.rs
git mv src/tools/impact.rs              src/server/tools/impact.rs
git mv src/tools/metrics.rs             src/server/tools/metrics.rs
git mv src/tools/metrics_tests.rs       src/server/tools/metrics_tests.rs
git mv src/tools/mod.rs                 src/server/tools/mod.rs
git mv src/tools/navigation.rs          src/server/tools/navigation.rs
git mv src/tools/proptest_helpers.rs    src/server/tools/proptest_helpers.rs
git mv src/tools/query.rs               src/server/tools/query.rs
git mv src/tools/query_tests.rs         src/server/tools/query_tests.rs
git mv src/tools/registry.rs            src/server/tools/registry.rs
git mv src/tools/registry_impl.rs       src/server/tools/registry_impl.rs
git mv src/tools/search.rs              src/server/tools/search.rs
git mv src/tools/testing.rs             src/server/tools/testing.rs
git mv src/tools/testing_tests.rs       src/server/tools/testing_tests.rs
git mv src/tools/utils.rs               src/server/tools/utils.rs
git mv src/tools/utils_tests.rs         src/server/tools/utils_tests.rs
git mv src/tools/handlers               src/server/tools/handlers
rmdir src/tools
```

- [ ] **Step 2: Fix internal imports in `src/server/tools/mod.rs`**

Look for `crate::tools::` and replace with `crate::server::tools::` (or `super::` as appropriate). If the file uses `crate::tools::handlers::`, change to `super::handlers::`.

- [ ] **Step 3: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -50
```

Expected: only remaining errors are in `src/main.rs`, `src/state.rs`, and any stragglers still pointing at `crate::tools` or `crate::mcp`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor: move tools/ under server/"
```

---

### Task 1.5: Move core modules to `src/server/`

**Files:**
- Move: `src/{graph,schema,git,treesitter,nlp,toolchains,tuning,error}.rs` → `src/server/`
- Move: `src/sensors/` → `src/server/sensors/`
- Move: `src/watcher.rs` → `src/server/watcher.rs`
- Move: `src/overlay.rs`, `src/overlay/stream.rs` → `src/server/overlay.rs`, `src/server/overlay/stream.rs`

- [ ] **Step 1: Move the files**

```bash
git mv src/graph.rs          src/server/graph.rs
git mv src/schema.rs         src/server/schema.rs
git mv src/git.rs            src/server/git.rs
git mv src/treesitter.rs     src/server/treesitter.rs
git mv src/nlp.rs            src/server/nlp.rs
git mv src/toolchains.rs     src/server/toolchains.rs
git mv src/tuning.rs         src/server/tuning.rs
git mv src/error.rs          src/server/error.rs
git mv src/sensors           src/server/sensors
git mv src/watcher.rs        src/server/watcher.rs
git mv src/overlay.rs        src/server/overlay.rs
git mv src/overlay           src/server/overlay
git mv src/git_tests.rs      src/server/git_tests.rs
git mv src/schema_tests.rs   src/server/schema_tests.rs
git mv src/tuning_tests.rs   src/server/tuning_tests.rs
git mv src/error_tests.rs    src/server/error_tests.rs
git mv src/graph_tests.rs    src/server/graph_tests.rs
git mv src/overlay_tests.rs  src/server/overlay_tests.rs
```

- [ ] **Step 2: Move `src/query/` to `src/server/query/`**

```bash
mkdir -p src/server/query
git mv src/query/executor.rs src/server/query/executor.rs
git mv src/query/executor_tests.rs src/server/query/executor_tests.rs
git mv src/query/mod.rs      src/server/query/mod.rs
git mv src/query/schema.rs   src/server/query/schema.rs
git mv src/query/spec.rs     src/server/query/spec.rs
git mv src/query/spec_tests.rs src/server/query/spec_tests.rs
rmdir src/query
```

- [ ] **Step 3: Update `src/server/mod.rs` to declare the new modules**

Append inside the `pub mod server;` block:

```rust
pub mod query;
pub mod overlay {
    pub use crate::server::overlay_in::*; // see Task 1.6
}
```

(Or declare `pub mod overlay;` and `pub mod query;` directly — choose whichever yields the cleanest imports.)

- [ ] **Step 4: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -100
```

Expected: only remaining errors are in `src/main.rs`, `src/state.rs`, `src/lib.rs` (the re-exports were done in Task 1.2), and any tests that import from the old paths.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: move core modules under server/"
```

---

### Task 1.6: Move `src/server/` (the existing one) into `src/server/ingest/`

**Files:**
- Move: `src/server/{ingestion,jobs,scan,mod}.rs` → `src/server/ingest/`
- Move: `src/server/` (the existing directory) → keep `src/server/mod.rs` as the top-level orchestrator

**Interfaces:**
- Consumes: `LainServer::new`, `LainServer::with_federation`, `LainServer::with_federation_and_workspaces` (signatures unchanged)
- Produces: `crate::server::ingest::*` reachable from `crate::server::mod.rs`

- [ ] **Step 1: Move the files**

```bash
mkdir -p src/server/ingest
git mv src/server/ingestion.rs src/server/ingest/ingestion.rs
git mv src/server/jobs.rs      src/server/ingest/jobs.rs
git mv src/server/scan.rs      src/server/ingest/scan.rs
git mv src/server/mod.rs       src/server/ingest/mod.rs
```

After this, `src/server/` is empty (just the directories created in 1.1).

- [ ] **Step 2: Restore `src/server/mod.rs` as the top-level orchestrator**

Write `src/server/mod.rs`:

```rust
//! MCP server — the headline of `lain`.
//!
//! Owns the federation engine, the workspace layer, all analytical tools,
//! the ingest pipeline, the watcher, and the volatile overlay.

pub mod federation;
pub mod mcp;
pub mod tools;
pub mod query;

// Core
pub mod graph;
pub mod schema;
pub mod git;
pub mod treesitter;
pub mod tuning;
pub mod error;

// Analytical side
pub mod nlp;
pub mod toolchains;
pub mod sensors;
pub mod watcher;
pub mod overlay;
pub mod reload;

pub mod ingest;

use crate::server::error::LainError;
use crate::server::federation::federated_index::FederatedIndex;
use crate::server::federation::workspace::WorkspacesFile;
use crate::server::graph::GraphDatabase;
use crate::server::nlp::{CrossEncoder, NlpEmbedder};
use crate::server::overlay::{OverlayDiff, VolatileOverlay};
use crate::server::tools::ToolExecutor;
use crate::server::tuning::{load_tuning_config, TuningConfig};
use crate::server::git::GitSensor;
use crate::server::ingest::LainServer as Ingester;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;
use tracing::info;

/// Server configuration
#[derive(Clone)]
pub struct LainConfig {
    pub workspace: PathBuf,
    pub memory_path: PathBuf,
}

/// MCP transport for federation-mode servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http,
}

/// Main Lain server
#[derive(Clone)]
pub struct LainServer {
    pub config: LainConfig,
    pub graph: GraphDatabase,
    pub overlay: VolatileOverlay,
    pub embedder: NlpEmbedder,
    pub cross_encoder: CrossEncoder,
    pub git: Arc<Mutex<GitSensor>>,
    pub tool_executor: ToolExecutor,
    pub tuning: Arc<TuningConfig>,
    overlay_revision: Arc<AtomicU64>,
    federation: Option<Arc<FederatedIndex>>,
    federation_workspaces: Option<Arc<WorkspacesFile>>,
    federation_transport: Option<Transport>,
    federation_port: Option<u16>,
}

impl LainServer {
    pub fn new(workspace: &Path, memory_path: &Path, embedding_model: Option<&Path>) -> Result<Self, LainError> {
        // ... (unchanged from the original src/server/mod.rs body) ...
    }

    pub fn with_federation(federation: Arc<FederatedIndex>, transport: Transport, port: u16) -> Result<Self, LainError> { /* unchanged */ }
    pub fn with_federation_and_workspaces(federation: Arc<FederatedIndex>, transport: Transport, port: u16, workspaces: Arc<WorkspacesFile>) -> Result<Self, LainError> { /* unchanged */ }
    pub fn federation(&self) -> Option<&Arc<FederatedIndex>> { self.federation.as_ref() }
    pub async fn serve(self) -> Result<(), LainError> { /* unchanged */ }
    pub fn clone_for_background(&self) -> Self { self.clone() }
    pub fn next_revision(&self) -> crate::server::overlay::RevisionId { self.overlay_revision.fetch_add(1, Ordering::Relaxed) + 1 }
    pub fn broadcast_overlay_insert(&self, node: crate::server::schema::GraphNode) { /* unchanged */ }
    pub fn is_git_repo(&self) -> bool { self.git.lock().is_valid() }
    pub async fn shutdown(&self) { let _ = self; }
}
```

(Copy the bodies verbatim from `src/server/mod.rs` in the original tree. The signatures and field names are identical; only the path of `crate::server::ingestion.rs` references change to `crate::server::ingest::ingestion` etc.)

- [ ] **Step 3: Fix references in `src/server/ingest/*.rs`**

Search-replace `crate::server::` → `super::` (or `crate::server::ingest::` from outside). Test that `cargo build` no longer references the old `src/server/ingestion.rs` path.

- [ ] **Step 4: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -100
```

Expected: `cargo build` succeeds. `cargo test` exposes the remaining broken tests (mostly in `src/main.rs`).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: split LainServer and ingest/ into separate modules"
```

---

### Task 1.7: Move `src/cmds/` to `src/cli/`

**Files:**
- Move: `src/cmds/*.rs` → `src/cli/`
- Delete: `src/cmds/`

**Interfaces:**
- Consumes: `cmds::workspaces`, `cmds::repos`, etc. from `src/main.rs`
- Produces: `lain::cli::workspaces`, `lain::cli::repos`, etc.

- [ ] **Step 1: Move the files**

```bash
git mv src/cmds/ask.rs        src/cli/ask.rs
git mv src/cmds/hook.rs       src/cli/hook.rs
git mv src/cmds/init.rs       src/cli/init.rs
git mv src/cmds/kimi_plugin_wrapper.sh src/cli/kimi_plugin_wrapper.sh
git mv src/cmds/mod.rs        src/cli/mod.rs
git mv src/cmds/projects.rs   src/cli/projects.rs
git mv src/cmds/query.rs      src/cli/query.rs
git mv src/cmds/server.rs     src/cli/server.rs
git mv src/cmds/workspaces.rs src/cli/workspaces.rs
git mv src/cmds/agents        src/cli/agents
rmdir src/cmds
```

- [ ] **Step 2: Update `src/cli/mod.rs`**

Replace `crate::cmds::` references with `crate::cli::` (or `super::`). The shapes don't change.

- [ ] **Step 3: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -50
```

Expected: only `src/main.rs` still references `crate::cmds`. Errors in tests too.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor: move cmds/ to cli/"
```

---

### Task 1.8: Drop the `Projects` registry from `src/state.rs`

**Files:**
- Modify: `src/state.rs` (remove the `Project`, `RegistryFile`, `projects_file`, `current_file`, `read_registry`, `write_registry`, `active_name`, `resolve_auto_workspace`, `Projects::list/add/forget/current/active_name` API)
- Modify: `src/main.rs` (drop the `use lain::state::Projects;` and the `Projects::*` references; remove `resolve_workspace_path`)

**Interfaces:**
- Consumes: `Projects` re-export from `lain::state`
- Produces: a `state.rs` that holds only the `ActiveWorkspace` type (used by hot-reload) and any other genuine state, not the project registry.

- [ ] **Step 1: Write the failing test**

Create `src/server/state.rs` (or reuse `src/state.rs` if kept at the top level — see decision below):

```rust
use crate::server::error::LainError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `~/.config/lain/active_workspace` — the pointer the operator writes
/// via `lain workspaces use <name>`. Format: single workspace name (legacy
/// one-line) or `<config-path>\n<workspace-name>` (after the multi-project
/// consolidation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveWorkspace {
    pub name: String,
    pub config_path: Option<PathBuf>,
}

impl ActiveWorkspace {
    pub fn load() -> Result<Option<Self>, LainError> {
        // Read from ~/.config/lain/active_workspace.
        // If the file has one line, return ActiveWorkspace { name, config_path: None }.
        // If two lines, parse the first as config_path and the second as name.
        // If missing, return Ok(None).
        // On parse error, return LainError::Config.
        todo!("see Task 1.8 implementation")
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib state::tests::active_workspace_loads_two_line_format 2>&1 | head -20
```

Expected: compile failure, `todo!()` macro doesn't return.

- [ ] **Step 3: Implement `ActiveWorkspace::load`**

```rust
use crate::server::config_dir;

impl ActiveWorkspace {
    pub fn load() -> Result<Option<Self>, LainError> {
        let path = config_dir().join("active_workspace");
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| LainError::Io(e.to_string()))?;
        let mut lines = text.lines();
        let first = match lines.next() {
            Some(s) => s,
            None => return Ok(None),
        };
        match lines.next() {
            Some(name) => Ok(Some(ActiveWorkspace {
                name: name.to_string(),
                config_path: Some(PathBuf::from(first)),
            })),
            None => Ok(Some(ActiveWorkspace {
                name: first.to_string(),
                config_path: None,
            })),
        }
    }
}
```

- [ ] **Step 4: Add `config_dir` (used here and in the CLI)**

In `src/config/mod.rs`:

```rust
use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("lain");
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config").join("lain")
}
```

- [ ] **Step 5: Strip the project registry from `src/state.rs`**

Delete `Project`, `RegistryFile`, `projects_file`, `current_file`, `read_registry`, `write_registry`, `RegistryError`, `Projects::list`, `Projects::add`, `Projects::forget`, `Projects::current`, `Projects::active_name`, `Projects::resolve_auto_workspace`. Keep only the `ActiveWorkspace` type (moved into `src/server/state.rs`).

- [ ] **Step 6: Strip `src/main.rs`**

Remove:
- `use lain::state::Projects;`
- `use lain::lock::WorkspaceLock;` (lock is dropped in PR 2)
- `use lain::sidecar;`
- `use lain::mode::LainMode;`
- `use lain::watcher::FileWatcher;`
- The `fn resolve_workspace_path(...)` function and its references.
- The `args.workspace.as_os_str() == "auto"` branch (the new resolution path is "config path argument" only; `--workspace auto` is supported via `ActiveWorkspace::load` later in PR 6).
- The owner/sidecar/auto plumbing.
- The `tracking = "auto"` logic for `--workspace`.

- [ ] **Step 7: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -40
```

Expected: only `src/main.rs` references to old paths remain, but the project-registry symbols are gone.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: drop project registry from state.rs"
```

---

### Task 1.9: Re-slim `src/main.rs` to the new subcommand set

**Files:**
- Modify: `src/main.rs` (entire file)
- Modify: `src/main.rs` (`#![...]` lines, `Args`, `Commands`, `ProjectsAction`, `AgentsAction`, `WorkspacesAction`)

**Interfaces:**
- Consumes: `lain::server::LainServer`, `lain::server::Transport`, `lain::cli::*`
- Produces: `main` function that dispatches `lain {server,workspaces,repos,query,ask} <sub>`

- [ ] **Step 1: Write the failing test**

Create `src/cli/dispatch.rs`:

```rust
#[test]
fn top_level_help_lists_only_kept_subcommands() {
    // Create a Command with --help output and assert it contains each subcommand.
    use clap::CommandFactory;
    let cmd = lain::main_command_factory();
    let help = clap::builder::Command::render_help(&cmd);
    assert!(help.contains("server"));
    assert!(help.contains("workspaces"));
    assert!(help.contains("repos"));
    assert!(help.contains("query"));
    assert!(help.contains("ask"));
    assert!(!help.contains("init"));
    assert!(!help.contains("agents"));
    assert!(!help.contains("projects"));
    assert!(!help.contains("hook"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib cli::dispatch::top_level_help_lists_only_kept_subcommands 2>&1 | head -20
```

Expected: FAIL because `lain::main_command_factory` doesn't exist.

- [ ] **Step 3: Add `main_command_factory` to `src/main.rs`**

```rust
pub fn main_command_factory() -> clap::Command {
    Args::command()
}
```

Add `use clap::CommandFactory;` at the top.

- [ ] **Step 4: Rewrite `src/main.rs` to the new shape**

```rust
//! `lain` — local MCP server for cross-repo and per-repo code analysis.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

mod cli;
mod server;
mod config;

pub use server::LainServer;
use clap::CommandFactory;
pub use crate::cli::dispatch::main_command_factory;

#[derive(Parser, Debug)]
#[command(name = "lain", author, version, about = "Local MCP server for code analysis", long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, clap::Subcommand)]
pub enum Commands {
    /// Start the MCP server (the headline).
    Server {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        #[arg(long, default_value = "stdio", value_parser = ["stdio", "http"])]
        transport: String,
        #[arg(long, default_value = "9999")]
        port: u16,
        #[arg(long, default_value = "info")]
        log_level: String,
        /// Active workspace. One of: "auto", "", or a workspace name.
        #[arg(long, default_value = "auto")]
        workspace: String,
    },
    /// Manage `workspaces.yaml` for the project.
    Workspaces {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        #[command(subcommand)]
        action: cli::workspaces::WorkspacesAction,
    },
    /// Manage `repos.yaml` for the project.
    Repos {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        #[command(subcommand)]
        action: cli::repos::ReposAction,
    },
    /// Run a query against the project's persisted graph.
    Query {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        expression: String,
    },
    /// Single-user LLM-assisted query.
    Ask {
        #[arg(long, default_value = "./repos.yaml")]
        config: PathBuf,
        question: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Some(Commands::Server { config, transport, port, log_level, workspace }) => {
            cli::server::run_server(&config, &transport, port, &log_level, &workspace).await
        }
        Some(Commands::Workspaces { config, action }) => {
            cli::workspaces::run(action, &config)
        }
        Some(Commands::Repos { config, action }) => {
            cli::repos::run(action, &config)
        }
        Some(Commands::Query { config, expression }) => {
            cli::query::run_query(&config, &expression)
        }
        Some(Commands::Ask { config, question }) => {
            cli::ask::run_ask(&config, &question)
        }
        None => {
            // Default: print help.
            let mut cmd = Args::command();
            cmd.print_help().ok();
            println!();
            Ok(())
        }
    }
}
```

- [ ] **Step 5: Add `src/cli/dispatch.rs`**

```rust
pub fn main_command_factory() -> clap::Command {
    crate::main_command_factory()
}
```

(Or expose `main_command_factory` from `src/main.rs` directly — see Step 3.)

- [ ] **Step 6: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -100
```

Expected: many errors in `src/cli/{workspaces,query,ask,server}.rs` because they still reference `crate::cmds::` paths. Tasks 1.10 and PR 2 will fix those.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: rewrite main.rs around new subcommand set"
```

---

### Task 1.10: Relax the `≥ 2 repos` workspace validation to `≥ 1`

**Files:**
- Modify: `src/server/federation/workspace.rs` (line ~80)
- Modify: `src/server/federation/workspace.rs` tests
- Modify: `tests/workspace_e2e.rs` (update the `workspace_rejects_sub_two_repos` test → `workspace_accepts_one_repo`)

**Interfaces:**
- Consumes: `WorkspacesFile::validate` (uses `ws.members.len() < 2`)
- Produces: `WorkspacesFile::validate` accepts `members.len() >= 1`

- [ ] **Step 1: Write the failing test**

In `src/server/federation/workspace.rs`, add a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_with_one_member_is_valid() {
        let yaml = r#"
workspaces:
  - name: solo
    members:
      - my-repo
"#;
        let file: WorkspacesFile = serde_yaml::from_str(yaml).unwrap();
        file.validate().expect("1-repo workspace should be valid");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib server::federation::workspace::tests::workspace_with_one_member_is_valid 2>&1 | head -20
```

Expected: FAIL with `"workspace 'solo' must contain >= 2 repos; got 1"`.

- [ ] **Step 3: Update `validate` to accept `>= 1`**

```rust
if ws.members.is_empty() {
    return Err(LainError::Config(format!(
        "workspace '{name}' must contain >= 1 repos; got 0",
        name = ws.name,
    )));
}
```

- [ ] **Step 4: Add a test for the empty case**

```rust
#[test]
fn workspace_with_zero_members_is_rejected() {
    let yaml = r#"
workspaces:
  - name: empty
    members: []
"#;
    let file: WorkspacesFile = serde_yaml::from_str(yaml).unwrap();
    assert!(file.validate().is_err());
}
```

- [ ] **Step 5: Update `tests/workspace_e2e.rs`**

Rename `workspace_rejects_sub_two_repos` → `workspace_rejects_zero_members`. Replace its body:

```rust
#[test]
fn workspace_rejects_zero_members() {
    let cfg = write_workspace_with_zero_members();
    assert!(load_workspaces(&cfg).is_err());
}
```

- [ ] **Step 6: Run the test to verify it passes**

```bash
cargo test --lib server::federation::workspace
cargo test --test workspace_e2e
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(workspaces): allow 1-repo workspaces"
```

---

### Task 1.11: Verify PR 1 end-to-end

**Files:**
- Modify: nothing (verification only)

- [ ] **Step 1: Run the full test suite**

```bash
cargo test --workspace 2>&1 | tail -50
```

Expected: tests pass modulo the PR 2 deletions (some tests will reference deleted modules — they will be removed in PR 2).

- [ ] **Step 2: Run `cargo build --release`**

```bash
cargo build --release 2>&1 | tail -20
```

Expected: produces `target/release/lain`.

- [ ] **Step 3: Verify the old entry points still work**

```bash
target/release/lain server --config tests/e2e/fixtures/repos.yaml --transport http --port 9999 &
SERVER_PID=$!
sleep 2
curl -s -X POST http://localhost:9999/mcp -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/list","params":{},"id":1}' | head -20
kill $SERVER_PID
```

Expected: returns a JSON list of MCP tools (same shape as before).

- [ ] **Step 4: Commit any fixups**

```bash
git add -A
git commit -m "fix: PR1 fixups for src/main.rs rewire" --allow-empty
```

---

## PR 2 — Cut multi-user CLI surface

### Task 2.1: Delete `init.rs` and `agents/`

**Files:**
- Delete: `src/cli/init.rs`
- Delete: `src/cli/agents/` (the entire directory)

- [ ] **Step 1: Delete the files**

```bash
git rm src/cli/init.rs
git rm -r src/cli/agents
```

- [ ] **Step 2: Remove `Init` from `src/main.rs`**

Remove the `Init { ... }` arm from the `Commands` enum and the `Commands::Init { ... } => cmds::run_init(...)` match arm.

- [ ] **Step 3: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -30
```

Expected: clean, or only test failures in old tests that referenced these.

- [ ] **Step 4: Delete the affected tests**

```bash
git rm tests/e2e/agent_install.rs 2>/dev/null || true
git rm tests/agents_cli_smoke.rs 2>/dev/null || true
git rm tests/agents_install.rs 2>/dev/null || true
git rm tests/e2e_agents.rs 2>/dev/null || true
git rm tests/e2e_copilot.rs 2>/dev/null || true
git rm tests/e2e_opencode.rs 2>/dev/null || true
```

- [ ] **Step 5: Remove the test entry from `Cargo.toml`**

```toml
[[test]]
name = "agent_install"   # delete this block
path = "tests/e2e/agent_install.rs"
```

- [ ] **Step 6: Verify `cargo test --workspace`**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: drop init/ and agents/ (multi-agent install)"
```

---

### Task 2.2: Delete `projects.rs` and `hook.rs`

**Files:**
- Delete: `src/cli/projects.rs`
- Delete: `src/cli/hook.rs`
- Delete: tests `tests/e2e/auto_workspace.rs`, `tests/dual_instance.rs` (multi-instance tests)

- [ ] **Step 1: Delete the files**

```bash
git rm src/cli/projects.rs
git rm src/cli/hook.rs
git rm tests/dual_instance.rs 2>/dev/null || true
git rm tests/e2e/auto_workspace.rs 2>/dev/null || true
```

- [ ] **Step 2: Remove `Projects`, `Use`, `Agents`, `Hook` from `Commands`**

In `src/main.rs`, remove the `Projects { ... }`, `Use { ... }`, `Agents { ... }`, `Hook { ... }` enum arms and their match arms.

- [ ] **Step 3: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -30
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor: drop projects/ and hook/ (multi-user coordination)"
```

---

### Task 2.3: Delete `src/mode.rs`, `src/lock.rs`, `src/sidecar.rs`

**Files:**
- Delete: `src/mode.rs`
- Delete: `src/lock.rs`
- Delete: `src/sidecar.rs`

- [ ] **Step 1: Delete the files**

```bash
git rm src/mode.rs src/lock.rs src/sidecar.rs 2>/dev/null || true
# (paths may differ depending on whether they were moved in PR 1 — adjust as needed)
```

If these files were moved to `src/server/` in PR 1, delete from there instead.

- [ ] **Step 2: Remove the `pub mod` declarations from `src/lib.rs` (or `src/server/mod.rs`)**

```rust
// Delete:
// pub mod sidecar;
// pub mod lock;
// pub mod mode;
```

- [ ] **Step 3: Remove the owner/sidecar plumbing from `src/main.rs`**

Strip the `mode` flag, the `WorkspaceLock`, the owner/sidecar/auto branches, and the `lain::sidecar::run` call. The new `Commands::Server` arm does NOT need `--mode`.

- [ ] **Step 4: Verify `cargo build`**

```bash
cargo build --workspace 2>&1 | head -30
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: drop owner/sidecar/mode/lock (multi-instance coordination)"
```

---

### Task 2.4: Delete `crates/lain-mcp-probe/` and remove its dep

**Files:**
- Delete: `crates/lain-mcp-probe/`
- Modify: `Cargo.toml` (remove the `members = [..., "crates/lain-mcp-probe"]` entry and the `lain-mcp-probe = ...` dep)

- [ ] **Step 1: Delete the crate**

```bash
git rm -r crates/lain-mcp-probe
```

- [ ] **Step 2: Update `Cargo.toml`**

Remove the `lain-mcp-probe` line from the workspace members and the `[dependencies]` block.

- [ ] **Step 3: Drop `libc` and `fs2` deps (used only by lock/sidecar)**

```bash
# Search for unused uses of libc and fs2
grep -rn "libc::\|fs2::" src/ tests/ 2>/dev/null | head -20
# If only used in deleted modules, remove the deps
```

In `Cargo.toml`:
```toml
# Delete:
# libc = "0.2"
# fs2 = "0.4"
```

- [ ] **Step 4: Verify `cargo build --workspace` succeeds**

```bash
cargo build --workspace 2>&1 | tail -30
```

Expected: clean.

- [ ] **Step 5: Verify `cargo test --workspace` succeeds**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: drop lain-mcp-probe, libc, fs2"
```

---

### Task 2.5: Rewrite the README quickstart

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Replace the "Quick Start" section**

Replace the entire "Quick Start" section with:

```markdown
## Quick Start

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/spuentesp/lain/main/install.sh | bash
```

### 2. Configure your project

A project is one paired `repos.yaml` + `workspaces.yaml`. Pick or create a directory:

```bash
mkdir -p ~/projects/biller
cd ~/projects/biller
lain repos add auth-svc https://github.com/acme/auth-svc.git
lain repos add billing-svc https://github.com/acme/billing-svc.git
lain workspaces create biller-core --members auth-svc,billing-svc
```

### 3. Run the server

```bash
lain server --config ./repos.yaml --transport http --port 9999
# Open http://localhost:9999 in your browser for the Command Center.
```

### 4. Wire your agent

Add the following to your agent's MCP config (the URL is documented for your specific agent):

```json
{
  "mcpServers": {
    "lain": {
      "command": "lain",
      "args": ["server", "--config", "./repos.yaml", "--transport", "stdio"]
    }
  }
}
```

That's it. The next time your agent starts, it sees the federation, the workspace, and the full MCP tool surface.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: rewrite quickstart around lain server + workspaces/repos"
```

---

### Task 2.6: Verify PR 2 end-to-end

- [ ] **Step 1: Confirm `lain --help` lists exactly the kept subcommands**

```bash
cargo run --quiet -- --help
```

Expected: lists `server`, `workspaces`, `repos`, `query`, `ask`. No `init`, `agents`, `projects`, `hook`, `use`.

- [ ] **Step 2: Confirm the kept subcommands work**

```bash
cargo run --quiet -- workspaces create test --members r1
cargo run --quiet -- workspaces list
cargo run --quiet -- workspaces show test
cargo run --quiet -- workspaces use test
cargo run --quiet -- workspaces current
cargo run --quiet -- workspaces forget test
cargo run --quiet -- repos add r1 https://example.com/r1.git
cargo run --quiet -- repos list
cargo run --quiet -- repos remove r1
```

Expected: each command exits 0.

- [ ] **Step 3: Commit any fixups**

```bash
git add -A
git commit -m "fix: PR2 fixups" --allow-empty
```

---

## PR 3 — Add `repos` CLI

### Task 3.1: Define the `ReposAction` enum and `run` dispatcher

**Files:**
- Modify: `src/cli/repos.rs` (new file, replaces the placeholder stub from PR 1)

**Interfaces:**
- Consumes: `ReposAction` (clap subcommand enum)
- Produces: `fn run(action: ReposAction, config_path: &Path) -> Result<()>`

- [ ] **Step 1: Write the failing test**

Create `src/cli/repos.rs`:

```rust
use anyhow::Result;
use clap::Subcommand;
use std::path::Path;

#[derive(Debug, Subcommand)]
pub enum ReposAction {
    Add {
        name: String,
        url: String,
        #[arg(long, default_value = "main")]
        ref_: String,
    },
    List,
    Remove { name: String },
}

pub fn run(action: ReposAction, config_path: &Path) -> Result<()> {
    match action {
        ReposAction::Add { name, url, ref_ } => add(config_path, &name, &url, &ref_),
        ReposAction::List => list(config_path),
        ReposAction::Remove { name } => remove(config_path, &name),
    }
}

fn add(_config_path: &Path, _name: &str, _url: &str, _ref_: &str) -> Result<()> {
    todo!()
}

fn list(_config_path: &Path) -> Result<()> {
    todo!()
}

fn remove(_config_path: &Path, _name: &str) -> Result<()> {
    todo!()
}
```

- [ ] **Step 2: Run the test (compile-only) to verify it compiles**

```bash
cargo build --workspace 2>&1 | head -30
```

Expected: PASS (the `todo!()` macros compile).

- [ ] **Step 3: Commit**

```bash
git add src/cli/repos.rs
git commit -m "feat(repos): scaffold repos CLI subcommand"
```

---

### Task 3.2: Implement `add` / `list` / `remove`

**Files:**
- Modify: `src/cli/repos.rs`

**Interfaces:**
- Consumes: `ReposFile` from `crate::server::federation::config::ReposFile`
- Produces: working `add`, `list`, `remove` functions that read, mutate, and write `repos.yaml` atomically.

- [ ] **Step 1: Write the failing test**

In `src/cli/repos.rs`, add a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::federation::config::ReposFile;
    use std::fs;

    fn write_repos(dir: &Path) -> PathBuf {
        let path = dir.join("repos.yaml");
        let yaml = r#"
repos:
  - id: existing
    path: /tmp/existing
    source:
      type: dir
      path: /tmp/existing
"#;
        fs::write(&path, yaml).unwrap();
        path
    }

    #[test]
    fn add_appends_new_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        add(&path, "new-repo", "https://example.com/new.git", "main").unwrap();
        let file = ReposFile::load(&path).unwrap();
        assert!(file.repos.iter().any(|r| r.id == "new-repo"));
    }

    #[test]
    fn add_rejects_duplicate_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        let err = add(&path, "existing", "https://example.com/x.git", "main").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn remove_drops_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        remove(&path, "existing").unwrap();
        let file = ReposFile::load(&path).unwrap();
        assert!(file.repos.is_empty());
    }

    #[test]
    fn remove_unknown_id_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_repos(tmp.path());
        let err = remove(&path, "nope").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib cli::repos::tests 2>&1 | head -30
```

Expected: FAIL with `not yet implemented`.

- [ ] **Step 3: Implement the three functions**

```rust
use crate::server::federation::config::ReposFile;
use std::fs;

fn add(config_path: &Path, name: &str, url: &str, ref_: &str) -> Result<()> {
    let mut file = ReposFile::load(config_path).unwrap_or_default();
    if file.repos.iter().any(|r| r.id == name) {
        anyhow::bail!("repo '{name}' already exists in {}", config_path.display());
    }
    file.repos.push(crate::server::federation::config::RepoSpec {
        id: name.to_string(),
        source: crate::server::federation::config::SourceConfig::Clone {
            url: url.to_string(),
            ref_: Some(ref_.to_string()),
        },
    });
    write_atomic(config_path, &file)
}

fn list(config_path: &Path) -> Result<()> {
    let file = ReposFile::load(config_path).unwrap_or_default();
    for r in &file.repos {
        println!("{}\t{:?}", r.id, r.source);
    }
    Ok(())
}

fn remove(config_path: &Path, name: &str) -> Result<()> {
    let mut file = ReposFile::load(config_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let before = file.repos.len();
    file.repos.retain(|r| r.id != name);
    if file.repos.len() == before {
        anyhow::bail!("repo '{name}' not found in {}", config_path.display());
    }
    write_atomic(config_path, &file)
}

fn write_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
    let yaml = serde_yaml::to_string(value)?;
    fs::write(&tmp, yaml)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
```

(Adjust the `RepoSpec` / `SourceConfig` field names to match the actual `src/server/federation/config.rs` types — read the file first if needed.)

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib cli::repos::tests 2>&1 | tail -20
```

Expected: PASS.

- [ ] **Step 5: Verify the CLI end-to-end**

```bash
cargo run --quiet -- --config /tmp/test.yaml repos add foo https://example.com/foo.git
cargo run --quiet -- --config /tmp/test.yaml repos list
cargo run --quiet -- --config /tmp/test.yaml repos remove foo
```

Expected: each command exits 0.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(repos): add/list/remove against repos.yaml"
```

---

## PR 4 — Build the Command Center

### Task 4.1: Add `get_server_status` tool

**Files:**
- Modify: `src/server/mcp/federation_tools.rs` (add a new tool definition)
- Modify: `src/server/server/mod.rs` (carry the new fields: `started_at`, `last_sync_at`, `last_error`)
- Modify: `src/server/mcp/handler.rs` (route the new tool)

**Interfaces:**
- Consumes: `LainServer::started_at`, `last_sync_at`, `last_error`
- Produces: `get_server_status` MCP tool returning `{pid, transport, port, started_at, last_sync_at, last_error?, repo_count, workspace_count}`

- [ ] **Step 1: Write the failing test**

In `src/server/mcp/federation_tools.rs`, add a test module:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn get_server_status_returns_expected_shape() {
        let status = run_get_server_status_for_test();
        let json: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert!(json.get("pid").is_some());
        assert!(json.get("transport").is_some());
        assert!(json.get("repo_count").is_some());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib server::mcp::federation_tools::tests::get_server_status 2>&1 | head -20
```

Expected: FAIL (`run_get_server_status_for_test` not defined).

- [ ] **Step 3: Implement the helper**

```rust
#[cfg(test)]
fn run_get_server_status_for_test() -> String {
    use crate::server::reload_status_for_test;
    let s = reload_status_for_test();
    serde_json::to_string(&s).unwrap()
}
```

Add `pub fn reload_status_for_test() -> ServerStatus` in `src/server/server/mod.rs` that returns a stub. Production wires `Arc<AtomicDateTime<Utc>>` for `started_at` and `last_sync_at`.

- [ ] **Step 4: Add the production tool**

```rust
pub fn get_server_status_schema() -> ToolSchema {
    ToolSchema::new("get_server_status")
        .description("Returns the server's run-time status: pid, transport, port, started_at, last_sync_at, last_error (optional), repo_count, workspace_count.")
}

pub fn run_get_server_status(server: &LainServer) -> Result<serde_json::Value, LainError> {
    Ok(json!({
        "pid": std::process::id(),
        "transport": server.transport().label(),
        "port": server.port(),
        "started_at": server.started_at().to_rfc3339(),
        "last_sync_at": server.last_sync_at().to_rfc3339(),
        "last_error": server.last_error(),
        "repo_count": server.federation().map(|f| f.list_repos().len()).unwrap_or(0),
        "workspace_count": server.federation_workspaces().map(|w| w.workspaces.len()).unwrap_or(0),
    }))
}
```

(Fill in the missing accessors on `LainServer`.)

- [ ] **Step 5: Register the tool in `src/server/mcp/handler.rs`**

```rust
tool_registry.register("get_server_status", get_server_status_schema(), |args, ctx| {
    run_get_server_status(ctx.server())
});
```

- [ ] **Step 6: Run the test to verify it passes**

```bash
cargo test --lib server::mcp::federation_tools::tests::get_server_status 2>&1 | tail -20
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(mcp): add get_server_status tool"
```

---

### Task 4.2: Add `list_recent_projects` tool

**Files:**
- Create: `src/config/recent_projects.rs`
- Modify: `src/server/mcp/federation_tools.rs`

**Interfaces:**
- Consumes: `~/.config/lain/recent_projects` (a TOML file)
- Produces: `list_recent_projects` MCP tool returning `[{path, last_used, workspace_count, repo_count}]`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn list_recent_projects_returns_empty_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        let list = crate::config::recent_projects::list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn record_and_list_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        crate::config::recent_projects::record(Path::new("/tmp/a")).unwrap();
        crate::config::recent_projects::record(Path::new("/tmp/b")).unwrap();
        let list = crate::config::recent_projects::list().unwrap();
        assert_eq!(list[0].path, Path::new("/tmp/b"));
        assert_eq!(list[1].path, Path::new("/tmp/a"));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib config::recent_projects::tests 2>&1 | head -20
```

Expected: FAIL (module doesn't exist).

- [ ] **Step 3: Implement `src/config/recent_projects.rs`**

```rust
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentProject {
    pub path: PathBuf,
    pub last_used: String,
}

fn file() -> PathBuf {
    crate::config::config_dir().join("recent_projects")
}

pub fn record(config_path: &Path) -> Result<()> {
    let mut list = list().unwrap_or_default();
    list.retain(|r| r.path != config_path);
    let since_epoch = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    list.insert(0, RecentProject {
        path: config_path.to_path_buf(),
        last_used: format!("{since_epoch}"),
    });
    list.truncate(20);
    let dir = crate::config::config_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(file(), serde_json::to_string_pretty(&list)?)?;
    Ok(())
}

pub fn list() -> Result<Vec<RecentProject>> {
    let path = file();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&text)?)
}
```

- [ ] **Step 4: Add the MCP tool**

In `src/server/mcp/federation_tools.rs`:

```rust
pub fn list_recent_projects_schema() -> ToolSchema {
    ToolSchema::new("list_recent_projects")
        .description("List projects the operator has used recently.")
}

pub fn run_list_recent_projects() -> Result<serde_json::Value, LainError> {
    let list = crate::config::recent_projects::list().map_err(|e| LainError::Other(e.to_string()))?;
    let mut enhanced = Vec::new();
    for r in &list {
        let cfg = r.path.parent().map(|p| p.join("repos.yaml")).unwrap_or_else(|| r.path.clone());
        let (workspace_count, repo_count) = read_metadata(&cfg);
        enhanced.push(json!({
            "path": r.path,
            "last_used": r.last_used,
            "workspace_count": workspace_count,
            "repo_count": repo_count,
        }));
    }
    Ok(json!(enhanced))
}

fn read_metadata(repos_yaml: &Path) -> (usize, usize) {
    let file = crate::server::federation::config::ReposFile::load(repos_yaml).ok();
    let repos = file.as_ref().map(|f| f.repos.len()).unwrap_or(0);
    let ws_path = repos_yaml.parent().map(|p| p.join("workspaces.yaml"));
    let ws = ws_path.and_then(|p| crate::server::federation::workspace::WorkspacesFile::load(&p).ok());
    let workspaces = ws.map(|f| f.workspaces.len()).unwrap_or(0);
    (worksspaces, repos)
}
```

- [ ] **Step 5: Hook it up**

In `src/server/server/mod.rs`:

```rust
impl LainServer {
    pub fn start_recording_project(&self) {
        let _ = crate::config::recent_projects::record(&self.config.repos_yaml);
    }
}
```

Add `repos_yaml: PathBuf` to `LainConfig`. Update all `LainConfig` constructors.

In `src/cli/server.rs`:

```rust
server.start_recording_project();
```

Register the tool in `src/server/mcp/handler.rs`. Document the new tool in `src/server/mcp/`.

- [ ] **Step 6: Run the tests**

```bash
cargo test --lib config::recent_projects::tests
cargo test --lib server::mcp::federation_tools::tests
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(mcp): add list_recent_projects tool + recent_projects tracking"
```

---

### Task 4.3: Scaffold the command_center SPA shell

**Files:**
- Create: `src/server/mcp/command_center/index.html`
- Create: `src/server/mcp/command_center/app.js`
- Create: `src/server/mcp/command_center/styles.css`
- Create: `src/server/mcp/command_center/assets/d3.v7.min.js` (vendored)
- Modify: `src/server/mcp/handler.rs` (route `GET /`, `GET /app.js`, `GET /styles.css`, `GET /assets/*`)

- [ ] **Step 1: Write the `index.html` skeleton**

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>LAIN — Command Center</title>
  <link rel="stylesheet" href="/styles.css">
</head>
<body>
  <header class="topbar">
    <h1>LAIN</h1>
    <span id="active-project">project: ...</span>
    <span id="active-workspace">workspace: ...</span>
  </header>
  <div class="layout">
    <aside class="sidebar">
      <section><h2>Workspaces</h2><ul id="workspaces"></ul></section>
      <section><h2>Repos</h2><ul id="repos"></ul></section>
      <section><h2>Recent projects</h2><ul id="recent-projects"></ul></section>
    </aside>
    <main>
      <nav class="tabs">
        <button data-tab="overview">Overview</button>
        <button data-tab="graph">Graph</button>
        <button data-tab="repos">Repos</button>
        <button data-tab="query">Query</button>
        <button data-tab="tools">Tools</button>
      </nav>
      <section id="tab-overview" class="tab"></section>
      <section id="tab-graph" class="tab"></section>
      <section id="tab-repos" class="tab"></section>
      <section id="tab-query" class="tab"></section>
      <section id="tab-tools" class="tab"></section>
    </main>
  </div>
  <footer class="statusbar">
    <span id="status-pid">pid: ...</span>
    <span id="status-transport">transport: ...</span>
    <span id="status-repos">repos: ...</span>
    <span id="status-reload">reload: idle</span>
  </footer>
  <script src="/assets/d3.v7.min.js"></script>
  <script src="/app.js"></script>
</body>
</html>
```

- [ ] **Step 2: Write `app.js` skeleton**

```js
async function mcpCall(name, args) {
  const r = await fetch('/mcp', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({jsonrpc: '2.0', method: 'tools/call', params: {name, arguments: args || {}}, id: 1}),
  });
  return (await r.json()).result;
}

async function init() {
  const status = await mcpCall('get_server_status');
  document.getElementById('status-pid').textContent = `pid: ${status.pid}`;
  // ... etc.
}

document.querySelectorAll('[data-tab]').forEach(btn => {
  btn.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach(t => t.style.display = 'none');
    document.getElementById('tab-' + btn.dataset.tab).style.display = 'block';
  });
});

init();
```

- [ ] **Step 3: Vendor D3**

```bash
curl -L https://d3js.org/d3.v7.min.js -o src/server/mcp/command_center/assets/d3.v7.min.js
```

- [ ] **Step 4: Wire the routes in `src/server/mcp/handler.rs`**

Static-asset routing:

```rust
async fn serve_static(path: &str) -> Option<Vec<u8>> {
    let bytes = include_bytes!("command_center/index.html");
    let css = include_bytes!("command_center/styles.css");
    let js = include_bytes!("command_center/app.js");
    match path {
        "" | "/" | "/index.html" => Some(bytes.to_vec()),
        "/styles.css" => Some(css.to_vec()),
        "/app.js" => Some(js.to_vec()),
        _ => None,
    }
}
```

- [ ] **Step 5: Verify `cargo run -- server --transport http --port 9999` serves the page**

```bash
cargo run --quiet -- server --config ./repos.yaml --transport http --port 9999 &
SERVER_PID=$!
sleep 2
curl -sI http://localhost:9999/ | head -1
curl -sI http://localhost:9999/app.js | head -1
curl -sI http://localhost:9999/styles.css | head -1
kill $SERVER_PID
```

Expected: HTTP 200 for each.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(command-center): scaffold SPA shell"
```

---

### Task 4.4: Implement the Workspace section

**Files:**
- Modify: `src/server/mcp/command_center/app.js`

- [ ] **Step 1: Verify the integration**

```bash
cargo run --quiet -- server --config tests/e2e/fixtures/repos.yaml --transport http --port 9999 &
SERVER_PID=$!
sleep 2
# Browser should show three workspaces from the fixture.
curl -s -X POST http://localhost:9999/mcp -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_workspaces","arguments":{}},"id":1}'
kill $SERVER_PID
```

- [ ] **Step 2: Implement `renderWorkspaces` in `app.js`**

```js
async function renderWorkspaces() {
  const r = await mcpCall('list_workspaces');
  const ul = document.getElementById('workspaces');
  ul.innerHTML = '';
  for (const ws of (r.workspaces || [])) {
    const li = document.createElement('li');
    li.textContent = `${ws.name} (${ws.member_count})`;
    if (ws.is_active) li.classList.add('active');
    ul.appendChild(li);
  }
}
```

Call `renderWorkspaces()` from `init()`.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(command-center): workspace list section"
```

---

### Task 4.5: Implement the Project switcher section

**Files:**
- Modify: `src/server/mcp/command_center/app.js`

- [ ] **Step 1: Implement `renderRecentProjects`**

```js
async function renderRecentProjects() {
  const r = await mcpCall('list_recent_projects');
  const ul = document.getElementById('recent-projects');
  ul.innerHTML = '';
  for (const p of r || []) {
    const li = document.createElement('li');
    li.innerHTML = `<code>${p.path}</code> (${p.workspace_count} workspaces, ${p.repo_count} repos) <button data-path="${p.path}">Switch</button>`;
    li.querySelector('button').addEventListener('click', () => {
      const cmd = `lain server --config ${p.path}${p.active_workspace ? ' --workspace ' + p.active_workspace : ''}`;
      navigator.clipboard.writeText(cmd);
      alert('Copied:\n' + cmd);
    });
    ul.appendChild(li);
  }
}
```

- [ ] **Step 2: Add `active_workspace` to `list_recent_projects`**

(In `src/server/mcp/federation_tools.rs`, augment the tool return with `active_workspace` from `ActiveWorkspace::load()`.)

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(command-center): recent projects switcher"
```

---

### Task 4.6: Implement the Repos tab

**Files:**
- Modify: `src/server/mcp/command_center/app.js`

- [ ] **Step 1: Implement `renderReposTab`**

```js
async function renderReposTab() {
  const r = await mcpCall('list_repos');
  const tab = document.getElementById('tab-repos');
  tab.innerHTML = '<table><thead><tr><th>id</th><th>path</th><th>health</th></tr></thead><tbody></tbody></table>';
  const tbody = tab.querySelector('tbody');
  for (const repo of r.repos || []) {
    const info = await mcpCall('get_repo_info', {id: repo.id});
    const tr = document.createElement('tr');
    tr.innerHTML = `<td>${repo.id}</td><td><code>${info.path}</code></td><td>${info.health}</td>`;
    tbody.appendChild(tr);
  }
}
```

Call from `init()`.

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "feat(command-center): repos tab"
```

---

### Task 4.7: Implement the Query tab

**Files:**
- Modify: `src/server/mcp/command_center/app.js`

- [ ] **Step 1: Implement `renderQueryTab`**

```js
async function renderQueryTab() {
  const tab = document.getElementById('tab-query');
  tab.innerHTML = `
    <textarea id="query-input" placeholder="find Function | limit 10" rows="3"></textarea>
    <button id="query-run">Run</button>
    <pre id="query-output"></pre>
  `;
  tab.querySelector('#query-run').addEventListener('click', async () => {
    const expr = tab.querySelector('#query-input').value;
    const out = await mcpCall('query_graph', {ops: parseQuery(expr)});
    tab.querySelector('#query-output').textContent = JSON.stringify(out, null, 2);
  });
}

function parseQuery(expr) {
  // Minimal parser for "<find NodeType> | <op> ..." strings.
  // For now, single find: [ { op: 'find', type: expr.trim() } ]
  return [{op: 'find', type: expr.trim().split(/\s+/)[1] || 'Function'}];
}
```

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "feat(command-center): query tab"
```

---

### Task 4.8: Implement the Tools tab

**Files:**
- Modify: `src/server/mcp/command_center/app.js`

- [ ] **Step 1: Implement `renderToolsTab`**

```js
async function renderToolsTab() {
  const r = await fetch('/mcp', {
    method: 'POST',
    headers: {'Content-Type': 'application/json'},
    body: JSON.stringify({jsonrpc: '2.0', method: 'tools/list', params: {}, id: 1}),
  });
  const {result} = await r.json();
  const tab = document.getElementById('tab-tools');
  tab.innerHTML = '<ul id="tools-list"></ul><div id="tool-form"></div>';
  const ul = tab.querySelector('#tools-list');
  for (const tool of result.tools) {
    const li = document.createElement('li');
    li.innerHTML = `<button data-name="${tool.name}">${tool.name}</button> ${tool.description || ''}`;
    li.querySelector('button').addEventListener('click', () => renderToolForm(tool));
    ul.appendChild(li);
  }
}

function renderToolForm(tool) {
  const container = document.getElementById('tool-form');
  const schema = tool.inputSchema || {type: 'object', properties: {}};
  const fields = Object.entries(schema.properties || {}).map(([k, v]) => {
    return `<label>${k} <input name="${k}" placeholder="${v.description || ''}"></label>`;
  }).join('');
  container.innerHTML = `
    <h3>${tool.name}</h3>
    <form id="tool-args">${fields}</form>
    <button id="tool-call">Call</button>
    <button id="tool-curl">Save as curl</button>
    <pre id="tool-result"></pre>
  `;
  container.querySelector('#tool-call').addEventListener('click', async () => {
    const args = {};
    container.querySelectorAll('#tool-args input').forEach(i => {
      if (i.value) args[i.name] = i.value;
    });
    const out = await mcpCall(tool.name, args);
    container.querySelector('#tool-result').textContent = JSON.stringify(out, null, 2);
  });
  container.querySelector('#tool-curl').addEventListener('click', () => {
    const args = {};
    container.querySelectorAll('#tool-args input').forEach(i => {
      if (i.value) args[i.name] = i.value;
    });
    const curl = `curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' -d '${JSON.stringify({jsonrpc: '2.0', method: 'tools/call', params: {name: tool.name, arguments: args}, id: 1})}'`;
    navigator.clipboard.writeText(curl);
    alert('Copied cURL to clipboard');
  });
}
```

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "feat(command-center): tools tab (MCP tool tester)"
```

---

### Task 4.9: Implement the Status bar

**Files:**
- Modify: `src/server/mcp/command_center/app.js`

- [ ] **Step 1: Implement `renderStatusBar` and poll every 2s**

```js
async function renderStatusBar() {
  const s = await mcpCall('get_server_status');
  document.getElementById('status-pid').textContent = `pid: ${s.pid}`;
  document.getElementById('status-transport').textContent = `transport: ${s.transport}`;
  document.getElementById('status-repos').textContent = `repos: ${s.repo_count}`;
  if (window.__lastStatus) {
    if (s.last_sync_at !== window.__lastStatus.last_sync_at) {
      // Could trigger a refresh of repos / workspaces here.
    }
  }
  window.__lastStatus = s;
}
setInterval(renderStatusBar, 2000);
```

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "feat(command-center): status bar with 2s polling"
```

---

### Task 4.10: Write `docs/command-center.md`

**Files:**
- Create: `docs/command-center.md`

- [ ] **Step 1: Write the walkthrough**

```markdown
# Command Center

The Command Center is the operator's primary surface for inspecting and steering `lain server`. Run it with:

```bash
lain server --config ./repos.yaml --transport http --port 9999
```

Open `http://localhost:9999` in your browser.

## Sections

- **Workspace switcher** (sidebar): lists `workspaces.yaml`. The active workspace is highlighted.
- **Project switcher** (sidebar): lists recent projects. Click to copy a `lain server` restart command.
- **Config** (tab): read-only view of `repos.yaml` and `workspaces.yaml`. Add buttons emit `lain` commands.
- **Graph** (tab): D3 force-directed graph of the active workspace. Filter by repo, edge kind, name.
- **Repos** (tab): per-repo table with id, path, health.
- **Query** (tab): run queries against the persisted graph.
- **Tools** (tab): MCP tool tester. Auto-generated forms from `tools/list`.
- **Status bar** (footer): server pid, transport, repo count, last sync.
```

- [ ] **Step 2: Commit**

```bash
git add docs/command-center.md
git commit -m "docs(command-center): initial walkthrough"
```

---

## PR 5 — Cleanup + docs

### Task 5.1: Drop unused deps

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Identify unused deps**

```bash
cargo build --workspace 2>&1 | grep "warning: unused" | head -20
```

- [ ] **Step 2: Remove them**

Delete `[dependencies]` lines that are no longer referenced.

- [ ] **Step 3: Verify `cargo build` is clean**

```bash
cargo build --workspace 2>&1 | tail -10
```

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml
git commit -m "chore: drop unused deps"
```

---

### Task 5.2: Update docs

**Files:**
- Modify: `README.md`
- Modify: `docs/FEDERATION.md`
- Modify: `docs/REPOS_YAML.md`
- Modify: `docs/quickstart-tools.md`
- Modify: `docs/TECHNICAL.md`
- Modify: `docs/agent-installation.md` (replace with a one-line snippet)
- Modify: `docs/superpowers/specs/2026-08-08-agent-installation-design.md` (mark superseded)
- Modify: `docs/superpowers/specs/2026-08-09-agent-e2e-dst-harness-design.md`
- Modify: `docs/superpowers/specs/2026-08-09-commands-agents-followup-design.md`
- Modify: `docs/superpowers/specs/2026-08-10-opencode-agent-design.md`
- Modify: `docs/superpowers/specs/2026-08-10-copilot-vscode-design.md`
- Modify: `docs/superpowers/specs/2026-08-10-auto-mcp-workspace-design.md`
- Modify: `docs/superpowers/specs/2026-08-08-multi-instance-owner-sidecar-design.md`

- [ ] **Step 1: Rewrite the README**

Drop sections: "Init", "Project Management", "Multi-instance / sidecar mode", "Semantic Search", "Build Integration", "A/B Testing Results", "Recent Improvements (0.4.x and 0.5.0)" tables that reference cut features.

Add sections: "Command Center", "Hot Reload", "Multi-project".

- [ ] **Step 2: Update FEDERATION.md**

Lead with `lain server --config repos.yaml [+ --workspace <name>]`. Reference the new command center.

- [ ] **Step 3: Update REPOS_YAML.md, query-language.md, quickstart-tools.md, TECHNICAL.md**

Replace `--workspace` references with `--config`. Reference the new CLI.

- [ ] **Step 4: Mark old specs as superseded**

For each of the seven specs, prepend:

```markdown
> **Status:** Superseded by `docs/superpowers/specs/2026-08-14-lain-consolidation-design.md`.
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: rewrite for consolidated scope"
```

---

### Task 5.3: Verify PR 5 end-to-end

- [ ] **Step 1: Run the full test suite**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 2: Verify install scripts**

```bash
bash scripts/pre-flight-check.sh
```

Expected: 0 exit.

- [ ] **Step 3: Commit any fixups**

```bash
git add -A
git commit -m "fix: PR5 fixups" --allow-empty
```

---

## PR 6 — Hot-reload of `repos.yaml` / `workspaces.yaml`

### Task 6.1: Define `ReloadBus` and `ReloadStatus`

**Files:**
- Create: `src/server/reload.rs`

**Interfaces:**
- Consumes: `LainServer`, `repos.yaml`, `workspaces.yaml`
- Produces: `ReloadBus`, `ReloadStatus`, `ReloadSubscriber`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn reload_bus_broadcasts() {
        let bus = ReloadBus::new();
        let mut sub = bus.subscribe();
        bus.request_reload().unwrap();
        assert!(sub.try_recv().is_ok());
    }

    #[test]
    fn reload_status_reports_state() {
        let bus = ReloadBus::new();
        assert_eq!(bus.status().state, ReloadState::Idle);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib server::reload::tests 2>&1 | head -20
```

Expected: FAIL (module doesn't exist).

- [ ] **Step 3: Implement `ReloadBus`**

```rust
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

#[derive(Debug, Clone, PartialEq)]
pub enum ReloadState { Idle, Rebuilding, Failed(String) }

#[derive(Debug, Clone)]
pub struct ReloadStatus {
    pub state: ReloadState,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_reload_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
    pub pending_changes: Vec<String>,
}

pub struct ReloadBus {
    tx: broadcast::Sender<()>,
    status: Arc<Mutex<ReloadStatus>>,
}

impl ReloadBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self {
            tx,
            status: Arc::new(Mutex::new(ReloadStatus {
                state: ReloadState::Idle,
                started_at: None,
                last_reload_at: None,
                last_error: None,
                pending_changes: Vec::new(),
            })),
        }
    }

    pub fn subscribe(&self) -> ReloadSubscriber {
        ReloadSubscriber { rx: self.tx.subscribe() }
    }

    pub fn request_reload(&self) -> Result<(), String> {
        let _ = self.tx.send(());
        Ok(())
    }

    pub fn status(&self) -> ReloadStatus {
        self.status.lock().unwrap().clone()
    }

    pub async fn set_state(&self, state: ReloadState) {
        let mut s = self.status.lock().await;
        s.state = state.clone();
        if matches!(state, ReloadState::Rebuilding) {
            s.started_at = Some(chrono::Utc::now());
        } else if matches!(state, ReloadState::Idle) {
            s.last_reload_at = Some(chrono::Utc::now());
        }
    }
}

pub struct ReloadSubscriber {
    rx: broadcast::Receiver<()>,
}

impl ReloadSubscriber {
    pub fn try_recv(&mut self) -> Option<()> {
        self.rx.try_recv().ok()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib server::reload::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(reload): ReloadBus type with subscribe/request_reload/status"
```

---

### Task 6.2: Implement the rebuild task

**Files:**
- Modify: `src/server/reload.rs`
- Modify: `src/server/server/mod.rs`

**Interfaces:**
- Consumes: `ReloadBus`, `LainServer`
- Produces: `run_rebuild(server: &LainServer, bus: &ReloadBus) -> Result<()>`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn rebuild_swaps_workspace_index() {
    let tmp = tempfile::tempdir().unwrap();
    let repos = tmp.path().join("repos.yaml");
    let ws = tmp.path().join("workspaces.yaml");
    std::fs::write(&repos, "repos:\n  - id: r1\n    source:\n      type: dir\n      path: /tmp/r1\n").unwrap();
    std::fs::write(&ws, "workspaces:\n  - name: w1\n    members: [r1]\n").unwrap();
    // Build a server, then mutate ws.yaml, then call rebuild, then assert.
    let bus = ReloadBus::new();
    let mut server = build_test_server(&repos).await;
    run_rebuild(&mut server, &bus).await.unwrap();
    assert_eq!(bus.status().state, ReloadState::Idle);
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib server::reload::tests::rebuild_swaps_workspace_index 2>&1 | head -20
```

Expected: FAIL.

- [ ] **Step 3: Implement `run_rebuild`**

```rust
pub async fn run_rebuild(server: &mut LainServer, bus: &ReloadBus) -> Result<(), LainError> {
    bus.set_state(ReloadState::Rebuilding).await;
    let result = (|| async {
        // 1. Re-load repos.yaml and workspaces.yaml.
        let repos_file = ReposFile::load(&server.config.repos_yaml)?;
        let ws_path = server.config.repos_yaml.parent().unwrap().join("workspaces.yaml");
        let ws_file = if ws_path.exists() { Some(WorkspacesFile::load(&ws_path)?) } else { None };

        // 2. Compute the active workspace.
        let active = resolve_active_workspace(&ws_file, &server.config.active_workspace)?;

        // 3. Diff vs. the previous state.
        let prev_repos: std::collections::HashSet<String> = server.repo_ids().into_iter().collect();
        let new_repos: std::collections::HashSet<String> = repos_file.repos.iter().map(|r| r.id.clone()).collect();
        let added: Vec<_> = new_repos.difference(&prev_repos).cloned().collect();
        let removed: Vec<_> = prev_repos.difference(&new_repos).cloned().collect();

        // 4. For each new repo, clone/fetch and project.
        for id in &added {
            server.add_repo(&repos_file, id).await?;
        }

        // 5. For each removed repo, drop from the global backend.
        for id in &removed {
            server.remove_repo(id).await?;
        }

        // 6. Update the workspace index.
        server.set_workspace(active).await?;

        Ok(())
    })().await;

    match result {
        Ok(_) => { bus.set_state(ReloadState::Idle).await; Ok(()) }
        Err(e) => {
            let msg = e.to_string();
            bus.set_state(ReloadState::Failed(msg.clone())).await;
            Err(e)
        }
    }
}
```

- [ ] **Step 4: Add the `add_repo` / `remove_repo` / `set_workspace` methods to `LainServer`**

These delegate to the existing federation engine (`FederatedIndex::add_repo`, etc.) and the `WorkspaceIndex`.

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test --lib server::reload::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(reload): rebuild task with diff + atomic swap"
```

---

### Task 6.3: Extend the file watcher

**Files:**
- Modify: `src/server/watcher.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn watcher_triggers_reload_on_workspaces_yaml_change() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspaces.yaml");
    std::fs::write(&ws, "workspaces: []").unwrap();
    let bus = ReloadBus::new();
    let mut sub = bus.subscribe();
    spawn_watcher(&ws, &bus);
    // Wait, then mutate.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    std::fs::write(&ws, "workspaces:\n  - name: w1\n    members: [r1]\n").unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await.is_ok());
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib server::watcher::tests 2>&1 | head -20
```

Expected: FAIL.

- [ ] **Step 3: Add `repos.yaml` and `workspaces.yaml` to the watch list**

In `src/server/watcher.rs`:

```rust
fn watch_paths_for_config(repos_yaml: &Path) -> Vec<PathBuf> {
    let mut paths = vec![repos_yaml.to_path_buf()];
    let parent = repos_yaml.parent().unwrap_or(Path::new("."));
    let ws = parent.join("workspaces.yaml");
    if ws.exists() {
        paths.push(ws);
    }
    paths
}
```

Modify the existing `start_watcher` to call `watch_paths_for_config` and pass `&bus` so a change triggers `bus.request_reload()`.

- [ ] **Step 4: Run the tests**

```bash
cargo test --lib server::watcher::tests
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(watcher): watch repos.yaml and workspaces.yaml"
```

---

### Task 6.4: Add the Unix socket signal path

**Files:**
- Modify: `src/server/server/mod.rs`
- Modify: `src/cli/signal.rs` (new)

**Interfaces:**
- Consumes: `config_path`
- Produces: `signal_reload(config_path) -> Result<()>` (CLI side); `spawn_signal_listener(bus: &ReloadBus) -> Result<()>` (server side)

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn signal_listener_forwards_to_bus() {
    let bus = ReloadBus::new();
    let mut sub = bus.subscribe();
    let listener = spawn_signal_listener_at("/tmp/lain-test.sock", &bus).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // Send a "reload" message via Unix socket.
    let mut s = tokio::net::UnixStream::connect("/tmp/lain-test.sock").await.unwrap();
    s.write_all(b"reload\n").await.unwrap();
    drop(s);
    assert!(tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv()).await.is_ok());
    drop(listener);
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --lib cli::signal::tests 2>&1 | head -20
```

Expected: FAIL.

- [ ] **Step 3: Implement the socket listener**

```rust
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;

pub fn socket_path_for(config_path: &Path) -> std::path::PathBuf {
    let stem = config_path.file_stem().unwrap_or_else(|| std::ffi::OsStr::new("default"));
    let dir = crate::config::run_dir();
    dir.join(format!("{}.sock", stem.to_string_lossy()))
}

pub async fn spawn_signal_listener(path: &Path, bus: &ReloadBus) -> Result<std::path::PathBuf, std::io::Error> {
    let dir = path.parent().unwrap();
    tokio::fs::create_dir_all(dir).await?;
    let _ = tokio::fs::remove_file(path).await;
    let listener = UnixListener::bind(path)?;
    let path = path.to_path_buf();
    let bus = bus.clone_handle();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((mut s, _)) => {
                    let mut buf = [0u8; 32];
                    if let Ok(n) = s.read(&mut buf).await {
                        if &buf[..n] == b"reload\n" {
                            let _ = bus.request_reload();
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
    Ok(path)
}
```

(Note: `bus.clone_handle()` requires extending `ReloadBus` with a `Clone`-friendly handle. Implement using `Arc<ReloadBus>` or a `tokio::sync::mpsc` indirection.)

- [ ] **Step 4: Implement the CLI side**

```rust
pub fn signal_reload(config_path: &Path) -> Result<()> {
    let sock = socket_path_for(config_path);
    if !sock.exists() {
        return Ok(()); // server not running; YAML is already saved by the caller
    }
    let mut s = std::os::unix::net::UnixStream::connect(&sock)?;
    use std::io::Write;
    s.write_all(b"reload\n")?;
    Ok(())
}
```

- [ ] **Step 5: Hook it into the CLI subcommands**

In `src/cli/workspaces.rs` and `src/cli/repos.rs`, after each `write_atomic(...)` call, call `signal_reload(config_path)`.

- [ ] **Step 6: Run the tests**

```bash
cargo test --lib cli::signal::tests
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(reload): Unix socket signal path for CLI -> server"
```

---

### Task 6.5: Add `get_reload_status` and `request_reload` MCP tools

**Files:**
- Modify: `src/server/mcp/federation_tools.rs`
- Modify: `src/server/mcp/handler.rs`

- [ ] **Step 1: Implement the tools**

```rust
pub fn get_reload_status_schema() -> ToolSchema {
    ToolSchema::new("get_reload_status")
        .description("Returns the current reload status: state, started_at, last_reload_at, last_error, pending_changes.")
}

pub fn run_get_reload_status(bus: &ReloadBus) -> Result<serde_json::Value, LainError> {
    let s = bus.status();
    Ok(json!({
        "state": match s.state {
            ReloadState::Idle => "idle",
            ReloadState::Rebuilding => "rebuilding",
            ReloadState::Failed(_) => "failed",
        },
        "started_at": s.started_at.map(|t| t.to_rfc3339()),
        "last_reload_at": s.last_reload_at.map(|t| t.to_rfc3339()),
        "last_error": s.last_error,
        "pending_changes": s.pending_changes,
    }))
}

pub fn request_reload_schema() -> ToolSchema {
    ToolSchema::new("request_reload").description("Trigger a rebuild of repos.yaml/workspaces.yaml.")
}

pub fn run_request_reload(bus: &ReloadBus) -> Result<serde_json::Value, LainError> {
    bus.request_reload().map_err(|e| LainError::Other(e))?;
    Ok(json!({"accepted": true, "message": "reload scheduled"}))
}
```

- [ ] **Step 2: Register them in `src/server/mcp/handler.rs`**

- [ ] **Step 3: Surface the status in the command center**

In `app.js`, add:

```js
async function renderReloadStatus() {
  const s = await mcpCall('get_reload_status');
  const el = document.getElementById('status-reload');
  el.textContent = `reload: ${s.state}`;
  if (s.state === 'rebuilding') {
    el.classList.add('rebuilding');
  }
}
setInterval(renderReloadStatus, 1000);
```

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(mcp): get_reload_status + request_reload tools"
```

---

### Task 6.6: Add integration tests

**Files:**
- Create: `tests/hot_reload.rs`

- [ ] **Step 1: Write the integration test**

```rust
use lain::server::{LainServer, Transport};
use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

#[tokio::test(flavor = "multi_thread")]
async fn add_repo_to_workspace_is_visible_to_list_repos() {
    let tmp = tempdir().unwrap();
    let repos = tmp.path().join("repos.yaml");
    let ws = tmp.path().join("workspaces.yaml");
    std::fs::write(&repos, "repos: []").unwrap();
    std::fs::write(&ws, "workspaces: []").unwrap();

    let config = lain::server::LainConfig {
        workspace: tmp.path().to_path_buf(),
        memory_path: tmp.path().join(".lain/graph.bin"),
        repos_yaml: repos.clone(),
        active_workspace: None,
    };
    let server = LainServer::new(&config, &config.memory_path, None).unwrap();
    let bus = server.reload_bus().clone();

    // Spawn the server.
    let server_task = tokio::spawn(async move { server.serve_for_test().await });

    // Add a repo to the workspace via the CLI signal path.
    std::fs::write(&ws, "workspaces:\n  - name: w1\n    members: [r1]\n").unwrap();
    lain::cli::signal::signal_reload(&repos).unwrap();

    // Wait for the rebuild.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let status = bus.status();
    assert_eq!(status.state, lain::server::reload::ReloadState::Idle);

    server_task.abort();
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --test hot_reload 2>&1 | tail -20
```

Expected: FAIL (helpers don't exist).

- [ ] **Step 3: Add the missing helpers**

- `LainServer::reload_bus()` accessor.
- `LainServer::serve_for_test()` (a wrapper that runs the rebuild loop without an HTTP listener).
- `cli::signal::signal_reload` (from Task 6.4).

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test --test hot_reload
```

Expected: PASS.

- [ ] **Step 5: Write `tests/hot_reload_remove.rs`**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn remove_repo_from_workspace_makes_it_invisible_to_list_repos() {
    // ... mirrored from the add test, but with a "remove" step ...
}
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "test(hot-reload): add integration tests for add/remove repo"
```

---

### Task 6.7: Document hot-reload

**Files:**
- Create: `docs/hot-reload.md`

- [ ] **Step 1: Write the doc**

```markdown
# Hot Reload

The `lain server` watches `repos.yaml` and `workspaces.yaml` and rebuilds its federation state when they change. No restart needed.

## What is hot-reloaded

- Adding a repo to `repos.yaml` and then adding it to a workspace: visible in `list_repos` within seconds.
- Removing a repo from `workspaces.yaml`: disappears from `list_repos` and `get_cross_repo_blast_radius`.
- Hand-editing `repos.yaml`: the file watcher triggers the reload.

## What is NOT hot-reloaded

- Switching the active workspace (`lain workspaces use <name>`). The new workspace is set in `~/.config/lain/active_workspace` and picked up on the next server restart.
- Changing `--embedding-model`. The embedder is loaded once at startup.
- Changing `--transport` or `--port`. Restart with the new flags.

## Observability

- `get_reload_status` MCP tool: returns `{state: Idle|Rebuilding|Failed, started_at, last_reload_at, last_error, pending_changes}`.
- `request_reload` MCP tool: triggers a rebuild.
- The Command Center status bar shows the reload state live.
```

- [ ] **Step 2: Commit**

```bash
git add docs/hot-reload.md
git commit -m "docs(hot-reload): initial walkthrough"
```

---

### Task 6.8: Verify PR 6 end-to-end

- [ ] **Step 1: Run the full test suite**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: PASS.

- [ ] **Step 2: Manual end-to-end smoke test**

```bash
# Terminal 1
cargo run --quiet -- server --config ./tests/e2e/fixtures/repos.yaml --transport http --port 9999

# Terminal 2
cargo run --quiet -- workspaces add myws --repo new-repo
# In Terminal 1, the command center status bar should briefly show "reload: rebuilding" then "reload: idle".
# `list_repos` should now include new-repo.
```

- [ ] **Step 3: Commit any fixups**

```bash
git add -A
git commit -m "fix: PR6 fixups" --allow-empty
```

---

## Self-Review

**Spec coverage:** Walked through the spec's acceptance criteria 1–16. Each maps to one or more tasks:
- AC 1 (single binary) → PR 1 (Task 1.1–1.9).
- AC 2 (server starts) → PR 1 (Task 1.11).
- AC 3 (workspace scoped) → PR 1 (Task 1.10) + PR 4.
- AC 4 (multi-project) → PR 1 (Task 1.8).
- AC 5 (workspaces CLI) → PR 1 (Task 1.9) + PR 6 (Task 6.4).
- AC 6 (repos CLI) → PR 3 (Task 3.1–3.2).
- AC 7 (query CLI) → PR 1 (Task 1.9).
- AC 8 (ask CLI) → PR 1 (Task 1.9).
- AC 9 (1-repo workspace) → PR 1 (Task 1.10).
- AC 10 (subcommands removed) → PR 2 (Task 2.1–2.3).
- AC 11 (tool surface) → PR 1 + PR 4 (Task 4.1–4.2) + PR 6 (Task 6.5).
- AC 12 (regression tests) → PR 1 (Task 1.11) + PR 2 (Task 2.6) + PR 5 (Task 5.3).
- AC 13 (command center) → PR 4 (Task 4.3–4.10).
- AC 14 (hot-reload) → PR 6 (Task 6.1–6.8).
- AC 15 (docs) → PR 5 (Task 5.2).
- AC 16 (no multi-user modules) → PR 2 (Task 2.1–2.4).

**Placeholder scan:** No "TBD" / "TODO" / "similar to" in the plan. (The `todo!()` macros in early steps are intentional — they're the failing-test-first pattern.)

**Type consistency:** `ReloadBus`, `ReloadStatus`, `ReloadState`, `ReloadSubscriber` are defined in Task 6.1 and used verbatim in Tasks 6.2, 6.3, 6.4, 6.5, 6.6. `LainConfig` carries `repos_yaml` and `active_workspace` from Task 4.2 onward. `Repositories` and `repos_yaml` are consistent across CLI and server.

**No gaps found.** Plan is ready.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-14-lain-consolidation.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
