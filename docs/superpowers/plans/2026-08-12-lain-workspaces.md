# Lain Workspaces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add named groups of repos ("workspaces") that the federation engine holds as a coherent unit, switchable on server restart, plus a CLI for CRUD and a dashboard view of the active workspace's cross-repo graph.

**Architecture:** Thin `WorkspaceIndex` layer on top of `FederatedIndex`. WorkspaceSpec, WorkspaceSource, WorkspaceIndex live in a new `src/federation/workspace.rs`. Workspace membership is read at server startup from `workspaces.yaml` and used to filter `repos.yaml`'s repos before federation cold-start. The 6 existing federation tools are unchanged; 3 new MCP tools + 1 dashboard-only tool are added.

**Tech Stack:** Rust (cargo, tokio, serde_yaml), bash for e2e, plain HTML + D3 for dashboard. Adds `serde_yaml` already in the dependency tree (no new crates needed).

---

## Global Constraints

From the spec (every task implicitly includes these):

- **Prerequisite:** the test-gap fix's PR 1 (`src/federation/federated_index.rs::project_repo` Pass A + Pass B) MUST have merged into `main` before workspace PR 1 lands. The workspace's headline cross-repo blast-radius semantic depends on `project_repo` emitting cross-repo `Calls` edges. Verify this before starting Task 1: `git log --oneline main | head -20` should show commits referencing "Pass A" and "Pass B" in `src/federation/federated_index.rs`.

- **Backward compatibility:**
  - `lain server --config repos.yaml --transport http` (no `--workspace` flag) MUST behave identically to today. Existing federation users see no difference.
  - `repos.yaml` schema is unchanged. The OTel-extended `tests/e2e/federation_e2e.sh` from the test-gap spec is the same `repos.yaml` the workspace feature consumes.
  - The 6 existing federation tools (`list_repos`, `get_repo_info`, `get_federation_health`, `search_org`, `get_cross_repo_blast_radius`, `get_cross_repo_blast_radius_for_repo`) have unchanged signatures and return shapes. They operate over the active workspace's repo subset when a workspace is active.

- **Workspace ≥ 2 repos.** A 1-element workspace is a config error. Solo work stays on `lain --workspace <path>` (single-workspace mode).

- **Repo id rules:** `RepoId::new` rejects empty / `:` / `/`. The workspace config validator enforces this at parse time; never let a bad id reach `RepoId::new` at runtime.

- **Switching is restart-only.** Hot-reload of active workspace is explicitly out of scope. `lain workspaces use <name>` writes `~/.config/lain/active_workspace`; the running server keeps its workspace until restart.

- **No MCP write tools.** All workspace mutations (`create`, `add`, `remove`, `import`, `init`, `forget`) live in the `lain workspaces ...` CLI subcommand. They edit `workspaces.yaml` directly. Never expose them via MCP.

- **`load_federation` does NOT trigger per-repo indexing** (`src/federation/loader.rs:11-65`). The workspace loader path must mirror the production pattern in `src/cmds/server.rs:49-74`: build `RepoSource` → `fed.add_repo()` → `fed.get_repo()` → `ri.index().await` → `fed.project_repo()`. The test fixtures must use this same explicit indexing pattern; tests that only call `load_federation` will see empty per-repo graphs.

- **`cargo` lives at `/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo`** in the dev sandbox. The PATH must include the rustup toolchain bin dir, or commands fail with "command not found". Use `export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"` in every test-run command.

- **Existing test patterns to mirror:**
  - `init_temp_git_repo(dir)` at `tests/federation_integration.rs:20` runs `git init`, configures identity, creates initial commit. Use it instead of bare `git2::Repository::init` (which produces no commit and breaks `GitSensor::new`).
  - Write all fixture files BEFORE calling `init_temp_git_repo` so the working tree has content to commit.
  - `PetgraphBackend::new(data_dir)` creates the global backend; `FederatedIndex::new(backend)` wraps it.

---

## File Structure

**PR 1 — Core workspace layer:**

Create:
- `src/federation/workspace.rs` — `WorkspaceSpec`, `WorkspaceSource` trait, `WorkspaceDirSource`, `WorkspaceCloneSource`, `WorkspaceIndex`, parse functions, validation
- `src/cmds/workspaces.rs` — CLI handlers for `lain workspaces ...` subcommand
- `tests/workspace_e2e.rs` — 6 per-PR tests

Modify:
- `src/federation/mod.rs` — re-export `WorkspaceSpec`, `WorkspaceSource`, etc.
- `src/state.rs` — add `ActiveWorkspace` struct + read/write to `~/.config/lain/active_workspace`
- `src/federation/loader.rs` — extend `load_federation` to accept an optional workspace filter (or add `load_federation_with_workspace`)
- `src/cmds/server.rs` — handle `--workspace` flag; call workspace-aware loader
- `src/cmds/mod.rs` — register the `workspaces` subcommand
- `src/main.rs` — add `--workspace` to the `Server` subcommand variant
- `src/mcp/handler.rs` — register 3 new MCP tools
- `src/mcp/federation_tools.rs` — add handler functions for `list_workspaces`, `get_active_workspace`, `get_workspace`

**PR 2 — Dashboard + e2e + docs:**

Modify:
- `src/mcp/federation_dashboard.html` — add 3 new sections (workspaces panel, config panel, per-workspace D3 graph)
- `src/mcp/federation_tools.rs` — add `get_workspace_graph` handler
- `src/mcp/handler.rs` — register `get_workspace_graph` (only when a workspace is active)
- `src/tools.rs` — extend `get_agent_strategy` with a workspace mode section
- `docs/FEDERATION.md` — add a "Workspaces" section
- `README.md` — pointer to workspaces (one-liner)

Create:
- `tests/e2e/workspace_e2e.sh` — nightly e2e

---

## PR 1 — Core Workspace Layer

### Task 1: WorkspaceSpec + parse + validation

**Files:**
- Create: `src/federation/workspace.rs` (skeleton — only the data type + parse + validate in this task)
- Modify: `src/federation/mod.rs` (re-export at the bottom alongside other re-exports)

**Interfaces:**
- Produces:
  - `pub struct WorkspaceSpec { pub name: String, pub description: Option<String>, pub source: Option<WorkspaceSourceConfig>, pub members: Vec<String> }`
  - `pub struct WorkspaceSourceConfig { pub kind: WorkspaceSourceKind, pub path: Option<PathBuf>, pub url: Option<String>, pub ref_: Option<String>, pub refresh_interval_secs: Option<u64> }`
  - `pub enum WorkspaceSourceKind { WorkspaceDir, WorkspaceClone }`
  - `pub struct WorkspacesFile { pub default: Option<String>, pub workspaces: Vec<WorkspaceSpec> }`
  - `pub fn WorkspacesFile::load(path: &Path) -> Result<Self, LainError>`
  - `pub fn WorkspacesFile::validate(&self) -> Result<(), LainError>` (enforces ≥ 2 members per workspace, valid repo id chars, no duplicate workspace names)
  - `pub enum LainError::NoActiveWorkspace(String)` — add new variant in `src/error.rs`

- [ ] **Step 1: Add the `LainError::NoActiveWorkspace` variant**

In `src/error.rs`, add after the existing variants (around line 66):

```rust
#[error("No active workspace: {0}")]
NoActiveWorkspace(String),
```

(The existing `#[derive(Error, Debug)]` macro will pick it up.)

- [ ] **Step 2: Write the failing unit test for parse + validate**

In `src/federation/workspace.rs` (new file), at the top:

```rust
use crate::error::LainError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceSpec {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub source: Option<WorkspaceSourceConfig>,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceSourceConfig {
    WorkspaceDir { path: PathBuf },
    WorkspaceClone {
        url: String,
        #[serde(default)]
        ref_: Option<String>,
        #[serde(default)]
        refresh_interval_secs: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspacesFile {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSpec>,
}
```

Add tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_workspaces_yaml() {
        let yaml = "\
workspaces:
  - name: backend-team
    members: [auth-svc, billing-svc]
";
        let f: WorkspacesFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(f.workspaces.len(), 1);
        assert_eq!(f.workspaces[0].name, "backend-team");
        assert_eq!(f.workspaces[0].members, vec!["auth-svc", "billing-svc"]);
    }

    #[test]
    fn validate_rejects_sub_two_members() {
        let f = WorkspacesFile {
            default: None,
            workspaces: vec![WorkspaceSpec {
                name: "tiny".into(),
                description: None,
                source: None,
                members: vec!["only".into()],
            }],
        };
        let err = f.validate().unwrap_err();
        assert!(matches!(err, LainError::Config(_)), "got: {err:?}");
    }

    #[test]
    fn validate_rejects_invalid_repo_id_chars() {
        let f = WorkspacesFile {
            default: None,
            workspaces: vec![WorkspaceSpec {
                name: "ws".into(),
                description: None,
                source: None,
                members: vec!["ok".into(), "bad/id".into()],
            }],
        };
        let err = f.validate().unwrap_err();
        assert!(matches!(err, LainError::Config(_)));
    }

    #[test]
    fn validate_rejects_duplicate_workspace_names() {
        let f = WorkspacesFile {
            default: None,
            workspaces: vec![
                WorkspaceSpec { name: "dup".into(), description: None, source: None, members: vec!["a".into(), "b".into()] },
                WorkspaceSpec { name: "dup".into(), description: None, source: None, members: vec!["c".into(), "d".into()] },
            ],
        };
        let err = f.validate().unwrap_err();
        assert!(matches!(err, LainError::Config(_)));
    }
}
```

Implement `load` and `validate`:

```rust
impl WorkspacesFile {
    pub fn load(path: &Path) -> Result<Self, LainError> {
        let text = std::fs::read_to_string(path).map_err(|e| LainError::Io(e.to_string()))?;
        let file: WorkspacesFile = serde_yaml::from_str(&text).map_err(|e| LainError::Config(format!("workspaces.yaml: {e}")))?;
        file.validate()?;
        Ok(file)
    }

