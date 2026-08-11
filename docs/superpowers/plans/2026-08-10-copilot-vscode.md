# GitHub Copilot in VS Code Agent Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `lain init --agent copilot` and `lain agents install copilot` configure GitHub Copilot Chat in VS Code to use the Lain MCP server, so a single install covers both VS Code and Copilot Chat.

**Architecture:** Add a new `copilot` agent id to the manifest, a `CopilotAdapter` for the `agents install` path, and an `init_copilot` function for the `init` path. Both write a JSON `servers.lain` entry using `command: "lain"` (string) + `args: [...]` (array) — verified from the VS Code and Copilot MCP docs. The same `--scope` flag added in the opencode work selects per-project (`.vscode/mcp.json`) vs user-global (`~/.copilot/mcp-config.json`) config; a bundled `.github/copilot-instructions.md` is written for project scope.

**Tech Stack:** Rust (bin crate), clap, serde_json, dirs, tempfile (dev), git2 (already a dep).

## Global Constraints

- **Root key is `servers`** (top-level) in both `.vscode/mcp.json` and `~/.copilot/mcp-config.json`. Not `mcp`, not `mcpServers`. Verified from the [VS Code MCP docs](https://code.visualstudio.com/docs/agent-customization/mcp-servers).
- **Local server shape** (verbatim from VS Code docs): `servers.<name>.{ command: <string>, args: <array of strings> }`. The `type: "stdio"` field is optional (local is the default). Verified GitHub Copilot uses the same file ([Copilot MCP docs](https://docs.github.com/en/copilot/customizing-copilot/using-model-context-protocol/extending-copilot-chat-with-mcp)).
- **`--workspace auto` works without a wrapper** because VS Code launches MCP subprocesses with cwd set to the project root.
- **`command: "lain"` (bare PATH-resolvable name)** — VS Code does not silently reject absolute paths the way Claude Code does, but the bare name keeps the config portable.
- **`--scope` flag is already on the `Init` subcommand** (added in the opencode work). It is only honored by `init_copilot` in this work; other inits continue to ignore it.
- **The adapter MUST use `entry.mcp_section.clone()` and `entry.mcp_name.clone()`** in `install`, `read`, and `remove` — the lesson from the opencode fix wave. No hardcoded `"servers"` / `"lain"` in the adapter.
- **`omp` is oh-my-pi, not VS Code/Copilot.** Unchanged.

---

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `agents/manifest.toml` | modify | Add `[[agent]]` block for `copilot`. |
| `src/cmds/agents/adapters/copilot.rs` | create | `CopilotAdapter` + `pub fn build_copilot_lain_entry`. |
| `src/cmds/agents/adapters/mod.rs` | modify | Register the adapter in `adapter_for`; re-export. |
| `src/cmds/init.rs` | modify | `init_copilot`, `COPILOT_INSTRUCTIONS_MD` const, dispatch, tests. |
| `hooks/copilot/copilot-instructions.md` | create | Bundled awareness doc. |
| `tests/e2e_copilot.rs` | create | End-to-end real-install test. |

`src/main.rs` does not need new CLI surface — the `--scope` flag is already there. We only need `copilot` added to `SUPPORTED_AGENTS` in `src/cmds/init.rs`.

---

## Task 1: Manifest entry + bundled `copilot-instructions.md` awareness doc

**Files:**
- Create: `hooks/copilot/copilot-instructions.md`
- Modify: `agents/manifest.toml`
- Test: content pin in `src/cmds/init.rs` (via a module-level `const` + a test in `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `const COPILOT_INSTRUCTIONS_MD: &str` in `src/cmds/init.rs` (module-level, used by Task 3 and the content pin).

- [ ] **Step 1: Create `hooks/copilot/copilot-instructions.md`**

Write the file with this exact content:

```markdown
# LAIN — Codebase Intelligence

You have access to the Lain MCP server (tool prefix `mcp__lain__`). Lain indexes
the current workspace and exposes tools for structural queries, semantic
search, and code navigation.

## When to use lain

Reach for lain when the question is about **structure**, not single lines:
- "Where should I start reading this codebase?"
- "What does X depend on?" / "Who calls X?"
- "If I change X, what else breaks?"
- "Where do we do X?" (semantic search by meaning)
- "What is this function/class doing?"
- "Is there unused code?"
- "What files change together?"
- "Which test covers X?" / "What's untested?"

Skip lain for simple file reads, single-line `grep`, or trivial edits.

## The most useful tools

| Tool | When to use it |
|------|----------------|
| `get_health` | First call in any session. Returns the resolved workspace, node counts, and status. |
| `find_anchors` | "Where should I start reading?" — most-called, most-stable symbols. |
| `list_entry_points` | Find `main()`s and entry points. |
| `explore_architecture` | High-level module/file tree. |
| `get_blast_radius` | "If I change X, what breaks?" — transitive callers. |
| `trace_dependency` | "What does X depend on?" — callees + imports. |
| `get_call_chain` | Specific call path between two symbols. |
| `semantic_search` | Find code by meaning (e.g. "rate limiting", "retry logic"). Body excerpts included. |
| `explain_symbol` | "What is this symbol?" — source + callers + callees + anchor. |
| `get_code_snippet` | Read the exact source of a symbol by id. |
| `find_dead_code` | Unused definitions (filters false positives). |
| `get_coupling_radar` | "What files change with X?" (co-change history). |
| `get_cross_runtime_callers` | Cross-language callers (e.g. Python → Rust FFI). |
| `find_test_file` / `find_untested_functions` / `get_coverage_summary` | Test discovery and coverage. |
| `get_file_diff` / `get_commit_history` | Git operations on the workspace. |

## Workflows

**"I'm new here, where do I start?"**
1. `get_health` — confirm the workspace resolved correctly.
2. `find_anchors limit=5` — top entry points.
3. `explain_symbol <top anchor>` — understand it.
4. `get_blast_radius <top anchor>` — see what depends on it.

**"I'm about to refactor X"**
1. `get_blast_radius <X>` — who will break.
2. `get_coupling_radar <X>` — what else usually changes with it.
3. `find_test_file <X>` (or `find_untested_functions`) — what's already covered.
4. Make the change, then re-run `get_blast_radius` to verify no surprises.

**"Where do we do X?" (semantic)**
1. `semantic_search query="<natural language>" limit=5` — get candidates with body excerpts.
2. `explain_symbol <top result>` — read it in context.

**"What calls X?" / "What does X call?"**
- Callers (incoming): `get_blast_radius <X>`
- Callees (outgoing): `trace_dependency <X>`

**"Read this symbol"**
- `explain_symbol <X>` for full context, or `get_code_snippet <X>` for raw source.

## Caveats

- **First-call latency**: the very first tool call after a fresh server start can take 5–10s (model warmup). Don't panic.
- **Workspace scope**: bound to the git repository you opened this session in (auto-discovered from the working directory). Lain cannot analyze a different repo.
- **Semantic search needs a query model**: if `get_health` reports no embedding model, `semantic_search` will not work — fall back to `explain_symbol` / `find_anchors` and the query language.

## Don't

- Don't use `semantic_search` with literal symbol names; use natural language describing the concept.
- Don't call lain tools against a path outside the workspace.
- Don't repeatedly call `get_health`; once per session is enough.
```

- [ ] **Step 2: Add the manifest entry**

In `agents/manifest.toml`, append a new `[[agent]]` block:

```toml
[[agent]]
id = "copilot"
display_name = "GitHub Copilot in VS Code"
binary = "code"
detect_paths = ["~/.config/Code", "~/.vscode"]
config_user = "~/.copilot/mcp-config.json"
config_project = ".vscode/mcp.json"
config_format = "jsonc"
mcp_section = "servers"
mcp_name = "lain"
transport = "stdio"
command = "lain"
default_args = []
headless_probe = ["code", "--version"]
```

- [ ] **Step 3: Add the module-level const and the content pin test**

In `src/cmds/init.rs`, near the existing `CLAUDE_AWARENESS_MD` const at the top of the file (around line 19), add:

```rust
const COPILOT_INSTRUCTIONS_MD: &str = include_str!("../../hooks/copilot/copilot-instructions.md");
```

(Place it right after `CLAUDE_AWARENESS_MD` so the awareness docs are grouped.)

In the `#[cfg(test)] mod tests` block, add the content pin test (above `claude_awareness_doc_contains_key_guidance`):

```rust
    /// Regression pin for the bundled Copilot `copilot-instructions.md`.
    /// Same intent as `claude_awareness_doc_contains_key_guidance` and
    /// `opencode_agents_md_contains_key_guidance`: the agent only
    /// reaches for the right tool if the doc actually contains the
    /// trigger phrases and tool table. A future edit that strips the
    /// guidance fails this test.
    #[test]
    fn copilot_instructions_md_contains_key_guidance() {
        let doc = COPILOT_INSTRUCTIONS_MD;
        assert!(
            doc.contains("When to use lain"),
            "copilot-instructions.md must have a 'When to use lain' section"
        );
        let required_tools = [
            "get_health",
            "find_anchors",
            "get_blast_radius",
            "trace_dependency",
            "semantic_search",
            "explain_symbol",
            "get_code_snippet",
            "find_dead_code",
            "get_coupling_radar",
        ];
        let missing: Vec<&str> = required_tools
            .iter()
            .filter(|name| !doc.contains(**name))
            .copied()
            .collect();
        assert!(
            missing.is_empty(),
            "copilot-instructions.md is missing tools: {missing:?}"
        );
        assert!(doc.contains("Workflows"), "missing 'Workflows' section");
        assert!(doc.contains("Caveats"), "missing 'Caveats' section");
    }
```

- [ ] **Step 4: Run the test to confirm it passes**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --bin lain copilot_instructions_md_contains_key_guidance
```

Expected: PASS (Step 1 created the file before Step 3, so the include_str resolves).

- [ ] **Step 5: Commit**

```bash
git add hooks/copilot/copilot-instructions.md agents/manifest.toml src/cmds/init.rs
git commit -m "feat(copilot): manifest entry and bundled copilot-instructions.md"
```

---

## Task 2: `CopilotAdapter` (adapter path)

**Files:**
- Create: `src/cmds/agents/adapters/copilot.rs`
- Modify: `src/cmds/agents/adapters/mod.rs` (register in `adapter_for`)

**Interfaces:**
- Consumes: `crate::cmds::agents::manifest::AgentEntry`, `InstallScope` from `super`.
- Produces: `pub fn build_copilot_lain_entry(embedding_model: Option<&Path>) -> serde_json::Value`; `CopilotAdapter` registered in `adapter_for`.

- [ ] **Step 1: Write the failing adapter tests**

Create `src/cmds/agents/adapters/copilot.rs` with the module skeleton and three failing tests at the bottom. (The `AgentEntry` struct fields are the full set; check `src/cmds/agents/manifest.rs` if the build fails to compile because of a missing field. The copilot row declares `format: "jsonc"` so the fixture must include `format`.)

```rust
use super::{expand_home, AdapterError, AgentAdapter, InstallScope};
use crate::cmds::agents::manifest::AgentEntry;
use serde_json::{json, Value};
use std::path::Path;

/// Build the `servers.lain` JSON value for VS Code / Copilot.
///
/// Verified from the VS Code and GitHub Copilot MCP docs: a local
/// stdio MCP server is `servers.<name>.{ command, args }` where
/// `command` is a string and `args` is an array. This is distinct from
/// OpenCode's array-`command` shape. `command: "lain"` is a bare
/// PATH-resolvable name.
pub fn build_copilot_lain_entry(embedding_model: Option<&Path>) -> Value {
    let mut args: Vec<String> = vec![
        "--workspace".to_string(),
        "auto".to_string(),
        "--transport".to_string(),
        "stdio".to_string(),
    ];
    if let Some(model) = embedding_model {
        args.push("--embedding-model".to_string());
        args.push(model.to_string_lossy().to_string());
    }
    json!({
        "command": "lain",
        "args": args,
    })
}

pub struct CopilotAdapter;

impl AgentAdapter for CopilotAdapter {
    fn id(&self) -> &'static str { "copilot" }

    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        todo!("implemented in Step 3")
    }

    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<Value, AdapterError> {
        todo!("implemented in Step 3")
    }

    fn remove(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        todo!("implemented in Step 3")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmds::agents::manifest::AgentEntry;

    // Use the SHARED `HOME_LOCK` from `cmds::agents::tests`, promoted to
    // `pub` in the opencode fix wave. All HOME-mutating tests in the
    // agents test tree must acquire this same lock to avoid the
    // pre-existing race that surfaced during the opencode fix wave.
    pub use super::super::super::tests::HOME_LOCK;

    fn entry() -> AgentEntry {
        AgentEntry {
            id: "copilot".to_string(),
            display_name: "GitHub Copilot in VS Code".to_string(),
            binary: "code".to_string(),
            detect_paths: vec!["~/.config/Code".to_string(), "~/.vscode".to_string()],
            config_user: "~/.copilot/mcp-config.json".to_string(),
            config_project: ".vscode/mcp.json".to_string(),
            config_format: "jsonc".to_string(),
            mcp_section: "servers".to_string(),
            mcp_name: "lain".to_string(),
            transport: "stdio".to_string(),
            command: "lain".to_string(),
            default_args: vec![],
            headless_probe: vec!["code".to_string(), "--version".to_string()],
        }
    }

    #[test]
    fn copilot_adapter_install_read_remove_round_trip() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let adapter = CopilotAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        let path = tmp.path().join(".copilot/mcp-config.json");
        assert!(path.exists(), "user-scope config not written");
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        let lain = doc.pointer("/servers/lain").expect("servers.lain present");
        assert_eq!(lain.get("command").and_then(|v| v.as_str()), Some("lain"));
        let args = lain.get("args").and_then(|v| v.as_array()).expect("args is array");
        let cmd_strs: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(cmd_strs.windows(2).any(|w| w == ["--workspace", "auto"]));
        assert!(cmd_strs.windows(2).any(|w| w == ["--transport", "stdio"]));
        // Read returns the same shape.
        let read_back = adapter.read(&e, InstallScope::User).unwrap();
        assert_eq!(read_back, lain.clone());
    }

    #[test]
    fn copilot_adapter_preserves_other_servers() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let path = tmp.path().join(".copilot/mcp-config.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "servers": {
                    "other-server": { "command": "x", "args": ["y"] }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let adapter = CopilotAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert!(
            doc.pointer("/servers/other-server").is_some(),
            "other-server must be preserved"
        );
        assert!(doc.pointer("/servers/lain").is_some(), "lain must be added");
    }

    #[test]
    fn copilot_adapter_remove_drops_only_lain() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let adapter = CopilotAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        // Pre-seed with another server.
        let path = tmp.path().join(".copilot/mcp-config.json");
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["servers"]["other-server"] = json!({ "command": "x", "args": ["y"] });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        adapter.remove(&e, InstallScope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert!(doc.pointer("/servers/lain").is_none(), "lain must be removed");
        assert!(doc.pointer("/servers/other-server").is_some(), "other-server preserved");
    }
}
```

- [ ] **Step 2: Run the tests to confirm they fail (or fail to compile)**

```bash
cargo test --bin lain copilot_adapter
```

Expected: tests fail (or `todo!()` panics). If `AgentEntry` is missing a field, the test won't compile; add the missing field with a sensible default (the opencode fix wave found `format: "json".to_string()` was required).

- [ ] **Step 3: Implement `install`, `read`, and `remove`**

Replace the three `todo!()` bodies in `src/cmds/agents/adapters/copilot.rs` with:

```rust
    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(raw_path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }) else {
            return Err(AdapterError::Unsupported(scope, self.id().into()));
        };
        let path = expand_home(raw_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut doc: Value = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
        } else {
            json!({})
        };
        let root = doc.as_object_mut()
            .ok_or_else(|| AdapterError::Shape("mcp.json root is not a JSON object".into()))?;
        let section = root.entry(entry.mcp_section.clone())
            .or_insert_with(|| json!({}));
        let section_obj = section.as_object_mut()
            .ok_or_else(|| AdapterError::Shape(format!("`{}` is not an object", entry.mcp_section)))?;
        // The adapter path doesn't have an embedding-model path; the init
        // path does. Without the model, Lain runs in stub embedder mode
        // (semantic search unavailable, every other tool works).
        section_obj.insert(entry.mcp_name.clone(), build_copilot_lain_entry(None));
        let serialized = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&path, serialized)?;
        Ok(())
    }

    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<Value, AdapterError> {
        let Some(raw_path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }) else {
            return Err(AdapterError::Unsupported(scope, self.id().into()));
        };
        let path = expand_home(raw_path);
        if !path.exists() { return Ok(Value::Null); }
        let raw = std::fs::read_to_string(&path)?;
        let doc: Value = serde_json::from_str(&raw)?;
        Ok(doc.pointer(&format!("/{}", entry.mcp_section))
            .and_then(|s| s.get(&entry.mcp_name)).cloned()
            .unwrap_or(Value::Null))
    }

    fn remove(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(raw_path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }) else {
            return Err(AdapterError::Unsupported(scope, self.id().into()));
        };
        let path = expand_home(raw_path);
        if !path.exists() { return Ok(()); }
        let raw = std::fs::read_to_string(&path)?;
        let mut doc: Value = serde_json::from_str(&raw)?;
        if let Some(section) = doc.get_mut(&entry.mcp_section).and_then(|v| v.as_object_mut()) {
            section.remove(&entry.mcp_name);
        }
        let serialized = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&path, serialized)?;
        Ok(())
    }