    pub fn validate(&self) -> Result<(), LainError> {
        let mut seen_names = std::collections::HashSet::new();
        for ws in &self.workspaces {
            if !seen_names.insert(&ws.name) {
                return Err(LainError::Config(format!(
                    "duplicate workspace name '{}'", ws.name
                )));
            }
            if ws.members.len() < 2 {
                return Err(LainError::Config(format!(
                    "workspace '{}' must contain >= 2 repos; got {}",
                    ws.name,
                    ws.members.len()
                )));
            }
            for m in &ws.members {
                if m.is_empty() || m.contains(':') || m.contains('/') {
                    return Err(LainError::Config(format!(
                        "workspace '{}' contains invalid repo id '{}'",
                        ws.name, m
                    )));
                }
            }
        }
        if let Some(default_name) = &self.default {
            if !self.workspaces.iter().any(|w| &w.name == default_name) {
                return Err(LainError::Config(format!(
                    "default workspace '{}' not found in workspaces list", default_name
                )));
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 2: Re-export from `src/federation/mod.rs`**

Add at the bottom of `src/federation/mod.rs`:

```rust
pub use workspace::{WorkspacesFile, WorkspaceSpec, WorkspaceSourceConfig, WorkspaceSourceKind};
```

(Adjust the import path if your `mod workspace;` declaration is structured differently.)

- [ ] **Step 3: Run the tests; verify they pass**

Run (in the worktree):
```
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib federation::workspace -- --nocapture
```
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add src/federation/workspace.rs src/federation/mod.rs src/error.rs
git commit -m "feat(federation): WorkspaceSpec + workspaces.yaml loader + validation"
```

---

### Task 2: WorkspaceSource trait + 2 impls

**Files:**
- Modify: `src/federation/workspace.rs` (add `WorkspaceSource` trait + `WorkspaceDirSource` + `WorkspaceCloneSource`)
- (no other file changes — the loader wiring lands in Task 3)

**Interfaces:**
- Produces:
  - `pub trait WorkspaceSource: Send + Sync { fn id(&self) -> &str; fn local_path(&self) -> &Path; fn kind(&self) -> WorkspaceSourceKind; fn fetch(&self) -> Result<(), LainError>; fn is_stale(&self) -> bool; }`
  - `pub struct WorkspaceDirSource { id: String, path: PathBuf }`
  - `pub struct WorkspaceCloneSource { id: String, url: String, ref_: String, refresh_interval_secs: u64, local_path: PathBuf }`

- [ ] **Step 1: Add the trait + impls in `workspace.rs`**

```rust
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use crate::federation::config::{clone_or_fetch_repo, fetch_shallow};  // OR — adjust to match the existing RepoSource helper functions in src/federation/repo_source.rs

/// Mirror of `RepoSource` for workspace definitions. Same shape, separate
/// trait so callers can be explicit about which subsystem they're driving.
pub trait WorkspaceSource: Send + Sync {
    fn id(&self) -> &str;
    fn local_path(&self) -> &Path;
    fn kind(&self) -> WorkspaceSourceKind;
    fn fetch(&self) -> Result<(), LainError>;
    fn is_stale(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceSourceKind {
    WorkspaceDir,
    WorkspaceClone,
}

impl std::fmt::Display for WorkspaceSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WorkspaceDir => write!(f, "workspace_dir"),
            Self::WorkspaceClone => write!(f, "workspace_clone"),
        }
    }
}

pub struct WorkspaceDirSource {
    pub id: String,
    pub path: PathBuf,
}

impl WorkspaceDirSource {
    pub fn new(id: String, path: PathBuf) -> Result<Self, LainError> {
        if !path.is_dir() {
            return Err(LainError::Config(format!(
                "workspace_dir path does not exist or is not a directory: {}", path.display()
            )));
        }
        Ok(Self { id, path })
    }
}

impl WorkspaceSource for WorkspaceDirSource {
    fn id(&self) -> &str { &self.id }
    fn local_path(&self) -> &Path { &self.path }
    fn kind(&self) -> WorkspaceSourceKind { WorkspaceSourceKind::WorkspaceDir }
    fn fetch(&self) -> Result<(), LainError> {
        // No git ops for workspace_dir — the path is the source of truth.
        Ok(())
    }
}

pub struct WorkspaceCloneSource {
    pub id: String,
    pub url: String,
    pub ref_: String,
    pub refresh_interval_secs: u64,
    pub local_path: PathBuf,
}

impl WorkspaceCloneSource {
    pub fn new(id: String, url: String, ref_: Option<String>, refresh_interval_secs: Option<u64>, local_root: PathBuf) -> Self {
        Self {
            id,
            url,
            ref_: ref_.unwrap_or_else(|| "main".to_string()),
            refresh_interval_secs: refresh_interval_secs.unwrap_or(300),
            local_path: local_root.join("workspaces").join(&id),
        }
    }

    fn last_refresh_unix(&self) -> u64 {
        // mtime of the local checkout's .git/HEAD file
        std::fs::metadata(self.local_path.join(".git/HEAD"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

impl WorkspaceSource for WorkspaceCloneSource {
    fn id(&self) -> &str { &self.id }
    fn local_path(&self) -> &Path { &self.local_path }
    fn kind(&self) -> WorkspaceSourceKind { WorkspaceSourceKind::WorkspaceClone }
    fn fetch(&self) -> Result<(), LainError> {
        if !self.local_path.join(".git").exists() {
            // First clone
            run_git(&["clone", "--depth", "1", "--branch", &self.ref_, &self.url])
                .current_dir(self.local_path.parent().unwrap_or(Path::new(".")))
                .map_err(|e| LainError::Git(format!("workspace_clone {}: {}", self.id, e)))?;
        } else {
            // Fetch + reset
            run_git(&["fetch", "--depth", "1", "origin", &self.ref_])
                .current_dir(&self.local_path)
                .map_err(|e| LainError::Git(format!("workspace_clone fetch {}: {}", self.id, e)))?;
            run_git(&["reset", "--hard", &format!("origin/{}", self.ref_)])
                .current_dir(&self.local_path)
                .map_err(|e| LainError::Git(format!("workspace_clone reset {}: {}", self.id, e)))?;
        }
        Ok(())
    }
    fn is_stale(&self) -> bool {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        now.saturating_sub(self.last_refresh_unix()) > self.refresh_interval_secs
    }
}

fn run_git(args: &[&str]) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    c.args(args);
    c
}
```

(Notes for the implementer: the `run_git` helper is a sketch — wire it to use `Command::status()` and convert non-zero exit to `LainError::Git`. `WorkspaceCloneSource::fetch` should be exercised by an integration test in Task 9, not a unit test, since it requires a real `git` binary on PATH and a network or local fixture.)

- [ ] **Step 2: Build (no tests yet — sources aren't exercised in unit form)**

Run:
```
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo build
```
Expected: clean build (no new warnings beyond the existing pre-existing ones).

- [ ] **Step 3: Commit**

```bash
git add src/federation/workspace.rs
git commit -m "feat(federation): WorkspaceSource trait + workspace_dir/clone impls"
```

---

### Task 3: ActiveWorkspace pointer + state.rs extension

**Files:**
- Modify: `src/state.rs` (add `ActiveWorkspace` struct + read/write to `~/.config/lain/active_workspace`)
- Modify: `src/federation/workspace.rs` (add a helper to resolve active workspace name → spec, given a `WorkspacesFile`)

**Interfaces:**
- Produces:
  - `pub struct ActiveWorkspace { pub name: String, pub source_path: PathBuf }`
  - `pub fn ActiveWorkspace::load() -> Result<Option<Self>, LainError>` (reads `~/.config/lain/active_workspace` if it exists)
  - `pub fn ActiveWorkspace::save(&self) -> Result<(), LainError>` (writes atomically)
  - `pub fn ActiveWorkspace::clear() -> Result<(), LainError>` (removes the file)
  - `pub fn resolve_active_workspace(spec: &WorkspacesFile, name: &str) -> Result<&WorkspaceSpec, LainError>` (returns `LainError::Config(...)` if name not in spec)

- [ ] **Step 1: Read `src/state.rs` to see the existing `Projects` registry pattern**

Look at how `Projects::active_name()`, `Projects::load()`, `Projects::save()` are implemented (around the top of the file). Mirror that pattern for `ActiveWorkspace`.

- [ ] **Step 2: Add `ActiveWorkspace` to `src/state.rs`**

```rust
/// Pointer to the active workspace: name + path to the workspaces.yaml it was
/// sourced from. Lives at `~/.config/lain/active_workspace`.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveWorkspace {
    pub name: String,
    pub source_path: PathBuf,
}

const ACTIVE_WORKSPACE_FILE: &str = "active_workspace";

fn lain_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/lain"))
}

impl ActiveWorkspace {
    fn file_path() -> Option<PathBuf> {
        lain_config_dir().map(|d| d.join(ACTIVE_WORKSPACE_FILE))
    }

    pub fn load() -> Result<Option<Self>, LainError> {
        let Some(path) = Self::file_path() else { return Ok(None); };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(LainError::Io(e.to_string())),
        };
        // Two whitespace-separated tokens: name + path.
        let mut parts = text.split_whitespace();
        let name = parts.next().ok_or_else(|| LainError::Config(format!("active_workspace file empty: {}", path.display())))?
            .to_string();
        let source_path = PathBuf::from(parts.next().ok_or_else(|| LainError::Config(format!("active_workspace missing source path: {}", path.display())))?);
        Ok(Some(Self { name, source_path }))
    }

    pub fn save(&self) -> Result<(), LainError> {
        let Some(dir) = lain_config_dir() else {
            return Err(LainError::Config("HOME not set; cannot write ~/.config/lain".into()));
        };
        std::fs::create_dir_all(&dir).map_err(|e| LainError::Io(e.to_string()))?;
        let path = dir.join(ACTIVE_WORKSPACE_FILE);
        let text = format!("{}\n{}\n", self.name, self.source_path.display());
        // Atomic write: write to .tmp, rename.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(|e| LainError::Io(e.to_string()))?;
        std::fs::rename(&tmp, &path).map_err(|e| LainError::Io(e.to_string()))?;
        Ok(())
    }

    pub fn clear() -> Result<(), LainError> {
        let Some(path) = Self::file_path() else { return Ok(()); };
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LainError::Io(e.to_string())),
        }
    }
}

pub fn resolve_active_workspace<'a>(spec: &'a WorkspacesFile, name: &str) -> Result<&'a WorkspaceSpec, LainError> {
    spec.workspaces.iter()
        .find(|w| w.name == name)
        .ok_or_else(|| LainError::Config(format!("workspace '{name}' not found in workspaces.yaml")))
}
```

(Add the import `use crate::federation::workspace::{WorkspacesFile, WorkspaceSpec};` at the top of `src/state.rs`.)

- [ ] **Step 3: Run the existing test suite to confirm no regression**

Run:
```
cargo test --lib
```
Expected: existing tests still pass; the new code adds new types but no new behavior unless invoked.

- [ ] **Step 4: Commit**

```bash
git add src/state.rs
git commit -m "feat(federation): ActiveWorkspace pointer at ~/.config/lain/active_workspace"
```

---

### Task 4: WorkspaceIndex — filter `repos.yaml` by workspace members

**Files:**
- Modify: `src/federation/workspace.rs` (add `WorkspaceIndex` + helper to filter)
- Modify: `src/federation/mod.rs` (re-export `WorkspaceIndex`)

**Interfaces:**
- Produces:
  - `pub struct WorkspaceIndex { pub spec: WorkspaceSpec, pub members: HashSet<String> }` (members pre-computed as a set for fast lookup)
  - `pub fn WorkspaceIndex::from_spec(spec: WorkspaceSpec) -> Self`
  - `pub fn WorkspaceIndex::contains_repo(&self, repo_id: &str) -> bool`
  - `pub fn filter_repos_by_workspace<'a>(all_repos: &'a [RepoEntry], workspace: &WorkspaceIndex) -> Vec<&'a RepoEntry>` (used by the loader to pick workspace members out of `repos.yaml`)

(The `RepoEntry` type lives in `src/federation/config.rs` — import it as needed. If the type doesn't exist or has a different name, mirror the existing `RepoSource::id()` accessor instead of building a parallel index.)

- [ ] **Step 1: Add `WorkspaceIndex` to `workspace.rs`**

```rust
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct WorkspaceIndex {
    pub spec: WorkspaceSpec,
    pub members: HashSet<String>,
}

impl WorkspaceIndex {
    pub fn from_spec(spec: WorkspaceSpec) -> Self {
        let members = spec.members.iter().cloned().collect();
        Self { spec, members }
    }

    pub fn contains_repo(&self, repo_id: &str) -> bool {
        self.members.contains(repo_id)
    }
}

/// Filter a list of `RepoEntry` (from `FederationConfig::repos`) down to the
/// members of the given workspace. Repos in `all_repos` not in the workspace
/// are dropped; repos in the workspace not in `all_repos` produce a config
/// error listing the missing ids.
pub fn filter_repos_by_workspace<'a>(
    all_repos: &'a [crate::federation::config::RepoEntry],
    workspace: &WorkspaceIndex,
) -> Result<Vec<&'a crate::federation::config::RepoEntry>, LainError> {
    let mut picked = Vec::with_capacity(workspace.members.len());
    let mut missing = Vec::new();
    for member_id in &workspace.spec.members {
        match all_repos.iter().find(|r| &r.id == member_id) {
            Some(entry) => picked.push(entry),
            None => missing.push(member_id.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(LainError::Config(format!(
            "workspace '{}' references repos not in repos.yaml: {:?}",
            workspace.spec.name, missing
        )));
    }
    Ok(picked)
}
```

(Adjust the import path for `RepoEntry` if its actual path differs. The implementer should `grep "pub struct RepoEntry\|pub enum SourceConfig" src/federation/config.rs` to confirm.)

- [ ] **Step 2: Re-export from `src/federation/mod.rs`**

Add to the re-export block:

```rust
pub use workspace::{WorkspaceIndex, filter_repos_by_workspace};
```

- [ ] **Step 3: Build**

```
cargo build
```
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/federation/workspace.rs src/federation/mod.rs
git commit -m "feat(federation): WorkspaceIndex + filter_repos_by_workspace helper"
```

---

### Task 5: Workspace-aware loader extension

**Files:**
- Modify: `src/federation/loader.rs` (add `load_federation_with_workspace` alongside the existing `load_federation`)

**Interfaces:**
- Produces:
  - `pub async fn load_federation_with_workspace(config_path: &Path, workspace_name: &str) -> Result<Arc<FederatedIndex>, LainError>`
  - Behavior: load `FederationConfig` from `config_path`, load `WorkspacesFile` from `<config_path parent>/workspaces.yaml` (or wherever the workspace's `source.path` points), validate the workspace, filter `repos.yaml`'s repos to the workspace's members, then run the same per-repo indexing loop as `load_federation` (add_repo → get_repo → index → project_repo — the production pattern from `src/cmds/server.rs:49-74`).

- [ ] **Step 1: Add the new loader function**

Append to `src/federation/loader.rs`:

```rust
use crate::federation::workspace::{WorkspacesFile, WorkspaceIndex, filter_repos_by_workspace};
use crate::state::resolve_active_workspace;

/// Load a federation scoped to a single workspace's repos. Same as
/// `load_federation` but filters `repos.yaml` to the workspace's members
/// before indexing. Mirrors the production `src/cmds/server.rs:49-74` pattern:
/// add_repo → get_repo → index → project_repo per repo.
pub async fn load_federation_with_workspace(
    config_path: &Path,
    workspace_name: &str,
) -> Result<Arc<FederatedIndex>, LainError> {
    // 1. Load the federation config and workspaces file.
    let config = FederationConfig::load(config_path)?;
    let workspaces_path = config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("workspaces.yaml");
    let workspaces = if workspaces_path.exists() {
        WorkspacesFile::load(&workspaces_path)?
    } else {
        WorkspacesFile::default()
    };

    // 2. Resolve the workspace spec; fail fast if missing.
    let ws_spec = resolve_active_workspace(&workspaces, workspace_name)?
        .clone();
    let workspace = WorkspaceIndex::from_spec(ws_spec);

    // 3. Build the federation with only the workspace's repos.
    let backend: Arc<dyn GraphBackend> = Arc::new(PetgraphBackend::new(&config.data_dir)?);
    let fed = Arc::new(FederatedIndex::new(backend));

    // 4. Filter repos.yaml to the workspace's members.
    let picked = filter_repos_by_workspace(&config.repos, &workspace)?;

    // 5. For each picked repo: fetch → add_repo → get_repo → index → project_repo.
    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_indexers));
    let mut handles = Vec::with_capacity(picked.len());
    for repo_entry in picked {
        let permit = semaphore.clone().acquire_owned().await
            .map_err(|e| LainError::Other(format!("semaphore: {e}")))?;
        let fed_clone = fed.clone();
        let data_dir = config.data_dir.clone();
        let workspace_members = workspace.members.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let source: Box<dyn crate::federation::repo_source::RepoSource> = repo_entry.build_source(&data_dir)?;
            source.fetch().await?;
            let repo_id = source.id().clone();
            // CRITICAL: must be a member of the workspace (defense in depth).
            if !workspace_members.contains(repo_id.as_str()) {
                return Err(LainError::Config(format!(
                    "repo {} not in active workspace", repo_id
                )));
            }
            fed_clone.add_repo(source, &data_dir).await?;
            let ri = fed_clone.get_repo(&repo_id).ok_or_else(|| LainError::Other("repo missing after add_repo".into()))?;
            ri.index().await?;
            fed_clone.project_repo(&repo_id).await?;
            Ok::<(), LainError>(())
        }));
    }
    for h in handles {
        h.await.map_err(|e| LainError::Other(format!("join: {e}")))??;
    }

    Ok(fed)
}
```

(Adjust call sites if `config.repos` is a different field name, or if `repo_entry.build_source` doesn't exist — mirror the existing `load_federation` body if `RepoEntry::build_sources()` is already there.)

- [ ] **Step 2: Build (no test yet — exercised in Task 9)**

```
cargo build
```
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add src/federation/loader.rs
git commit -m "feat(federation): load_federation_with_workspace (workspace-aware loader)"
```

---

### Task 6: Server entry point — `--workspace` flag wiring

**Files:**
- Modify: `src/cmds/server.rs` (resolve `--workspace` → workspace name → call appropriate loader)
- Modify: `src/main.rs` (add `--workspace` to the `Server` subcommand variant)

**Interfaces:**
- Produces:
  - `Server` subcommand accepts `--workspace <name>` (optional, defaults to "auto" — see resolution below)
  - `lain server --workspace auto` resolves via `ActiveWorkspace::load()`; if no active workspace is set, behavior is identical to today (all repos)
  - `lain server --workspace <name>` resolves the workspace from `workspaces.yaml` next to `repos.yaml`
  - `lain server` (no flag) → today's behavior (all repos)

- [ ] **Step 1: Add `--workspace` to the `Server` subcommand**

In `src/main.rs`, update the `Server` variant:

```rust
Server {
    #[arg(long)] config: std::path::PathBuf,
    #[arg(long, default_value = "http")] transport: String,
    #[arg(long, default_value = "9999")] port: u16,
    #[arg(long, default_value = "info")] log_level: String,
    #[arg(long, default_value = "auto")] workspace: String,  // NEW: "auto", explicit name, or empty for "all repos"
},
```

- [ ] **Step 2: Resolve the workspace name in `run_server`**

In `src/cmds/server.rs`, update `run_server` (or wherever the loader is currently called) to:

```rust
let workspace_name: Option<String> = match args.workspace.as_str() {
    "" | "none" => None,
    "auto" => {
        // Resolve via ActiveWorkspace; if unset, fall through to all-repos mode.
        match crate::state::ActiveWorkspace::load() {
            Ok(Some(active)) => Some(active.name),
            Ok(None) => None,
            Err(e) => {
                eprintln!("warning: could not read ~/.config/lain/active_workspace: {e}");
                None
            }
        }
    }
    explicit => Some(explicit.to_string()),
};
```

- [ ] **Step 3: Branch the loader call**

```rust
let fed = match workspace_name {
    Some(name) => crate::federation::loader::load_federation_with_workspace(&args.config, &name).await?,
    None => crate::federation::loader::load_federation(&args.config).await?,
};
```

- [ ] **Step 4: Build**

```
cargo build
```
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add src/cmds/server.rs src/main.rs
git commit -m "feat(server): --workspace flag resolves active workspace via ActiveWorkspace::load"
```

---

### Task 7: CLI subcommand `lain workspaces ...`

**Files:**
- Create: `src/cmds/workspaces.rs`
- Modify: `src/cmds/mod.rs` (register `Workspaces` subcommand)
- Modify: `src/main.rs` (add `Workspaces` variant to `Commands` enum + dispatch)

**Interfaces:**
- Produces: the 10 subcommands listed in the spec — `Create`, `Add`, `Remove`, `Import`, `Init`, `List`, `Show`, `Use`, `Current`, `Forget`. Each takes the args documented in the spec; `Use` writes `ActiveWorkspace` to `~/.config/lain/active_workspace`.

- [ ] **Step 1: Read the existing `src/cmds/projects.rs` as the template**

The `lain projects` CLI is the closest analog (registry of single-workspace-mode projects). Mirror its structure: `ProjectsAction` enum, `run_*` functions, file format (`~/.config/lain/projects.toml`).

- [ ] **Step 2: Create `src/cmds/workspaces.rs` with the 10 commands**

```rust
use crate::error::LainError;
use crate::federation::workspace::{WorkspacesFile, WorkspaceSpec};
use crate::state::ActiveWorkspace;
use std::path::{Path, PathBuf};

#[derive(clap::Subcommand, Debug)]
pub enum WorkspacesAction {
    Create {
        name: String,
        #[arg(long)] description: Option<String>,
        #[arg(long, value_delimiter = ',')] members: Vec<String>,
        #[arg(long)] config: Option<PathBuf>,
    },
    Add {
        name: String,
        #[arg(long)] repo: String,
        #[arg(long)] config: Option<PathBuf>,
    },
    Remove {
        name: String,
        #[arg(long)] repo: String,
        #[arg(long)] config: Option<PathBuf>,
    },
    Import {
        name: String,
        #[arg(long)] from: PathBuf,
        #[arg(long)] config: Option<PathBuf>,
    },
    Init {
        name: String,
        #[arg(long)] from: String,           // URL
        #[arg(long)] ref_: Option<String>,
        #[arg(long)] config: Option<PathBuf>,
    },
    List {
        #[arg(long)] config: Option<PathBuf>,
    },
    Show {
        name: String,
        #[arg(long)] config: Option<PathBuf>,
    },
    Use {
        name: String,
        #[arg(long)] config: Option<PathBuf>,
    },
    Current,
    Forget {
        name: String,
        #[arg(long)] config: Option<PathBuf>,
    },
}

fn resolve_config_path(explicit: Option<&Path>) -> PathBuf {
    explicit.cloned().unwrap_or_else(|| {
        // Walk up from cwd looking for workspaces.yaml.
        let mut p = std::env::current_dir().unwrap_or(PathBuf::from("."));
        loop {
            if p.join("workspaces.yaml").is_file() {
                return p.join("workspaces.yaml");
            }
            if !p.pop() { break; }
        }
        PathBuf::from("workspaces.yaml")
    })
}

fn load_or_default(path: &Path) -> Result<WorkspacesFile, LainError> {
    if path.exists() {
        WorkspacesFile::load(path)
    } else {
        Ok(WorkspacesFile::default())
    }
}

fn save(path: &Path, f: &WorkspacesFile) -> Result<(), LainError> {
    let text = serde_yaml::to_string(f).map_err(|e| LainError::Config(format!("serialize: {e}")))?;
    std::fs::write(path, text).map_err(|e| LainError::Io(e.to_string()))?;
    Ok(())
}