```

- [ ] **Step 4: Register the adapter in `adapter_for` and the module**

In `src/cmds/agents/adapters/mod.rs`:
- Add `pub mod copilot;` next to the other `pub mod` lines (around the `pub mod opencode;` line added in the opencode work).
- Add a new arm to `adapter_for` (next to the opencode arm):

```rust
        Some("copilot") => Ok(Box::new(super::copilot::CopilotAdapter)),
```

- [ ] **Step 5: Run the tests to confirm they pass**

```bash
cargo test --bin lain copilot_adapter
```

Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/cmds/agents/adapters/copilot.rs src/cmds/agents/adapters/mod.rs
git commit -m "feat(agents): add CopilotAdapter writing verified servers.lain entry"
```

---

## Task 3: `init_copilot` in `src/cmds/init.rs`

**Files:**
- Modify: `src/cmds/init.rs` (add `init_copilot`, dispatch, add `copilot` to `SUPPORTED_AGENTS`, add tests)

**Interfaces:**
- Consumes: `build_copilot_lain_entry` from `crate::cmds::agents::adapters::copilot`; `COPILOT_INSTRUCTIONS_MD` const (Task 1).
- Produces: `init_copilot(workspace, embedding_model, transport, port, yes, scope) -> Result<()>`. The `run_init` dispatch returns a `Result<()>` as before.