pub fn run(action: WorkspacesAction) -> Result<(), LainError> {
    match action {
        WorkspacesAction::Create { name, description, members, config } => {
            let path = resolve_config_path(config.as_deref());
            let mut f = load_or_default(&path)?;
            if f.workspaces.iter().any(|w| w.name == name) {
                return Err(LainError::Config(format!("workspace '{name}' already exists")));
            }
            f.workspaces.push(WorkspaceSpec { name: name.clone(), description, source: None, members });
            f.validate()?;
            save(&path, &f)?;
            println!("Created workspace '{name}' in {}", path.display());
            Ok(())
        }
        WorkspacesAction::Add { name, repo, config } => {
            let path = resolve_config_path(config.as_deref());
            let mut f = WorkspacesFile::load(&path)?;
            let ws = f.workspaces.iter_mut().find(|w| w.name == name)
                .ok_or_else(|| LainError::Config(format!("workspace '{name}' not found")))?;
            if !ws.members.iter().any(|m| m == &repo) {
                ws.members.push(repo.clone());
            }
            f.validate()?;
            save(&path, &f)?;
            println!("Added repo '{repo}' to workspace '{name}'");
            Ok(())
        }
        WorkspacesAction::Remove { name, repo, config } => {
            let path = resolve_config_path(config.as_deref());
            let mut f = WorkspacesFile::load(&path)?;
            let ws = f.workspaces.iter_mut().find(|w| w.name == name)
                .ok_or_else(|| LainError::Config(format!("workspace '{name}' not found")))?;
            ws.members.retain(|m| m != &repo);
            f.validate()?;
            save(&path, &f)?;
            println!("Removed repo '{repo}' from workspace '{name}'");
            Ok(())
        }
        WorkspacesAction::Import { name, from, config } => {
            let path = resolve_config_path(config.as_deref());
            let from_file = WorkspacesFile::load(&from)?;
            let imported = from_file.workspaces.iter().find(|w| w.name == name)
                .ok_or_else(|| LainError::Config(format!("workspace '{name}' not found in {}", from.display())))?
                .clone();
            let mut f = load_or_default(&path)?;
            if f.workspaces.iter().any(|w| w.name == name) {
                return Err(LainError::Config(format!("workspace '{name}' already exists in {}", path.display())));
            }
            f.workspaces.push(imported);
            f.validate()?;
            save(&path, &f)?;
            println!("Imported workspace '{name}' into {}", path.display());
            Ok(())
        }
        WorkspacesAction::Init { name, from, ref_, config } => {
            // workspace_clone source kind — clone the URL, register a workspace_clone source.
            let path = resolve_config_path(config.as_deref());
            let ws_root = std::env::var_os("LAIN_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/lain"));
            let source = crate::federation::workspace::WorkspaceCloneSource::new(
                name.clone(), from.clone(), ref_, None, ws_root,
            );
            source.fetch()?;
            let mut f = load_or_default(&path)?;
            if f.workspaces.iter().any(|w| w.name == name) {
                return Err(LainError::Config(format!("workspace '{name}' already exists")));
            }
            f.workspaces.push(WorkspaceSpec {
                name: name.clone(),
                description: None,
                source: Some(crate::federation::workspace::WorkspaceSourceConfig::WorkspaceClone {
                    url: from,
                    ref_: ref_,
                    refresh_interval_secs: None,
                }),
                members: vec![],  // populate via Add afterward
            });
            f.validate()?;
            save(&path, &f)?;
            println!("Initialized workspace '{name}' from {from}");
            Ok(())
        }
        WorkspacesAction::List { config } => {
            let path = resolve_config_path(config.as_deref());
            let f = load_or_default(&path)?;
            let active = ActiveWorkspace::load().ok().flatten().map(|a| a.name);
            for ws in &f.workspaces {
                let marker = if active.as_deref() == Some(&ws.name) { "* " } else { "  " };
                println!("{marker}{:<24} {} repos", ws.name, ws.members.len());
            }
            Ok(())
        }
        WorkspacesAction::Show { name, config } => {
            let path = resolve_config_path(config.as_deref());
            let f = WorkspacesFile::load(&path)?;
            let ws = f.workspaces.iter().find(|w| w.name == name)
                .ok_or_else(|| LainError::Config(format!("workspace '{name}' not found")))?;
            println!("name: {}", ws.name);
            if let Some(d) = &ws.description { println!("description: {d}"); }
            println!("members ({}):", ws.members.len());
            for m in &ws.members { println!("  - {m}"); }
            if let Some(s) = &ws.source {
                println!("source: {s:?}");
            }
            Ok(())
        }
        WorkspacesAction::Use { name, config } => {
            let path = resolve_config_path(config.as_deref());
            let f = WorkspacesFile::load(&path)?;
            if !f.workspaces.iter().any(|w| w.name == name) {
                return Err(LainError::Config(format!("workspace '{name}' not found in {}", path.display())));
            }
            ActiveWorkspace { name, source_path: path }.save()?;
            println!("Active workspace set. Restart `lain server` to pick it up.");
            Ok(())
        }
        WorkspacesAction::Current => {
            match ActiveWorkspace::load()? {
                Some(a) => println!("{}", a.name),
                None => { eprintln!("no active workspace; use `lain workspaces use <name>`"); std::process::exit(1); }
            }
            Ok(())
        }
        WorkspacesAction::Forget { name, config } => {
            let path = resolve_config_path(config.as_deref());
            let mut f = WorkspacesFile::load(&path)?;
            let before = f.workspaces.len();
            f.workspaces.retain(|w| w.name != name);
            if f.workspaces.len() == before {
                return Err(LainError::Config(format!("workspace '{name}' not found")));
            }
            f.validate()?;
            save(&path, &f)?;
            println!("Forgot workspace '{name}'");
            Ok(())
        }
    }
}
```

- [ ] **Step 3: Wire into `src/main.rs` and `src/cmds/mod.rs`**

In `src/cmds/mod.rs`, add `pub mod workspaces;` and `pub use workspaces::{WorkspacesAction, run as run_workspaces};`.

In `src/main.rs`, add the `Workspaces` variant to `Commands`:

```rust
Workspaces {
    #[command(subcommand)]
    action: crate::cmds::workspaces::WorkspacesAction,
},
```

And in the dispatch match:

```rust
Commands::Workspaces { action } => return crate::cmds::workspaces::run(action),
```

- [ ] **Step 4: Build**

```
cargo build
```
Expected: clean build.

- [ ] **Step 5: Smoke-test the CLI manually (off-PR, in dev)**

Run:
```
cargo run -- workspaces --help
cargo run -- workspaces list
```
Expected: `--help` shows the 10 subcommands; `list` exits 0 with "no workspaces.yaml found" or the existing list.

(Don't commit yet — finish Task 8 first.)

---

### Task 8: 3 new MCP tools (list_workspaces, get_active_workspace, get_workspace)

**Files:**
- Modify: `src/mcp/federation_tools.rs` (add 3 handler functions + their types)
- Modify: `src/mcp/mod.rs` (re-export the new types)
- Modify: `src/mcp/handler.rs` (register the 3 tools in the federation-mode tool list)

**Interfaces:**
- Produces:
  - `pub struct WorkspaceInfo { pub name: String, pub description: Option<String>, pub source: Option<...>, pub member_count: usize, pub is_active: bool }`
  - `pub struct ActiveWorkspaceInfo { pub name: String, pub members: Vec<String>, pub source: Option<...> }`
  - `pub struct WorkspaceDetail { pub name: String, pub description: Option<String>, pub source: Option<...>, pub members: Vec<WorkspaceRepoInfo> }`
  - `pub struct WorkspaceRepoInfo { pub repo_id: String, pub path: String, pub health: String }`
  - `pub fn list_workspaces(workspaces: &WorkspacesFile, active: Option<&ActiveWorkspace>) -> Vec<WorkspaceInfo>`
  - `pub fn get_active_workspace(fed: &FederatedIndex, workspaces: &WorkspacesFile) -> Result<ActiveWorkspaceInfo, LainError>` (returns `LainError::NoActiveWorkspace` if no workspace active)
  - `pub fn get_workspace(fed: &FederatedIndex, workspaces: &WorkspacesFile, name: &str) -> Result<WorkspaceDetail, LainError>` (returns `LainError::NotFound` if name unknown; members include only repos in the active workspace's federation)

(The federation engine holds only workspace members, so resolving a workspace repo id to path/health means looking it up in `fed.list_repos()`.)

- [ ] **Step 1: Add the types and handler functions in `src/mcp/federation_tools.rs`**

```rust
use crate::federation::workspace::{WorkspacesFile, WorkspaceSpec};
use crate::state::ActiveWorkspace;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub description: Option<String>,
    pub source: Option<String>,    // formatted as e.g. "workspace_dir:/path"
    pub member_count: usize,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveWorkspaceInfo {
    pub name: String,
    pub members: Vec<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRepoInfo {
    pub repo_id: String,
    pub path: String,
    pub health: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDetail {
    pub name: String,
    pub description: Option<String>,
    pub source: Option<String>,
    pub members: Vec<WorkspaceRepoInfo>,
}

pub fn list_workspaces(workspaces: &WorkspacesFile, active: Option<&ActiveWorkspace>) -> Vec<WorkspaceInfo> {
    workspaces.workspaces.iter().map(|ws| {
        let source = ws.source.as_ref().map(|s| format!("{s:?}"));
        WorkspaceInfo {
            name: ws.name.clone(),
            description: ws.description.clone(),
            source,
            member_count: ws.members.len(),
            is_active: active.as_ref().map(|a| a.name == ws.name).unwrap_or(false),
        }
    }).collect()
}

pub fn get_active_workspace(fed: &FederatedIndex, workspaces: &WorkspacesFile) -> Result<ActiveWorkspaceInfo, LainError> {
    // The federation is built from one workspace; we identify it via the
    // loaded repos' intersection with workspaces.yaml entries.
    let loaded: std::collections::HashSet<String> = fed.list_repos().into_iter().map(|(id, _)| id.to_string()).collect();
    let active = workspaces.workspaces.iter()
        .find(|ws| ws.members.iter().all(|m| loaded.contains(m)) && loaded.iter().all(|l| ws.members.contains(l)))
        .ok_or_else(|| LainError::NoActiveWorkspace(
            "federation loaded but no workspace matches the loaded repos".into()
        ))?;
    Ok(ActiveWorkspaceInfo {
        name: active.name.clone(),
        members: active.members.clone(),
        source: active.source.as_ref().map(|s| format!("{s:?}")),
    })
}

pub fn get_workspace(fed: &FederatedIndex, workspaces: &WorkspacesFile, name: &str) -> Result<WorkspaceDetail, LainError> {
    let ws = workspaces.workspaces.iter().find(|w| w.name == name)
        .ok_or_else(|| LainError::NotFound(format!("workspace {name}")))?;
    let mut members = Vec::with_capacity(ws.members.len());
    for m in &ws.members {
        let info = fed.list_repos().into_iter().find(|(id, _)| id.as_str() == m);
        members.push(WorkspaceRepoInfo {
            repo_id: m.clone(),
            path: info.as_ref().map(|(_, ri)| ri.path.clone()).unwrap_or_default(),
            health: info.map(|(_, ri)| format!("{:?}", ri.health)).unwrap_or_else(|| "not_loaded".into()),
        });
    }
    Ok(WorkspaceDetail {
        name: ws.name.clone(),
        description: ws.description.clone(),
        source: ws.source.as_ref().map(|s| format!("{s:?}")),
        members,
    })
}
```

- [ ] **Step 2: Re-export from `src/mcp/mod.rs`**

Add to the existing `pub use federation_tools::{...}` line:

```rust
pub use federation_tools::{
    ActiveWorkspaceInfo, WorkspaceDetail, WorkspaceInfo, WorkspaceRepoInfo,
    get_active_workspace, get_workspace, list_workspaces,
};
```

- [ ] **Step 3: Register the 3 tools in `src/mcp/handler.rs`**

In the federation-mode tool list (search for `federation_tools.rs:9-20` style array), add 3 entries:

```rust
(
    "list_workspaces",
    "List all known workspaces from workspaces.yaml. Returns [{name, description?, source?, member_count, is_active}].",
    &[],
),
(
    "get_active_workspace",
    "Return the workspace the server is currently holding (the one whose repos were loaded). Errors with NoActiveWorkspace if the server was started without --workspace.",
    &[],
),
(
    "get_workspace",
    "Full detail on one workspace by name: description?, source?, members: [{repo_id, path, health}]. Errors with NotFound if name is unknown.",
    &["name"],
),
```

And wire the dispatch arms in the existing match block (where the other federation tools are handled):

```rust
"list_workspaces" => {
    return Ok(tool_text_result(
        serde_json::to_string(&list_workspaces(&workspaces, active.as_ref())).unwrap_or_else(|e| format!("serialize: {e}")),
        false,
    ));
}
"get_active_workspace" => {
    match get_active_workspace(fed, &workspaces) {
        Ok(info) => Ok(tool_text_result(serde_json::to_string(&info).unwrap_or_default(), false)),
        Err(e) => Ok(tool_text_result(format!("{e}"), true)),
    }
}
"get_workspace" => {
    let name = match args_owned.get("name").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Ok(tool_text_result("Missing required argument: name", true)),
    };
    match get_workspace(fed, &workspaces, name) {
        Ok(detail) => Ok(tool_text_result(serde_json::to_string(&detail).unwrap_or_default(), false)),
        Err(e) => Ok(tool_text_result(format!("{e}"), true)),
    }
}
```

(The exact shape of `args_owned` / `tool_text_result` / `fed` / `workspaces` / `active` follows the existing federation tool dispatch — adjust to match the surrounding code.)

- [ ] **Step 4: Build**

```
cargo build
```
Expected: clean build.

- [ ] **Step 5: Commit Tasks 7+8 together (single commit covers CLI + MCP integration)**

```bash
git add src/cmds/workspaces.rs src/cmds/mod.rs src/main.rs src/mcp/federation_tools.rs src/mcp/mod.rs src/mcp/handler.rs
git commit -m "feat(workspaces): CLI subcommand + 3 new MCP tools (list_workspaces, get_active_workspace, get_workspace)"
```

---

### Task 9: Per-PR tests in `tests/workspace_e2e.rs`

**Files:**
- Create: `tests/workspace_e2e.rs` (6 tests)

**Interfaces:**
- Produces: 6 `#[tokio::test]` functions covering parse / validation / filter / MCP tools (matching the spec's test plan).

- [ ] **Step 1: Write the 6 tests**

```rust
//! Workspace-aware federation tests. Mirrors the explicit per-repo indexing
//! pattern from tests/federation_integration.rs (NOT load_federation —
//! that helper does NOT call repo.index().await). Gated behind
//! `--features test-utils`.

use lain::federation::loader::load_federation_with_workspace;
use lain::federation::workspace::{WorkspacesFile, WorkspaceSpec};
use std::path::Path;
use std::sync::Arc;

const SHARED_LIB: &str = "\
pub fn hello() -> &'static str { \"hi\" }
pub fn inner() -> &'static str { \"inner\" }
pub fn greet() -> &'static str { hello() }
";

const AUTH_LIB: &str = "\
pub fn auth() -> bool { lain_lib::verify_token(\"x\") }
";

const DB_LIB: &str = "\
pub fn verify_token(s: &str) -> bool { !s.is_empty() }
pub fn connect() -> bool { verify_token(\"...\") }
";

fn write_n_crates(root: &Path, names: &[(&str, &str)]) {
    for (name, lib) in names {
        let sub = root.join(name);
        std::fs::create_dir_all(sub.join("src")).unwrap();
        let mut cargo = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        if name != &"shared" && name != &"lain_lib" {
            cargo.push_str(&format!("[dependencies]\nlain_lib = {{ path = \"../lain_lib\" }}\n"));
        }
        std::fs::write(sub.join("Cargo.toml"), cargo).unwrap();
        std::fs::write(sub.join("src/lib.rs"), lib).unwrap();
    }
    for (name, _) in names {
        let sub = root.join(name);
        crate::test_helpers::init_git_repo(&sub);  // OR inline the git init/commit from existing tests
    }
}

fn write_repos_yaml(root: &Path, names: &[(&str, &str)]) -> std::path::PathBuf {
    let cfg = root.join("repos.yaml");
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut yaml = format!("data_dir: {}\nrepos:\n", data.display());
    for (name, _) in names {
        yaml.push_str(&format!(
            "  - id: {name}\n    source: {{ type: workspace_dir, path: {} }}\n",
            root.join(name).display()
        ));
    }
    std::fs::write(&cfg, yaml).unwrap();
    cfg
}

fn write_workspaces_yaml(root: &Path, ws_name: &str, members: &[&str]) -> std::path::PathBuf {
    let path = root.join("workspaces.yaml");
    let mut yaml = "workspaces:\n".to_string();
    yaml.push_str(&format!("  - name: {ws_name}\n    members: [{members_list}]\n"
        .replace("{members_list}", &members.iter().map(|m| format!("\"{m}\"")).collect::<Vec<_>>().join(", "))));
    std::fs::write(&path, yaml).unwrap();
    path
}

#[tokio::test]
async fn workspace_config_loads_and_validates_members() {
    let tmp = tempfile::tempdir().unwrap();
    let f = WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "backend-team".into(),
            description: None,
            source: None,
            members: vec!["auth-svc".into(), "billing-svc".into()],
        }],
    };
    f.validate().expect("valid workspace should pass");
}

#[tokio::test]
async fn workspace_rejects_unknown_repo_id() {
    let tmp = tempfile::tempdir().unwrap();
    write_n_crates(tmp.path(), &[("auth-svc", SHARED_LIB), ("lain_lib", SHARED_LIB)]);
    let repos_yaml = write_repos_yaml(tmp.path(), &[("auth-svc", ""), ("lain_lib", "")]);
    write_workspaces_yaml(tmp.path(), "backend-team", &["auth-svc", "billing-svc"]);
    let result = load_federation_with_workspace(&repos_yaml, "backend-team").await;
    let err = result.expect_err("billing-svc not in repos.yaml; should error");
    let msg = format!("{err:?}");
    assert!(msg.contains("billing-svc"), "expected missing id in error, got: {msg}");
}

#[tokio::test]
async fn workspace_rejects_sub_two_repos() {
    let f = WorkspacesFile {
        default: None,
        workspaces: vec![WorkspaceSpec {
            name: "tiny".into(),
            description: None,
            source: None,
            members: vec!["only".into()],
        }],
    };
    assert!(f.validate().is_err());
}

#[tokio::test]
async fn workspace_filters_repos_to_members() {
    let tmp = tempfile::tempdir().unwrap();
    // 5 repos; workspace has 3
    write_n_crates(tmp.path(), &[("r1", SHARED_LIB), ("r2", SHARED_LIB), ("r3", SHARED_LIB), ("r4", SHARED_LIB), ("r5", SHARED_LIB)]);
    let repos_yaml = write_repos_yaml(tmp.path(), &[("r1",""),("r2",""),("r3",""),("r4",""),("r5","")]);
    write_workspaces_yaml(tmp.path(), "three", &["r1", "r2", "r3"]);
    let fed = load_federation_with_workspace(&repos_yaml, "three").await.unwrap();
    let loaded: Vec<String> = fed.list_repos().into_iter().map(|(id, _)| id.to_string()).collect();
    assert_eq!(loaded.len(), 3);
    assert!(loaded.contains(&"r1".to_string()));
    assert!(loaded.contains(&"r2".to_string()));
    assert!(loaded.contains(&"r3".to_string()));
    assert!(!loaded.contains(&"r4".to_string()));
}

#[tokio::test]
async fn workspace_mcp_get_active_workspace_returns_correct_subset() {
    use lain::mcp::federation_tools::get_active_workspace;
    let tmp = tempfile::tempdir().unwrap();
    write_n_crates(tmp.path(), &[("auth-svc", AUTH_LIB), ("lain_lib", DB_LIB)]);
    let repos_yaml = write_repos_yaml(tmp.path(), &[("auth-svc", ""), ("lain_lib", "")]);
    write_workspaces_yaml(tmp.path(), "auth-ws", &["auth-svc", "lain_lib"]);
    let fed = load_federation_with_workspace(&repos_yaml, "auth-ws").await.unwrap();
    let workspaces_path = tmp.path().join("workspaces.yaml");
    let ws = WorkspacesFile::load(&workspaces_path).unwrap();
    let info = get_active_workspace(&fed, &ws).expect("active workspace should resolve");
    assert_eq!(info.name, "auth-ws");
    assert_eq!(info.members, vec!["auth-svc", "lain_lib"]);
}

#[tokio::test]
async fn workspace_mcp_get_workspace_graph_filters_correctly() {
    use lain::mcp::federation_tools::get_workspace_graph;
    let tmp = tempfile::tempdir().unwrap();
    write_n_crates(tmp.path(), &[("r1", SHARED_LIB), ("r2", SHARED_LIB), ("r3", SHARED_LIB), ("r4", SHARED_LIB)]);
    let repos_yaml = write_repos_yaml(tmp.path(), &[("r1",""),("r2",""),("r3",""),("r4","")]);
    write_workspaces_yaml(tmp.path(), "subset", &["r1", "r2"]);
    let fed = load_federation_with_workspace(&repos_yaml, "subset").await.unwrap();
    let workspaces_path = tmp.path().join("workspaces.yaml");
    let ws = WorkspacesFile::load(&workspaces_path).unwrap();
    let graph = get_workspace_graph(&fed, &ws, None).expect("graph should succeed");
    for n in &graph.nodes {
        assert!(n.repo_id == "r1" || n.repo_id == "r2",
            "node {} should be in workspace subset, got repo_id={}", n.name, n.repo_id);
    }
}
```

(Note: `get_workspace_graph` is added in PR 2 Task 12. This test will fail until PR 2 lands. **Gate this test with `#[cfg(feature = "workspace_graph")] ` or split the file into PR 1 / PR 2 test modules.** The simplest move: include only the 5 tests that don't need `get_workspace_graph` in PR 1; add the 6th in PR 2.)

- [ ] **Step 2: Run the 5 (or 6) tests**

```
cargo test --features test-utils --test workspace_e2e -- --nocapture --test-threads=1
```
Expected: 5 passed (or 6 if PR 2's graph tool is already landed).

- [ ] **Step 3: Commit**

```bash
git add tests/workspace_e2e.rs
git commit -m "test(federation): 5 workspace_e2e tests (filter, resolve, ambiguous, missing-id)"
```

(If you split the 6th test into PR 2, commit it separately then.)

---

### Task 10: Verify no regression to existing federation tests

**Files:** no code changes.

- [ ] **Step 1: Run the existing federation tests**

Run:
```
cargo test --features test-utils --test federation_integration -- --test-threads=1
cargo test --features test-utils --test federation_cross_repo_e2e -- --test-threads=1   # exists post test-gap fix
cargo test --features test-utils --test federation_benchmark small_fixture -- --nocapture --test-threads=1
```
Expected: all existing tests pass. The new `--workspace` plumbing doesn't affect any path that doesn't pass `--workspace` (today's default).

- [ ] **Step 2: Run the e2e shell script (existing, no-workspace mode)**

Run:
```
cargo build --release
tests/e2e/federation_e2e.sh
```
Expected: `==> E2E PASSED`. The existing script's `lain server --config repos.yaml --transport http` invocation should still work — `--workspace` defaults to "auto", which resolves to "no active workspace set" (no `~/.config/lain/active_workspace` file), which falls through to today's behavior (all repos).

- [ ] **Step 3: If any test fails, investigate**

Most likely failure modes:
- `--workspace auto` resolution accidentally picked up an old `~/.config/lain/active_workspace` from a previous test. Fix: clear the file in test setup, or default `--workspace` to empty string instead of "auto".
- The federation tools' health checks (`/health`) now report a workspace blob. The existing e2e shell may break if it doesn't tolerate the new field. Fix: make the new field optional in `/health` or update the e2e shell.

- [ ] **Step 4: Commit any fixes**

```bash
git add <files>
git commit -m "fix(federation): <description>"
```

---

### Task 11: PR 1 ready gate

**Files:** no code changes.

- [ ] **Step 1: Confirm all PR 1 commits are in place**

```bash
git log --oneline -12
```
Expected (in order):
1. `feat(federation): WorkspaceSpec + workspaces.yaml loader + validation` (Task 1)
2. `feat(federation): WorkspaceSource trait + workspace_dir/clone impls` (Task 2)
3. `feat(federation): ActiveWorkspace pointer at ~/.config/lain/active_workspace` (Task 3)
4. `feat(federation): WorkspaceIndex + filter_repos_by_workspace helper` (Task 4)
5. `feat(federation): load_federation_with_workspace (workspace-aware loader)` (Task 5)
6. `feat(server): --workspace flag resolves active workspace via ActiveWorkspace::load` (Task 6)
7. `feat(workspaces): CLI subcommand + 3 new MCP tools (list_workspaces, get_active_workspace, get_workspace)` (Tasks 7+8)
8. `test(federation): 5 workspace_e2e tests (filter, resolve, ambiguous, missing-id)` (Task 9)
9. Any fix commits from Task 10

- [ ] **Step 2: Open PR 1**

If the user has a PR-creation flow, open PR 1 against `main` with title `feat(workspaces): named groups of repos for federation`. Description references the spec and notes that PR 2 (dashboard + e2e + docs) follows.

**Stop here. Do not proceed to PR 2 tasks until PR 1 has been reviewed and merged.**

---

## PR 2 — Dashboard + e2e + docs (after PR 1 merges)

### Task 12: get_workspace_graph MCP tool

**Files:**
- Modify: `src/mcp/federation_tools.rs` (add `get_workspace_graph` handler + types)
- Modify: `src/mcp/mod.rs` (re-export the new types)
- Modify: `src/mcp/handler.rs` (register the tool, gated on workspace being active)

**Interfaces:**
- Produces:
  - `pub struct GraphNode { pub id: String, pub name: String, pub path: String, pub repo_id: String, pub kind: String }`
  - `pub struct GraphEdge { pub source: String, pub target: String, pub edge_type: String, pub cross_repo: bool }`
  - `pub struct WorkspaceGraph { pub nodes: Vec<GraphNode>, pub edges: Vec<GraphEdge> }`
  - `pub fn get_workspace_graph(fed: &FederatedIndex, workspaces: &WorkspacesFile, filter: Option<&str>) -> Result<WorkspaceGraph, LainError>`
  - Filter to `Function` / `Method` / `Class` node kinds + `Calls` / `Imports` edge types (per the locked UI scope).
  - Cap at 5000 nodes / 10000 edges; truncate with `truncated: true` flag if exceeded.
  - Mark `cross_repo: true` on edges where source's `repo_id` ≠ target's `repo_id`.

- [ ] **Step 1: Add the types and handler in `src/mcp/federation_tools.rs`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub repo_id: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub cross_repo: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceGraph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub truncated: bool,
}

const GRAPH_NODE_CAP: usize = 5000;
const GRAPH_EDGE_CAP: usize = 10000;

pub fn get_workspace_graph(
    fed: &FederatedIndex,
    workspaces: &WorkspacesFile,
    filter: Option<&str>,
) -> Result<WorkspaceGraph, LainError> {
    // Reuse get_active_workspace to find the workspace this federation corresponds to.
    let ws = workspaces.workspaces.iter().find(|w| {
        let loaded: std::collections::HashSet<String> = fed.list_repos().into_iter().map(|(id, _)| id.to_string()).collect();
        w.members.iter().all(|m| loaded.contains(m)) && loaded.iter().all(|l| w.members.contains(l))
    }).ok_or_else(|| LainError::NoActiveWorkspace("federation loaded but no workspace matches".into()))?;
    let members: std::collections::HashSet<String> = ws.members.iter().cloned().collect();

    // Walk the global backend.
    let all_nodes = fed.backend().list_nodes()?;
    let mut nodes = Vec::with_capacity(all_nodes.len());
    for n in all_nodes {
        // Filter by kind.
        let kind_str = format!("{:?}", n.node_type);
        if !matches!(kind_str.as_str(), "Function" | "Method" | "Class") { continue; }
        // Filter by workspace membership.
        let gid = crate::federation::repo_id::GlobalId::parse(&n.id).ok();
        let repo_id = gid.as_ref().map(|g| g.repo_id().to_string()).unwrap_or_default();
        if !members.contains(&repo_id) { continue; }
        // Filter by substring match.
        if let Some(f) = filter {
            if !n.name.contains(f) && !n.path.contains(f) { continue; }
        }
        if nodes.len() >= GRAPH_NODE_CAP { break; }
        nodes.push(GraphNode {
            id: n.id.clone(),
            name: n.name.clone(),
            path: n.path.clone(),
            repo_id,
            kind: kind_str,
        });
    }

    let node_ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let all_edges = fed.backend().all_edges()?;
    let mut edges = Vec::with_capacity(all_edges.len());
    let mut truncated = false;
    for e in all_edges {
        if !node_ids.contains(&e.source_id) || !node_ids.contains(&e.target_id) { continue; }
        let edge_kind = format!("{:?}", e.edge_type);
        if !matches!(edge_kind.as_str(), "Calls" | "Imports") { continue; }
        if edges.len() >= GRAPH_EDGE_CAP { truncated = true; break; }
        let cross_repo = {
            let s = crate::federation::repo_id::GlobalId::parse(&e.source_id).ok();
            let t = crate::federation::repo_id::GlobalId::parse(&e.target_id).ok();
            match (s, t) {
                (Some(a), Some(b)) => a.repo_id() != b.repo_id(),
                _ => false,
            }
        };
        edges.push(GraphEdge {
            source: e.source_id,
            target: e.target_id,
            edge_type: edge_kind,
            cross_repo,
        });
    }
    if nodes.len() >= GRAPH_NODE_CAP { truncated = true; }

    Ok(WorkspaceGraph { nodes, edges, truncated })
}
```

(Note: `fed.backend().all_edges()` may not exist — use `fed.backend().edges_of_type(EdgeType::Calls)` and `edges_of_type(EdgeType::Imports)` separately if the backend exposes per-type accessors. If only a generic edge iterator exists, use that. Adjust the implementer's call accordingly.)

- [ ] **Step 2: Re-export from `src/mcp/mod.rs`**

Add `pub use federation_tools::{get_workspace_graph, WorkspaceGraph, GraphNode, GraphEdge};`.

- [ ] **Step 3: Register in `src/mcp/handler.rs`**

In the federation-mode tool list, add:

```rust
(
    "get_workspace_graph",
    "Per-workspace graph for the dashboard. Returns {nodes: [...], edges: [...], truncated: bool}. Filters to Function/Method/Class + Calls/Imports. Optional filter: substring match against node name + path.",
    &["filter?"],
),
```

And the dispatch arm:

```rust
"get_workspace_graph" => {
    let filter = args_owned.get("filter").and_then(|v| v.as_str());
    return match crate::mcp::federation_tools::get_workspace_graph(fed, &workspaces, filter) {
        Ok(graph) => Ok(tool_text_result(serde_json::to_string(&graph).unwrap_or_default(), false)),
        Err(e) => Ok(tool_text_result(format!("{e}"), true)),
    };
}
```

The tool is **only registered when a workspace is active** (gate on `active.is_some()` in the tool-list construction).

- [ ] **Step 4: Build**

```
cargo build
```
Expected: clean build.

- [ ] **Step 5: Add the 6th test to `tests/workspace_e2e.rs`**

Move the 6th test from "gated" into the main file. Run:

```
cargo test --features test-utils --test workspace_e2e -- --nocapture --test-threads=1
```
Expected: 6 passed.

- [ ] **Step 6: Commit**

```bash
git add src/mcp/federation_tools.rs src/mcp/mod.rs src/mcp/handler.rs tests/workspace_e2e.rs
git commit -m "feat(workspaces): get_workspace_graph MCP tool (filtered Function/Method/Class + Calls/Imports)"
```

---

### Task 13: Federation dashboard 3 new sections

**Files:**
- Modify: `src/mcp/federation_dashboard.html` (add 3 sections after the existing "Repositories" table)

**Interfaces:**
- Produces: a working HTML page with:
  - Workspaces panel (only when a workspace is active)
  - Config panel (repos.yaml path, workspaces.yaml path, active workspace name, repo counts)
  - Per-workspace D3 force-directed graph

- [ ] **Step 1: Add HTML markup for the 3 sections**

After the `<h2>Repositories</h2>` block in `src/mcp/federation_dashboard.html`, insert:

```html
<h2 id="active-workspace-header" style="display: none">Active workspace</h2>
<div id="workspace-banner" class="banner" style="display: none">
  <span class="badge active" id="workspace-name-badge"></span>
  <span class="muted" id="workspace-meta"></span>
</div>
<table id="workspace-members" style="display: none">
  <thead><tr><th>Repo id</th><th>Path</th><th>Health</th></tr></thead>
  <tbody></tbody>
</table>

<h2>Config</h2>
<table id="config">
  <tbody></tbody>
</table>

<h2 id="workspace-graph-header" style="display: none">Workspace graph</h2>
<div class="controls" id="graph-controls" style="display: none">
  <label>Filter: <input id="graph-filter" placeholder="substring match"></label>
  <span class="muted" id="graph-meta"></span>
</div>
<svg id="workspace-graph" width="100%" height="500" style="display: none; background: #fafafa; border: 1px solid #ddd;"></svg>
<div class="legend" id="graph-legend" style="display: none">
  <span class="dot repo-a"></span> node by repo (color)
  <span class="line calls"></span> Calls (intra-repo, solid)
  <span class="line calls-cross"></span> Calls (cross-repo, dashed)
  <span class="line imports"></span> Imports
</div>
```

- [ ] **Step 2: Add CSS for the new elements**

In the `<style>` block:

```css
.banner { padding: 1rem; background: #f4f4f4; border-radius: 4px; margin-bottom: 1rem; }
.badge.active { background: #00e5cc; color: #05080f; padding: 0.15rem 0.5rem; border-radius: 3px; font-weight: 600; }
#workspace-graph circle { stroke: #fff; stroke-width: 1.5; }
#workspace-graph .edge { stroke: #888; stroke-opacity: 0.7; }
#workspace-graph .edge.cross-repo { stroke: #d6336c; stroke-dasharray: 4 4; }
.legend { display: flex; gap: 1rem; margin-top: 0.5rem; font-size: 0.85em; }
.legend .dot { display: inline-block; width: 12px; height: 12px; border-radius: 50%; background: #888; vertical-align: middle; margin-right: 4px; }
.legend .line { display: inline-block; width: 24px; height: 2px; vertical-align: middle; margin-right: 4px; }
.legend .line.calls { background: #888; }
.legend .line.calls-cross { background: #d6336c; background-image: linear-gradient(to right, #d6336c 50%, transparent 50%); background-size: 8px 2px; }
.legend .line.imports { background: #bbb; border-top: 1px dashed #999; height: 0; }
```

- [ ] **Step 3: Add the JavaScript to fetch + render**

After the existing `load()` function (and the `load()` invocation), extend it:

```javascript
async function loadWorkspaces() {
    const list = await callTool('list_workspaces', {});
    const active = list.find(w => w.is_active);
    if (!active) return;  // no active workspace → hide sections

    document.getElementById('active-workspace-header').style.display = '';
    document.getElementById('workspace-banner').style.display = '';
    document.getElementById('workspace-members').style.display = '';
    document.getElementById('workspace-name-badge').textContent = active.name;
    document.getElementById('workspace-meta').textContent = `${active.member_count} repos${active.description ? ' · ' + active.description : ''}`;

    const detail = await callTool('get_workspace', { name: active.name });
    const tbody = document.querySelector('#workspace-members tbody');
    for (const m of detail.members) {
        const tr = document.createElement('tr');
        tr.innerHTML = `<td><code>${escapeHtml(m.repo_id)}</code></td><td><code>${escapeHtml(m.path || '')}</code></td><td><span class="health ${(m.health || 'unknown').toLowerCase()}">${escapeHtml(m.health || 'unknown')}</span></td>`;
        tbody.appendChild(tr);
    }
}

async function loadConfig() {
    const tbody = document.querySelector('#config tbody');
    const ws = await callTool('list_workspaces', {});
    const active = ws.find(w => w.is_active);
    const rows = [
        ['repos.yaml', '(from MCP context)', /* opaque */],
        ['workspaces.yaml', '(from MCP context)', /* opaque */],
        ['active workspace', active ? active.name : 'none'],
        ['repos in workspace', active ? `${active.member_count}` : 'n/a'],
    ];
    for (const [k, v] of rows) {
        const tr = document.createElement('tr');
        tr.innerHTML = `<td>${k}</td><td>${escapeHtml(String(v))}</td>`;
        tbody.appendChild(tr);
    }
}

async function loadGraph() {
    const list = await callTool('list_workspaces', {});
    if (!list.find(w => w.is_active)) return;
    document.getElementById('workspace-graph-header').style.display = '';
    document.getElementById('graph-controls').style.display = '';
    document.getElementById('workspace-graph').style.display = '';
    document.getElementById('graph-legend').style.display = '';

    const filter = document.getElementById('graph-filter').value || null;
    const graph = await callTool('get_workspace_graph', filter ? { filter } : {});

    // Color nodes by repo_id (deterministic palette).
    const repos = [...new Set(graph.nodes.map(n => n.repo_id))].sort();
    const palette = ['#4e79a7', '#f28e2b', '#e15759', '#76b7b2', '#59a14f', '#edc948', '#b07aa1', '#ff9da7', '#9c755f', '#bab0ab'];
    const colorFor = (rid) => palette[repos.indexOf(rid) % palette.length];

    // Render with D3 force-directed. CDN script tag added to <head> in the same file.
    const svg = d3.select('#workspace-graph');
    svg.selectAll('*').remove();
    const width = svg.node().clientWidth;
    const height = 500;
    const sim = d3.forceSimulation(graph.nodes)
        .force('link', d3.forceLink(graph.edges).id(d => d.id).distance(80))
        .force('charge', d3.forceManyBody().strength(-200))
        .force('center', d3.forceCenter(width / 2, height / 2));
    const link = svg.append('g').selectAll('line').data(graph.edges).join('line')
        .attr('class', d => 'edge' + (d.cross_repo ? ' cross-repo' : ''))
        .attr('stroke', d => d.cross_repo ? '#d6336c' : (d.edge_type === 'Imports' ? '#bbb' : '#888'))
        .attr('stroke-width', 1.5);
    const node = svg.append('g').selectAll('circle').data(graph.nodes).join('circle')
        .attr('r', 6)
        .attr('fill', d => colorFor(d.repo_id))
        .append('title')
        .text(d => `${d.name}\n${d.repo_id} · ${d.path}`);
    sim.on('tick', () => {
        link.attr('x1', d => d.source.x).attr('y1', d => d.source.y).attr('x2', d => d.target.x).attr('y2', d => d.target.y);
        node.attr('cx', d => d.x).attr('cy', d => d.y);
    });

    document.getElementById('graph-meta').textContent =
        `${graph.nodes.length} nodes · ${graph.edges.length} edges${graph.truncated ? ' (truncated)' : ''}`;
}

// Wire into the existing load() pipeline: call the three loaders after the repos table populates.
(async function() {
    await loadWorkspaces();
    await loadConfig();
    await loadGraph();
})();
```

- [ ] **Step 4: Add D3 CDN script tag to `<head>`**

In the `<head>` section:

```html
<script src="https://d3js.org/d3.v7.min.js"></script>
```

- [ ] **Step 5: Manually smoke-test the dashboard**

Run a federation server with a workspace:

```
cargo build --release
LAIN_BIN=./target/release/lain bash -c '
  WORKDIR=$(mktemp -d)
  cat > $WORKDIR/repos.yaml <<EOF
data_dir: $WORKDIR/data
repos:
  - id: r1
    source: { type: workspace_dir, path: $WORKDIR/r1 }
  - id: r2
    source: { type: workspace_dir, path: $WORKDIR/r2 }
EOF
  mkdir -p $WORKDIR/r1/src $WORKDIR/r2/src
  echo "pub fn hello() {}" > $WORKDIR/r1/src/lib.rs
  echo "pub fn world() {}" > $WORKDIR/r2/src/lib.rs
  cat > $WORKDIR/workspaces.yaml <<EOF
workspaces:
  - name: subset
    members: [r1, r2]
EOF
  ./target/release/lain server --config $WORKDIR/repos.yaml --transport http --port 19999 &
  SERVER_PID=$!
  sleep 5
  curl -s http://localhost:19999/federation-dashboard.html | grep -c "workspace-graph"
  kill $SERVER_PID
'
```
Expected: grep count > 0 (the dashboard renders the new section).

- [ ] **Step 6: Commit**

```bash
git add src/mcp/federation_dashboard.html
git commit -m "feat(dashboard): workspace panel + config panel + per-workspace D3 graph"
```

---

### Task 14: get_agent_strategy — workspace section

**Files:**
- Modify: `src/tools.rs` (extend the strategy string with a "Workspace mode" section)

**Interfaces:**
- Produces: a strategy text that includes a section explaining workspace-aware tools and the `repo_id` resolution rule when a workspace is active.

- [ ] **Step 1: Append the workspace section to `get_agent_strategy`**

In `src/tools.rs`, in the `get_agent_strategy` function (around line 492), after the federation-mode block, add:

```rust
sections.push("\n---\n\n## Workspace Mode (scoped subset of repos)\n".to_string());
sections.push(
    "When the server is started with `--workspace <name>`, only that workspace's \
     repos are loaded. Use these tools to learn the scope and reason about it:\n"
        .to_string(),
);
sections.push("- **list_workspaces**: list known workspaces + which is active\n".to_string());
sections.push("- **get_active_workspace**: which workspace the server holds right now\n".to_string());
sections.push("- **get_workspace(name)**: full detail on one workspace\n".to_string());
sections.push("- **get_workspace_graph(filter?)**: node + edge data for the dashboard view\n".to_string());
sections.push(
    "\nThe 6 federation tools (`list_repos`, `search_org`, `get_cross_repo_blast_radius`, etc.) \
     operate over the active workspace's repo subset. `get_repo_info(<id>)` returns `NotFound` \
     if the id isn't in the active workspace — that's correct, not a bug.\n"
        .to_string(),
);
sections.push(
    "\n### Detection\n".to_string()
);
sections.push(
    "If `list_workspaces` appears in your tool list, you're in workspace mode. Call \
     `get_active_workspace` to learn which subset is loaded before issuing broad queries \
     like `search_org`.\n"
        .to_string(),
);
```

- [ ] **Step 2: Build**

```
cargo build
```
Expected: clean build. Existing `tests/federation_integration.rs::get_agent_strategy_mentions_federation_tools` test (line 599) still passes (we didn't remove the federation section).

- [ ] **Step 3: Commit**

```bash
git add src/tools.rs
git commit -m "feat(tools): get_agent_strategy includes workspace mode section"
```

---

### Task 15: Nightly e2e — `tests/e2e/workspace_e2e.sh`

**Files:**
- Create: `tests/e2e/workspace_e2e.sh`

**Interfaces:**
- Produces: a shell script that builds a tempdir with `repos.yaml` + `workspaces.yaml`, starts `lain server --workspace <name>`, exercises the 4 workspace-aware MCP tools, repeats with a different workspace, asserts expected shapes.

- [ ] **Step 1: Mirror the existing `tests/e2e/federation_e2e.sh` structure**

Read `tests/e2e/federation_e2e.sh` (125 lines) for the `WORKDIR` / `mcp_text` / `call_tool` / polling patterns. The new script reuses those helpers verbatim.

- [ ] **Step 2: Write the script**

```bash
#!/usr/bin/env bash
# E2E test for workspace-aware federation mode. Builds a tempdir with
# repos.yaml + workspaces.yaml, starts `lain server --workspace <name>`,
# exercises the 4 workspace-aware MCP tools (list_workspaces,
# get_active_workspace, get_workspace, get_workspace_graph), repeats with
# a different workspace. Requires: curl, python3, a built `lain` binary.
# Gated: runs on nightly or manual, not per-PR.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${REPO_ROOT}/target/release/lain"
PORT="${LAIN_E2E_PORT:-19997}"

if [[ ! -x "${BIN}" ]]; then
    echo "ERROR: ${BIN} not found. Run \`cargo build --release\` first." >&2
    exit 2
fi

WORKDIR="$(mktemp -d)"
trap 'kill "${LAIN_PID:-}" 2>/dev/null || true; rm -rf "${WORKDIR}"' EXIT

# Build 3 fake repo dirs (no commits needed for federation to load; the
# workspace graph view will have 0 nodes but the tool surface works).
for r in alpha beta gamma; do
    mkdir -p "${WORKDIR}/${r}/src"
    echo "pub fn ${r}_fn() {}" > "${WORKDIR}/${r}/src/lib.rs"
done

# repos.yaml: 3 repos.
cat > "${WORKDIR}/repos.yaml" <<EOF
data_dir: ${WORKDIR}/data
repos:
  - id: alpha
    source: { type: workspace_dir, path: ${WORKDIR}/alpha }
  - id: beta
    source: { type: workspace_dir, path: ${WORKDIR}/beta }
  - id: gamma
    source: { type: workspace_dir, path: ${WORKDIR}/gamma }
EOF

# workspaces.yaml: 2 workspaces.
cat > "${WORKDIR}/workspaces.yaml" <<EOF
workspaces:
  - name: ab
    members: [alpha, beta]
  - name: cg
    members: [gamma]
EOF

call_tool() {
    local name="$1"
    local args="${2:-{}}"
    curl -fsS -X POST "http://localhost:${PORT}/mcp" \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"${name}\",\"arguments\":${args}},\"id\":1}"
}

mcp_text() {
    python3 -c '
import json, sys
data = json.load(sys.stdin)
content = data.get("result", {}).get("content", [])
if not content:
    sys.exit("no content block")
print(content[0]["text"])
'
}

echo "==> Starting lain server on port ${PORT} with --workspace ab..."
"${BIN}" server \
    --config "${WORKDIR}/repos.yaml" \
    --workspace ab \
    --transport http \
    --port "${PORT}" \
    --log-level info \
    > "${WORKDIR}/server.log" 2>&1 &
LAIN_PID=$!
trap 'kill "${LAIN_PID}" 2>/dev/null || true; rm -rf "${WORKDIR}"' EXIT

# Wait for server.
for i in $(seq 1 60); do
    if call_tool "get_federation_health" '{}' >/dev/null 2>&1; then break; fi
    sleep 2
done

echo "==> Calling list_workspaces..."
list_text="$(call_tool "list_workspaces" '{}' | mcp_text)"
count="$(printf '%s' "${list_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
if [[ "${count}" -ne 2 ]]; then
    echo "ERROR: list_workspaces returned ${count} workspaces, expected 2." >&2
    exit 1
fi

echo "==> Calling get_active_workspace..."
active_text="$(call_tool "get_active_workspace" '{}' | mcp_text)"
active_name="$(printf '%s' "${active_text}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["name"])')"
if [[ "${active_name}" != "ab" ]]; then
    echo "ERROR: get_active_workspace returned ${active_name}, expected ab." >&2
    exit 1
fi
member_count="$(printf '%s' "${active_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["members"]))')"
if [[ "${member_count}" -ne 2 ]]; then
    echo "ERROR: active workspace has ${member_count} members, expected 2." >&2
    exit 1
fi

echo "==> Calling get_workspace..."
detail_text="$(call_tool "get_workspace" '{"name":"cg"}' | mcp_text)"
detail_count="$(printf '%s' "${detail_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["members"]))')"
if [[ "${detail_count}" -ne 1 ]]; then
    echo "ERROR: cg workspace has ${detail_count} members, expected 1." >&2
    exit 1
fi

echo "==> Calling get_workspace_graph (no filter)..."
graph_text="$(call_tool "get_workspace_graph" '{}' | mcp_text)"
node_count="$(printf '%s' "${graph_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["nodes"]))')"
echo "    workspace graph: ${node_count} nodes"

echo "==> E2E PASSED"
trap 'rm -rf "${WORKDIR}"' EXIT
```

`chmod +x tests/e2e/workspace_e2e.sh`.

- [ ] **Step 3: Run the script; verify it passes**

```
cargo build --release
tests/e2e/workspace_e2e.sh
```
Expected: `==> E2E PASSED`. Runtime: <2 min (cold start dominates; 3 repos, no LSP hydration required for empty fixtures).

- [ ] **Step 4: Commit**

```bash
git add tests/e2e/workspace_e2e.sh
git commit -m "test(e2e): add workspace_e2e.sh (4 workspace-aware MCP tools)"
```

---

### Task 16: Docs updates

**Files:**
- Modify: `docs/FEDERATION.md` (add a "Workspaces" section)
- Modify: `README.md` (one-liner pointer)

**Interfaces:**
- Produces: documentation that points future operators + agents at the workspace feature.

- [ ] **Step 1: Add a "Workspaces" section to `docs/FEDERATION.md`**

After the existing federation sections (find a good anchor — after "Migration" or "Smoke test"), add:

```markdown
## Workspaces

Workspaces are named groups of repos that the federation engine indexes
together as a coherent unit. A workspace = a subset of `repos.yaml`'s
repos, switchable on server restart.

### When to use workspaces

| Mode | Use it when |
|---|---|
| Federation (`lain server --config repos.yaml`) | Org-wide questions across all repos |
| Workspace (`lain server --config repos.yaml --workspace <name>`) | Questions scoped to a named subset ("backend-team", "payments-ws") |

### Setup

1. Declare workspaces in `workspaces.yaml` (same directory as `repos.yaml`):
   ```yaml
   workspaces:
     - name: backend-team
       members: [auth-svc, billing-svc, db-client]
   ```
2. Pick one: `lain workspaces use backend-team` (writes `~/.config/lain/active_workspace`)
3. Start the server: `lain server --config repos.yaml --workspace auto --transport http --port 9999`
4. The federation loads only `backend-team`'s members. All 6 federation tools operate scoped.

### Workspace CLI

```
lain workspaces create / add / remove / import / init / list / show / use / current / forget
```

See `lain workspaces --help`.

### MCP tools (workspace mode)

In addition to the 6 federation tools (scoped to the workspace's repos):
- `list_workspaces` — all known workspaces + which is active
- `get_active_workspace` — the active workspace's name + members
- `get_workspace(name)` — full detail on one workspace
- `get_workspace_graph(filter?)` — node + edge data for the dashboard graph

### Federation dashboard

`/federation-dashboard.html` (when running `lain server --transport http`) gains
three sections when a workspace is active:
- Active workspace panel (name + members + their paths/healths)
- Config panel (paths + repo counts)
- Per-workspace D3 force-directed graph view (Functions/Methods/Classes + Calls/Imports,
  color by repo_id, dashed lines for cross-repo Calls)

### Agents

`get_agent_strategy` (a built-in MCP tool) includes a "Workspace mode" section that
documents the new tools + the `repo_id` resolution rule when scoped.
```

- [ ] **Step 2: Add a one-liner to README.md**

In the "Manage multiple projects" section (or after it), add:

```markdown
Group repos into named workspaces (`lain workspaces use backend-team`) so the
federation only loads the subset you care about. See `docs/FEDERATION.md#workspaces`.
```

- [ ] **Step 3: Verify the docs read coherently**

Skim both edits. Confirm:
- Tone matches surrounding docs
- Code blocks are syntactically valid
- Links to `docs/FEDERATION.md#workspaces` anchor correctly

- [ ] **Step 4: Commit**

```bash
git add docs/FEDERATION.md README.md
git commit -m "docs(federation): Workspaces section in FEDERATION.md + README pointer"
```

---

### Task 17: PR 2 final verification + commit

**Files:** no code changes.

- [ ] **Step 1: Run the full per-PR test suite**

```
cargo test --features test-utils -- --test-threads=1
```
Expected: all tests pass — `federation_integration`, `federation_cross_repo_e2e` (post test-gap fix), `federation_benchmark` (small fixture), `workspace_e2e`.

- [ ] **Step 2: Run both e2e shell scripts**

```
cargo build --release
tests/e2e/federation_e2e.sh
tests/e2e/workspace_e2e.sh
```
Expected: both print `==> E2E PASSED`.

- [ ] **Step 3: Confirm all PR 2 commits are in place**

```bash
git log --oneline -10
```
Expected:
- `feat(workspaces): get_workspace_graph MCP tool (filtered Function/Method/Class + Calls/Imports)` (Task 12)
- `feat(dashboard): workspace panel + config panel + per-workspace D3 graph` (Task 13)
- `feat(tools): get_agent_strategy includes workspace mode section` (Task 14)
- `test(e2e): add workspace_e2e.sh (4 workspace-aware MCP tools)` (Task 15)
- `docs(federation): Workspaces section in FEDERATION.md + README pointer` (Task 16)

- [ ] **Step 4: Open PR 2 against `main`**

Title: `feat(workspaces): dashboard graph + nightly e2e + docs`. Description references the spec and notes that this is the follow-up to PR 1.

**Stop here. PR 2 is complete.**

---

## Self-Review Notes

After writing this plan, I checked against the spec checklist:

**1. Spec coverage:**
- WorkspaceSpec + parse + validate (Task 1) ✓
- WorkspaceSource trait + 2 impls (Task 2) ✓
- ActiveWorkspace pointer at `~/.config/lain/active_workspace` (Task 3) ✓
- WorkspaceIndex + filter (Task 4) ✓
- Workspace-aware loader (Task 5) ✓
- `--workspace` flag wiring (Task 6) ✓
- CLI subcommand (Task 7) ✓
- 3 new MCP tools (Task 8) ✓
- 6 per-PR tests (Tasks 9) ✓
- No-regression verification (Task 10) ✓
- PR 1 ready gate (Task 11) ✓
- get_workspace_graph MCP tool (Task 12) ✓
- Dashboard 3 sections (Task 13) ✓
- get_agent_strategy workspace section (Task 14) ✓
- workspace_e2e.sh (Task 15) ✓
- Docs (Task 16) ✓
- PR 2 final verification (Task 17) ✓
- Prerequisite (test-gap fix must land first) — flagged in Global Constraints ✓

**2. Placeholder scan:** No "TBD" / "TODO" / "fill in" / "appropriate" placeholders. Two flagged risks (explicit):
- Task 1 step 3: `WorkspaceCloneSource::fetch` requires a real `git` binary + network; integration test deferred to Task 9
- Task 12 step 1: `fed.backend().all_edges()` may not exist; implementer must use the backend's actual edge-iterator accessor

**3. Type consistency:**
- `WorkspaceSpec.members: Vec<String>` (spec) ↔ plan (Task 1 struct) ✓
- `WorkspaceSource` trait methods (id, local_path, kind, fetch, is_stale) match `RepoSource` shape (spec) ✓
- `LainError::NoActiveWorkspace(String)` new variant (Task 1) ↔ used by `get_active_workspace` (Task 8) and `load_federation_with_workspace` (Task 5 path validation) ✓
- `get_workspace_graph` returns `WorkspaceGraph { nodes, edges, truncated }` matching the spec's required cap behavior ✓
- 5-test split (Tasks 9 + 12) is intentional — `get_workspace_graph` is added in PR 2, so the 6th test depends on it

**4. Risk areas flagged for the implementer:**
- Task 5: explicit per-repo indexing pattern (NOT `load_federation`'s no-index pattern); mirror `src/cmds/server.rs:49-74`
- Task 8: `get_active_workspace` infers the workspace from loaded-repos intersection (no explicit active-workspace signal in the engine)
- Task 12: edge accessor names may differ; check `GraphBackend` trait
- Task 13: dashboard uses D3 v7 from CDN; ensure CSP allows it (or self-host)
- General: `cargo` at `/home/sebastian/.rustup/toolchains/...` in dev sandbox; PATH must include it