The `--scope` flag is already on the `Init` subcommand in `src/main.rs` and already threaded through `run_init` (added in the opencode work). This task only adds the new agent to the dispatch and to `SUPPORTED_AGENTS`.

- [ ] **Step 1: Add `copilot` to `SUPPORTED_AGENTS`**

In `src/cmds/init.rs`, find the `SUPPORTED_AGENTS` array (around line 7). Add `"copilot"` to the list (alphabetical or match the surrounding order; the opencode row is already there).

- [ ] **Step 2: Add the `copilot` arm to `run_init`**

In the `run_init` function in `src/cmds/init.rs`, find the `match agent` block. Add a new arm after the `"opencode"` arm (or wherever fits the surrounding order):

```rust
        "copilot" => {
            init_copilot(
                workspace,
                embedding_model,
                transport,
                port,
                yes,
                scope,
            )?;
        }
```

- [ ] **Step 3: Write the failing `init_copilot` tests**

In `src/cmds/init.rs`'s `mod tests` block, add a `HomeGuard` struct (the opencode fix wave found it can't be shared across the bin and e2e test binaries, so we duplicate it locally — same pattern as `tests/e2e_opencode.rs`) and the six `init_copilot` tests. Place them next to the opencode tests for grouping.

```rust
    /// Mirrors the `HomeGuard` in `tests/e2e_copilot.rs` and the one in
    /// the opencode fix wave. Each test binary duplicates the type
    /// because Rust integration tests cannot share `#[cfg(test)]` items
    /// across binaries. Panic-safe HOME restore: `Drop` runs even on
    /// assertion failure, so HOME never leaks past the test.
    struct HomeGuard(Option<String>);
    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let prev = std::env::var("HOME").ok();
            std::env::set_var("HOME", path);
            Self(prev)
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn temp_git_workspace_copilot() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&ws).status().unwrap();
        (tmp, ws)
    }

    #[test]
    fn init_copilot_writes_verified_mcp_config() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        init_copilot(&ws, None, "stdio", 0, true, "project").unwrap();
        let body = std::fs::read_to_string(ws.join(".vscode/mcp.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
        let lain = doc.pointer("/servers/lain").expect("servers.lain present");
        assert_eq!(lain["command"], "lain");
        let args = lain["args"].as_array().expect("args is JSON array");
        let cmd: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(cmd.windows(2).any(|w| w == ["--workspace", "auto"]));
        assert!(cmd.windows(2).any(|w| w == ["--transport", "stdio"]));
    }

    #[test]
    fn init_copilot_includes_embedding_model_when_provided() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        let model = std::path::Path::new("/models/all-MiniLM-L6-v2.onnx");
        init_copilot(&ws, Some(model), "stdio", 0, true, "project").unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join(".vscode/mcp.json")).unwrap()).unwrap();
        let cmd: Vec<String> = doc.pointer("/servers/lain/args").unwrap().as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let idx = cmd.iter().position(|s| s == "--embedding-model").expect("--embedding-model present");
        assert_eq!(cmd[idx + 1], "/models/all-MiniLM-L6-v2.onnx");
    }

    #[test]
    fn init_copilot_writes_copilot_instructions_md_in_project_root() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        init_copilot(&ws, None, "stdio", 0, true, "project").unwrap();
        let instructions = ws.join(".github/copilot-instructions.md");
        assert!(instructions.exists(), ".github/copilot-instructions.md must be written to project root");
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(body.contains("When to use lain"));
        assert!(body.contains("find_anchors"));
    }

    #[test]
    fn init_copilot_scope_user_writes_global_config() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&ws).status().unwrap();
        let _home_guard = HomeGuard::set(tmp.path());
        init_copilot(&ws, None, "stdio", 0, true, "user").unwrap();
        drop(_home_guard); // restore HOME before assertions, in case a later assert panics

        let global = tmp.path().join(".copilot/mcp-config.json");
        assert!(global.exists(), "user-scope must write ~/.copilot/mcp-config.json");
        assert!(!ws.join(".vscode/mcp.json").exists(), "user-scope must NOT write project .vscode/mcp.json");
        assert!(!ws.join(".github/copilot-instructions.md").exists(), "user-scope must NOT write project awareness doc");
    }

    #[test]
    fn init_copilot_merges_with_existing_mcp_json() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        std::fs::write(
            ws.join(".vscode/mcp.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "servers": {
                    "other-server": { "command": "x", "args": ["y"] }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        init_copilot(&ws, None, "stdio", 0, true, "project").unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join(".vscode/mcp.json")).unwrap()).unwrap();
        assert!(doc.pointer("/servers/other-server").is_some(), "other-server preserved");
        assert!(doc.pointer("/servers/lain").is_some(), "lain added");
    }

    #[test]
    fn init_copilot_does_not_overwrite_existing_instructions_md_without_yes() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        let instructions = ws.join(".github/copilot-instructions.md");
        std::fs::create_dir_all(instructions.parent().unwrap()).unwrap();
        std::fs::write(&instructions, "# my custom instructions\n").unwrap();
        init_copilot(&ws, None, "stdio", 0, false, "project").unwrap();
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert_eq!(body, "# my custom instructions\n", "existing awareness doc must be preserved when yes=false");
    }

    #[test]
    fn init_copilot_yes_overwrites_existing_instructions_md() {
        let (_tmp, ws) = temp_git_workspace_copilot();
        let instructions = ws.join(".github/copilot-instructions.md");
        std::fs::create_dir_all(instructions.parent().unwrap()).unwrap();
        std::fs::write(&instructions, "# my custom instructions\n").unwrap();
        init_copilot(&ws, None, "stdio", 0, true, "project").unwrap();
        let body = std::fs::read_to_string(&instructions).unwrap();
        assert!(body.contains("When to use lain"), "yes=true must replace with the bundled doc");
    }
```

- [ ] **Step 4: Run the tests to confirm they fail (compile error on `init_copilot`)**

```bash
cargo test --bin lain init_copilot
```

Expected: compile error `cannot find function init_copilot`.

- [ ] **Step 5: Implement `init_copilot`**

In `src/cmds/init.rs`, above the `mod tests` block, add the function:

```rust
/// Install Lain for GitHub Copilot in VS Code. Writes `.vscode/mcp.json`
/// (MCP config) and, when `scope == "project"`, `.github/copilot-instructions.md`
/// (awareness doc) in the workspace root. When `scope == "user"`, writes
/// the global `~/.copilot/mcp-config.json` and skips the awareness doc
/// (a per-repo convention, inappropriate to write globally).
fn init_copilot(
    workspace: &std::path::Path,
    embedding_model: Option<&std::path::Path>,
    _transport: &str,
    _port: u16,
    yes: bool,
    scope: &str,
) -> Result<()> {
    if scope != "project" && scope != "user" {
        anyhow::bail!(
            "init_copilot: --scope must be 'project' or 'user', got '{}'",
            scope
        );
    }
    use crate::cmds::agents::adapters::copilot::build_copilot_lain_entry;

    let target_path: std::path::PathBuf = if scope == "project" {
        workspace.join(".vscode/mcp.json")
    } else {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        home.join(".copilot/mcp-config.json")
    };
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut doc: serde_json::Value = if target_path.exists() {
        let raw = std::fs::read_to_string(&target_path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    {
        let root = doc.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcp.json root is not a JSON object"))?;
        let section = root.entry("servers".to_string())
            .or_insert_with(|| serde_json::json!({}));
        let section_obj = section.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("`servers` is not an object"))?;
        section_obj.insert("lain".to_string(), build_copilot_lain_entry(embedding_model));
    }
    let serialized = serde_json::to_string_pretty(&doc)?;
    std::fs::write(&target_path, serialized)?;
    println!("Wrote GitHub Copilot/VS Code MCP config to {}", target_path.display());

    if scope == "project" {
        let instructions_dir = workspace.join(".github");
        std::fs::create_dir_all(&instructions_dir)?;
        let instructions_path = instructions_dir.join("copilot-instructions.md");
        if instructions_path.exists() && !yes {
            println!(
                "Copilot instructions file already exists at {} - skipped.",
                instructions_path.display()
            );
        } else {
            std::fs::write(&instructions_path, COPILOT_INSTRUCTIONS_MD)?;
            println!("Wrote GitHub Copilot awareness doc to {}", instructions_path.display());
        }
    }

    Ok(())
}
```

- [ ] **Step 6: Run the tests to confirm they pass**

```bash
cargo test --bin lain init_copilot copilot_instructions_md
```

Expected: 7 new tests pass (6 init_copilot + 1 AGENTS.md content pin).

- [ ] **Step 7: Run the full test sweep to confirm no regressions**

```bash
cargo test --lib
cargo test --bin lain cmds::init::tests cmds::agents
```

Expected: green. (The combined `cmds::init::tests cmds::agents` filter needs `--` to pass two filters; use `cargo test --bin lain -- cmds::init::tests cmds::agents`.)

- [ ] **Step 8: Commit**

```bash
git add src/cmds/init.rs
git commit -m "feat(init): add init_copilot writing verified servers.lain and copilot-instructions.md"
```

---

## Task 4: End-to-end test (`tests/e2e_copilot.rs`)

**Files:**
- Create: `tests/e2e_copilot.rs`

**Interfaces:**
- Consumes: `lain_bin()` = `PathBuf::from(env!("CARGO_BIN_EXE_lain"))`.
- Produces: a passing `cargo test --test e2e_copilot` run.

- [ ] **Step 1: Create the e2e test file**

```rust
//! End-to-end test for `lain init --agent copilot`.
//!
//! Runs the real binary in a temp git repo and verifies the produced
//! `.vscode/mcp.json` matches the VS Code and GitHub Copilot MCP schema
//! at <https://code.visualstudio.com/docs/agent-customization/mcp-servers>.

use std::path::PathBuf;
use std::process::Command;

fn lain_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

fn git_init_quiet(path: &std::path::Path) {
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(path)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed for {}", path.display());
}

/// Mirrors `HomeGuard` in `src/cmds/init.rs::tests`. Each test binary
/// duplicates the type because integration tests cannot share `#[cfg(test)]`
/// items. Panic-safe HOME restore.
struct HomeGuard(Option<String>);
impl HomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", path);
        Self(prev)
    }
}
impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn lain_init_copilot_writes_verified_mcp_json_and_instructions_md() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);

    let status = Command::new(lain_bin())
        .args(["init", "--agent", "copilot", "--yes"])
        .args(["--workspace", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .expect("spawn lain init");
    assert!(status.success(), "lain init exited with {status:?}");

    let body = std::fs::read_to_string(repo.join(".vscode/mcp.json")).expect("read .vscode/mcp.json");
    let doc: serde_json::Value = serde_json::from_str(&body).expect("parse .vscode/mcp.json");

    let lain = doc.pointer("/servers/lain").expect("servers.lain present");
    assert_eq!(lain["command"], "lain");
    let args = lain["args"].as_array().expect("args is a JSON array");
    let cmd: Vec<String> = args.iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(cmd.first().map(String::as_str), Some("lain"));
    assert!(cmd.windows(2).any(|w| w == ["--workspace", "auto"]));
    assert!(cmd.windows(2).any(|w| w == ["--transport", "stdio"]));

    let instructions = repo.join(".github/copilot-instructions.md");
    assert!(instructions.exists(), "copilot-instructions.md not written");
    let body = std::fs::read_to_string(&instructions).expect("read copilot-instructions.md");
    assert!(body.contains("When to use lain"));
    assert!(body.contains("find_anchors"));
}

#[test]
fn lain_init_copilot_scope_user_writes_global_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);

    let _home_guard = HomeGuard::set(tmp.path());

    let status = Command::new(lain_bin())
        .args(["init", "--agent", "copilot", "--yes", "--scope", "user"])
        .args(["--workspace", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .expect("spawn lain init");
    assert!(status.success(), "lain init exited with {status:?}");
    drop(_home_guard); // restore HOME before assertions

    let global = tmp.path().join(".copilot/mcp-config.json");
    assert!(global.exists(), "user-scope must write ~/.copilot/mcp-config.json");
    assert!(!repo.join(".vscode/mcp.json").exists(), "user-scope must NOT write project .vscode/mcp.json");
    assert!(!repo.join(".github/copilot-instructions.md").exists(), "user-scope must NOT write project awareness doc");
}
```

- [ ] **Step 2: Run the e2e test**

```bash
cargo test --test e2e_copilot
```

Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e_copilot.rs
git commit -m "test(e2e): verify lain init --agent copilot writes verified servers.lain"
```

---

## Task 5: Full test sweep + manual smoke check

- [ ] **Step 1: Run the full library test suite**

```bash
cargo test --lib
```

Expected: same count as before, 0 failed.

- [ ] **Step 2: Run the bin test suite (init + agents)**

```bash
cargo test --bin lain -- cmds::init::tests
cargo test --bin lain -- cmds::agents
```

Expected: all green. The init suite should be roughly 20 (prior 13 + 6 init_copilot + the copilot_instructions_md content pin added in Task 1). The agents suite should be roughly 14 (prior 11 + 3 copilot_adapter).

- [ ] **Step 3: Run the e2e tests**

```bash
cargo test --test e2e_copilot
cargo test --test e2e_opencode
cargo test --test e2e_portable
cargo test --test e2e_agents
cargo test --test auto_workspace
```

Expected: all green.

- [ ] **Step 4: Commit any final fixes**

If adjustments were needed, commit them with a `chore:` or `fix:` message.

- [ ] **Step 5: Do NOT push.**

Stop here. The user (per their instruction "when is done and tested with a real install, test with copilot and vs code") will manually verify by opening a repo in VS Code + Copilot, then we'll push the whole batch (OpenCode + Copilot/VS Code) at once.

---

## Out of Scope

- **Migrating any existing agent** — `copilot` is a new id.
- **VS Code's `settings.json` MCP namespace** (legacy `chat.mcp.discovery` import from Claude Desktop) — `~/.copilot/mcp-config.json` is the supported portable path.
- **Sandboxed MCP server config** — not needed for Lain.
- **Remote/HTTP MCP servers** — Lain is stdio only.
- **Windows-specific path quirks** — `dirs::home_dir()` already handles Windows; the `~/.copilot/mcp-config.json` path expands correctly.
- **Live VS Code / Copilot behavior test** — gated `#[ignore]` + `RUN_E2E_BEHAVIOR=1`, spawns `code` headless. Add only after the user confirms a real VS Code + Copilot install works with the produced config.
