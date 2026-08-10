# OpenCode Agent Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `lain init --agent opencode` and `lain agents install opencode` configure the OpenCode terminal/IDE agent with a verified `opencode.json` MCP config and an `AGENTS.md` awareness doc, so OpenCode both *can* and *knows when to* reach for Lain.

**Architecture:** Add a new `opencode` agent id to the manifest, an `OpenCodeAdapter` for the `agents install` path, and an `init_opencode` function for the `init` path. Both write a JSON `mcp.lain` entry with `type: "local"` and a `command` **array** (the load-bearing detail — OpenCode rejects string commands). A bundled `hooks/opencode/AGENTS.md` is written to the project root. A `--scope` flag on `Init` selects per-project (default) vs user-global config.

**Tech Stack:** Rust (bin crate), clap, serde_json, dirs, tempfile (dev), git2 (already a dep).

## Global Constraints

- **OpenCode `command` is a JSON array** of `[executable, arg1, arg2, ...]`, not a string. Element 0 is `"lain"` (bare PATH-resolvable name). This is the load-bearing detail from [opencode.ai/docs/mcp-servers](https://opencode.ai/docs/mcp-servers/); a string `command` is invalid and OpenCode will reject the config.
- **Local server schema** (verbatim from the OpenCode docs): `mcp.<name>.{ type: "local", command: [...], enabled: bool?, environment: object?, timeout: number?, cwd: string? }`. We always set `type: "local"`, `enabled: true`, `timeout: 30000` (overrides the 5-second default; Lain needs ~5–10s to load the NLP model on cold start).
- **`--workspace auto` works for OpenCode without a wrapper** because OpenCode launches MCP subprocesses with `cwd` set to the project root. (Kimi needed a `/proc/$PPID/cwd` wrapper; OpenCode does not.)
- **`omp` is oh-my-pi, not OpenCode.** The existing `omp` adapter and `init_omp` are untouched. This plan adds a new `opencode` id.
- **No `opencode mcp add` CLI** exists. We write `opencode.json` directly, mirroring how `init_gemini` / `init_cursor` etc. write their config files.
- **`--scope` is only honored by `init_opencode` in this work.** Other agents (claude, kimi, gemini, cursor, windsurf, cline, omp) ignore it; extending them is a separate change. The flag is accepted by the `Init` subcommand but silently unused for non-opencode agents.

---

## File Structure

| File | Change | Responsibility |
|------|--------|----------------|
| `agents/manifest.toml` | modify | Add `[[agent]]` block for OpenCode. |
| `src/cmds/agents/adapters/opencode.rs` | create | `OpenCodeAdapter` + `pub fn build_opencode_lain_entry` (shared builder). |
| `src/cmds/agents/adapters/mod.rs` | modify | Register the adapter in `adapter_for`; re-export the builder. |
| `src/cmds/init.rs` | modify | `init_opencode`, dispatch, tests, awareness content regression test. |
| `src/main.rs` | modify | Add `--scope` to `Init`; thread into `run_init`. |
| `hooks/opencode/AGENTS.md` | create | Bundled awareness doc. |
| `tests/e2e_opencode.rs` | create | End-to-end real-install test. |

---

## Task 1: Manifest entry + bundled `AGENTS.md` awareness doc

**Files:**
- Create: `hooks/opencode/AGENTS.md`
- Modify: `agents/manifest.toml`
- Test: `src/cmds/init.rs` (content pin for the bundled doc, like `claude_awareness_doc_contains_key_guidance`)

**Interfaces:**
- Consumes: nothing.
- Produces: `const OPENCODE_AGENTS_MD: &str` in `src/cmds/init.rs` (used by Task 3); the manifest row for `opencode`.

- [ ] **Step 1: Create `hooks/opencode/AGENTS.md`**

Write this file with the exact content below:

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

In `agents/manifest.toml`, append a new `[[agent]]` block (the file uses a top-level `[[agent]]` table array; keep the same style as the other entries). Add:

```toml
[[agent]]
id = "opencode"
display_name = "OpenCode"
binary = "opencode"
detect_paths = ["~/.config/opencode"]
config_user = "~/.config/opencode/opencode.json"
config_project = "opencode.json"
config_format = "jsonc"
mcp_section = "mcp"
mcp_name = "lain"
transport = "stdio"
command = "lain"
default_args = []
headless_probe = ["opencode", "--version"]
```

- [ ] **Step 3: Add the content pin test (red)**

In `src/cmds/init.rs`, near the top of the `mod tests` block (above the `claude_awareness_doc_contains_key_guidance` test), add:

```rust
const OPENCODE_AGENTS_MD: &str = include_str!("../../hooks/opencode/AGENTS.md");

/// Regression pin for the bundled OpenCode `AGENTS.md`. The agent only
/// reaches for the right tool if the doc actually contains the trigger
/// phrases and tool table. Asserts the structural shape so a future edit
/// can't silently strip the guidance without a test failure.
#[test]
fn opencode_agents_md_contains_key_guidance() {
    let doc = OPENCODE_AGENTS_MD;
    assert!(
        doc.contains("When to use lain"),
        "AGENTS.md must have a 'When to use lain' section"
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
        "AGENTS.md is missing tools: {missing:?}"
    );
    assert!(doc.contains("Workflows"), "missing 'Workflows' section");
    assert!(doc.contains("Caveats"), "missing 'Caveats' section");
}
```

- [ ] **Step 4: Run the test to confirm it fails**

Run:
```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --bin lain opencode_agents_md_contains_key_guidance
```

Expected: compile error `no file named "../../hooks/opencode/AGENTS.md"` (because the file is created in Step 1, which comes first; this step ordering is intentional — we want the test to fail before the file exists, then the next run passes after the file lands). If the test compiles and fails at runtime, that is also acceptable; the goal is the test references the bundled bytes.

- [ ] **Step 5: Run the test to confirm it passes**

After Step 1 has created the file, run the same command again:
```bash
cargo test --bin lain opencode_agents_md_contains_key_guidance
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add hooks/opencode/AGENTS.md agents/manifest.toml src/cmds/init.rs
git commit -m "feat(opencode): manifest entry and bundled AGENTS.md awareness doc"
```

---

## Task 2: `OpenCodeAdapter` (adapter path)

**Files:**
- Create: `src/cmds/agents/adapters/opencode.rs`
- Modify: `src/cmds/agents/adapters/mod.rs` (register in `adapter_for`)
- Test: unit tests inside the new file (or in `mod.rs`'s test block) — `opencode_adapter_install_read_remove_round_trip`, `opencode_adapter_preserves_other_mcp_servers`

**Interfaces:**
- Consumes: `crate::cmds::agents::manifest::AgentEntry`, `InstallScope` from `super`.
- Produces: `pub fn build_opencode_lain_entry(embedding_model: Option<&Path>) -> serde_json::Value` (re-used by Task 3); `OpenCodeAdapter` registered in `adapter_for`.

- [ ] **Step 1: Write the failing adapter tests**

Create `src/cmds/agents/adapters/opencode.rs` with the module skeleton and the failing tests at the bottom. (The tests live in the same file so they have direct access to the adapter types.)

```rust
use super::{expand_home, AdapterError, AgentAdapter, InstallScope};
use crate::cmds::agents::manifest::AgentEntry;
use serde_json::{json, Value};
use std::path::Path;

/// Build the `mcp.lain` JSON value for OpenCode's `opencode.json`.
///
/// Verified against the schema at <https://opencode.ai/docs/mcp-servers>:
/// `command` is an **Array** `[executable, arg1, arg2, ...]` — a
/// string `command` is invalid. We always set `type: "local"`,
/// `enabled: true`, and `timeout: 30000` (the default 5000ms is too
/// short for Lain's cold-start NLP model load).
pub fn build_opencode_lain_entry(embedding_model: Option<&Path>) -> Value {
    let mut command: Vec<String> = vec![
        "lain".to_string(),
        "--workspace".to_string(),
        "auto".to_string(),
        "--transport".to_string(),
        "stdio".to_string(),
    ];
    if let Some(model) = embedding_model {
        command.push("--embedding-model".to_string());
        command.push(model.to_string_lossy().to_string());
    }
    json!({
        "type": "local",
        "command": command,
        "enabled": true,
        "timeout": 30000
    })
}

pub struct OpenCodeAdapter;

impl AgentAdapter for OpenCodeAdapter {
    fn id(&self) -> &'static str { "opencode" }

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

    fn entry() -> AgentEntry {
        // Minimal manifest row. Only the fields the adapter reads matter here.
        AgentEntry {
            id: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
            binary: "opencode".to_string(),
            detect_paths: vec!["~/.config/opencode".to_string()],
            config_user: "~/.config/opencode/opencode.json".to_string(),
            config_project: "opencode.json".to_string(),
            config_format: "jsonc".to_string(),
            mcp_section: "mcp".to_string(),
            mcp_name: "lain".to_string(),
            transport: "stdio".to_string(),
            command: "lain".to_string(),
            default_args: vec![],
            headless_probe: vec!["opencode".to_string(), "--version".to_string()],
        }
    }

    #[test]
    fn opencode_adapter_install_read_remove_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        // Redirect HOME so the user-scope path lives inside the tempdir.
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        let adapter = OpenCodeAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        let path = tmp.path().join(".config/opencode/opencode.json");
        assert!(path.exists(), "config file not written");
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        let lain = doc.pointer("/mcp/lain").expect("mcp.lain present");
        assert_eq!(lain.get("type").and_then(|v| v.as_str()), Some("local"));
        let cmd = lain.get("command").and_then(|v| v.as_array()).expect("command is array");
        let cmd_strs: Vec<String> = cmd.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(cmd_strs.first().map(String::as_str), Some("lain"));
        assert!(cmd_strs.windows(2).any(|w| w == ["--workspace", "auto"]));
        assert!(cmd_strs.windows(2).any(|w| w == ["--transport", "stdio"]));
        assert_eq!(lain.get("enabled"), Some(&Value::Bool(true)));
        assert_eq!(lain.get("timeout"), Some(&json!(30000)));

        // Read returns the same shape.
        let read_back = adapter.read(&e, InstallScope::User).unwrap();
        assert_eq!(read_back, lain);

        if let Some(h) = original_home { std::env::set_var("HOME", h); }
    }

    #[test]
    fn opencode_adapter_preserves_other_mcp_servers() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let path = tmp.path().join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "mcp": {
                    "other-server": { "type": "local", "command": ["x"], "enabled": true }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let adapter = OpenCodeAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert!(
            doc.pointer("/mcp/other-server").is_some(),
            "other-server must be preserved"
        );
        assert!(doc.pointer("/mcp/lain").is_some(), "lain must be added");
    }

    #[test]
    fn opencode_adapter_remove_drops_only_lain() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());
        let adapter = OpenCodeAdapter;
        let e = entry();
        adapter.install(&e, InstallScope::User).unwrap();
        // Pre-seed with another server so we can assert remove preserves it.
        let path = tmp.path().join(".config/opencode/opencode.json");
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["mcp"]["other-server"] = json!({ "type": "local", "command": ["x"], "enabled": true });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        adapter.remove(&e, InstallScope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert!(doc.pointer("/mcp/lain").is_none(), "lain must be removed");
        assert!(doc.pointer("/mcp/other-server").is_some(), "other-server preserved");
    }
}
```

(The `AgentEntry` struct fields above are the full set; check the struct in `src/cmds/agents/manifest.rs` and adjust if a field is missing or named differently — the test will fail to compile if so.)

- [ ] **Step 2: Run the tests to confirm they fail**

```bash
cargo test --bin lain opencode_adapter
```

Expected: tests fail (or fail to compile if `AgentEntry` fields differ). The `todo!()` bodies cause runtime panics; that's the failing signal.

- [ ] **Step 3: Implement `install` and `read`**

Replace the `todo!()` bodies in `src/cmds/agents/adapters/opencode.rs` with:

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
            .ok_or_else(|| AdapterError::Shape("opencode.json root is not a JSON object".into()))?;
        let mcp = root.entry("mcp".to_string()).or_insert_with(|| json!({}));
        let mcp_obj = mcp.as_object_mut()
            .ok_or_else(|| AdapterError::Shape("opencode.json `mcp` is not an object".into()))?;
        // The adapter path doesn't have an embedding-model path; the init
        // path does. Without the model, Lain runs in stub embedder mode
        // (semantic search unavailable, every other tool works).
        mcp_obj.insert("lain".to_string(), build_opencode_lain_entry(None));
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
        if let Some(mcp) = doc.get_mut("mcp").and_then(|v| v.as_object_mut()) {
            mcp.remove("lain");
        }
        let serialized = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&path, serialized)?;
        Ok(())
    }
```

- [ ] **Step 4: Register the adapter in `adapter_for`**

In `src/cmds/agents/adapters/mod.rs`, add a new arm to `adapter_for`:

```rust
        Some("opencode") => Ok(Box::new(super::opencode::OpenCodeAdapter)),
```

(Place it next to the other adapter arms.)

- [ ] **Step 5: Run the tests to confirm they pass**

```bash
cargo test --bin lain opencode_adapter
```

Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/cmds/agents/adapters/opencode.rs src/cmds/agents/adapters/mod.rs
git commit -m "feat(agents): add OpenCodeAdapter writing verified opencode.json mcp entry"
```

---

## Task 3: `init_opencode` in `src/cmds/init.rs` + `--scope` in `src/main.rs`

**Files:**
- Modify: `src/cmds/init.rs` (add `init_opencode`, dispatch in `run_init`, tests)
- Modify: `src/main.rs` (add `--scope` to `Init`, thread into `run_init`)

**Interfaces:**
- Consumes: `build_opencode_lain_entry` from `crate::cmds::agents::adapters::opencode`.
- Produces: `init_opencode(workspace, embedding_model, transport, port, yes, scope) -> Result<()>`. The `run_init` dispatch returns a `Result<()>` as before.

- [ ] **Step 1: Add `--scope` to the `Init` subcommand in `src/main.rs`**

Locate the `Init` variant (currently around line 39). Add the `scope` field. The current variant:

```rust
    Init {
        #[arg(long, default_value = "auto")] agent: String,
        #[arg(long)] workspace: Option<std::path::PathBuf>,
        #[arg(long)] embedding_model: Option<std::path::PathBuf>,
        #[arg(long, default_value = "stdio")] transport: String,
        #[arg(long, default_value = "9999")] port: u16,
        #[arg(long, short)] yes: bool,
    },
```

Add `scope` to the end:

```rust
    Init {
        #[arg(long, default_value = "auto")] agent: String,
        #[arg(long)] workspace: Option<std::path::PathBuf>,
        #[arg(long)] embedding_model: Option<std::path::PathBuf>,
        #[arg(long, default_value = "stdio")] transport: String,
        #[arg(long, default_value = "9999")] port: u16,
        #[arg(long, short)] yes: bool,
        /// Where to write the agent's MCP config: `project` (in-repo) or
        /// `user` (global, e.g. `~/.config/...`). Currently only honored
        /// by `--agent opencode`; other agents ignore it.
        #[arg(long, default_value = "project", value_parser = ["project", "user"])]
        scope: String,
    },
```

- [ ] **Step 2: Thread `scope` through `run_init`**

In `src/main.rs`, update the `Init` dispatch (currently around line 147):

```rust
            Commands::Init { agent, workspace, embedding_model, transport, port, yes, scope } => {
                let workspace = workspace.unwrap_or(args.workspace);
                let resolved = resolve_workspace_path(&workspace);
                return cmds::run_init(&agent, Some(&resolved), embedding_model.as_deref(), &transport, port, yes, &scope);
            }
```

In `src/cmds/init.rs`, update `run_init`'s signature to accept `scope`:

```rust
pub fn run_init(
    agent: &str,
    workspace: Option<&std::path::Path>,
    embedding_model: Option<&std::path::Path>,
    transport: &str,
    port: u16,
    yes: bool,
    scope: &str,
) -> Result<()> {
```

The body of `run_init` currently dispatches to `init_claude`, `init_kimi`, etc. Add a new dispatch for `"opencode"` *after* the `omp` arm (or wherever fits the alphabetical order — match the surrounding style). The new arm:

```rust
        "opencode" => {
            init_opencode(
                workspace,
                embedding_model,
                transport,
                port,
                yes,
                scope,
            )?;
        }
```

`init_opencode` will be defined in Step 5. The dispatch just calls it with the resolved `workspace` (a `&Path`) and the `scope` value.

- [ ] **Step 3: Write the failing tests for `init_opencode`**

In `src/cmds/init.rs`, add tests at the end of the `mod tests` block. These all run with a tempdir; project-scope tests pass the project root as the workspace; user-scope tests redirect `HOME` to the tempdir.

```rust
    use crate::cmds::agents::adapters::opencode::build_opencode_lain_entry;

    fn temp_git_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&ws).status().unwrap();
        (tmp, ws)
    }

    #[test]
    fn init_opencode_writes_verified_mcp_config() {
        let (_tmp, ws) = temp_git_workspace();
        init_opencode(&ws, None, "stdio", 0, true, "project").unwrap();
        let body = std::fs::read_to_string(ws.join("opencode.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
        let lain = doc.pointer("/mcp/lain").expect("mcp.lain present");
        assert_eq!(lain["type"], "local");
        let cmd = lain["command"].as_array().expect("command is JSON array");
        let cmd: Vec<String> = cmd.iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(cmd.first().map(String::as_str), Some("lain"));
        assert!(cmd.windows(2).any(|w| w == ["--workspace", "auto"]));
        assert!(cmd.windows(2).any(|w| w == ["--transport", "stdio"]));
        assert_eq!(lain["enabled"], true);
        assert_eq!(lain["timeout"], 30000);
    }

    #[test]
    fn init_opencode_includes_embedding_model_when_provided() {
        let (_tmp, ws) = temp_git_workspace();
        let model = std::path::Path::new("/models/all-MiniLM-L6-v2.onnx");
        init_opencode(&ws, Some(model), "stdio", 0, true, "project").unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join("opencode.json")).unwrap()).unwrap();
        let cmd: Vec<String> = doc.pointer("/mcp/lain/command").unwrap().as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let idx = cmd.iter().position(|s| s == "--embedding-model").expect("--embedding-model present");
        assert_eq!(cmd[idx + 1], "/models/all-MiniLM-L6-v2.onnx");
    }

    #[test]
    fn init_opencode_writes_agents_md_in_project_root() {
        let (_tmp, ws) = temp_git_workspace();
        init_opencode(&ws, None, "stdio", 0, true, "project").unwrap();
        let agents = ws.join("AGENTS.md");
        assert!(agents.exists(), "AGENTS.md must be written to project root");
        let body = std::fs::read_to_string(&agents).unwrap();
        assert!(body.contains("When to use lain"));
        assert!(body.contains("find_anchors"));
    }

    #[test]
    fn init_opencode_scope_user_writes_global_config() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        Command::new("git").args(["init", "--quiet"]).current_dir(&ws).status().unwrap();
        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", tmp.path());
        init_opencode(&ws, None, "stdio", 0, true, "user").unwrap();
        if let Some(h) = &original_home { std::env::set_var("HOME", h); } else { std::env::remove_var("HOME"); }

        let global = tmp.path().join(".config/opencode/opencode.json");
        assert!(global.exists(), "user-scope must write ~/.config/opencode/opencode.json");
        assert!(!ws.join("opencode.json").exists(), "user-scope must NOT write project config");
        assert!(!ws.join("AGENTS.md").exists(), "user-scope must NOT write AGENTS.md");
    }

    #[test]
    fn init_opencode_merges_with_existing_opencode_json() {
        let (_tmp, ws) = temp_git_workspace();
        std::fs::write(
            ws.join("opencode.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "mcp": {
                    "other-server": { "type": "local", "command": ["x"], "enabled": true }
                },
                "$schema": "https://opencode.ai/config.json"
            }))
            .unwrap(),
        )
        .unwrap();
        init_opencode(&ws, None, "stdio", 0, true, "project").unwrap();
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(ws.join("opencode.json")).unwrap()).unwrap();
        assert!(doc.pointer("/mcp/other-server").is_some(), "other-server preserved");
        assert!(doc.pointer("/mcp/lain").is_some(), "lain added");
        assert_eq!(doc["$schema"], "https://opencode.ai/config.json", "other top-level keys preserved");
    }
```

Add the `build_opencode_lain_entry` import at the top of the test module as shown.

- [ ] **Step 4: Run the tests to confirm they fail**

```bash
cargo test --bin lain init_opencode
```

Expected: compile error `cannot find function init_opencode` (since Step 5 hasn't implemented it yet).

- [ ] **Step 5: Implement `init_opencode`**

In `src/cmds/init.rs`, add the function (above the `mod tests` block). Use the shared builder from Task 2:

```rust
/// Install Lain for OpenCode. Writes `opencode.json` (MCP config) and,
/// when `scope == "project"`, `AGENTS.md` (awareness doc) in the
/// workspace root. When `scope == "user"`, writes the global
/// `~/.config/opencode/opencode.json` and skips `AGENTS.md` (a
/// per-project convention, inappropriate to write globally).
fn init_opencode(
    workspace: &std::path::Path,
    embedding_model: Option<&std::path::Path>,
    _transport: &str,
    _port: u16,
    _yes: bool,
    scope: &str,
) -> Result<()> {
    if scope != "project" && scope != "user" {
        anyhow::bail!(
            "init_opencode: --scope must be 'project' or 'user', got '{}'",
            scope
        );
    }
    use crate::cmds::agents::adapters::opencode::build_opencode_lain_entry;

    let target_path: std::path::PathBuf = if scope == "project" {
        workspace.join("opencode.json")
    } else {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        home.join(".config/opencode/opencode.json")
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
        let schema = doc.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("opencode.json root is not a JSON object"))?;
        let mcp = schema.entry("mcp".to_string()).or_insert_with(|| serde_json::json!({}));
        let mcp_obj = mcp.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("opencode.json `mcp` is not an object"))?;
        mcp_obj.insert("lain".to_string(), build_opencode_lain_entry(embedding_model));
    }
    let serialized = serde_json::to_string_pretty(&doc)?;
    std::fs::write(&target_path, serialized)?;
    println!("Wrote OpenCode MCP config to {}", target_path.display());

    if scope == "project" {
        let agents_path = workspace.join("AGENTS.md");
        std::fs::write(&agents_path, OPENCODE_AGENTS_MD)?;
        println!("Wrote OpenCode awareness doc to {}", agents_path.display());
    }

    Ok(())
}
```

The `_transport`/`_port`/`_yes` parameters are accepted for signature parity with the other `init_*` functions; the spec notes that OpenCode's local MCP is stdio-only and the port flag is HTTP-only, so we don't use them here. (If the team later wants to add HTTP MCP support to OpenCode via this installer, the parameters are already in place.)

- [ ] **Step 6: Run the init tests to confirm they pass**

```bash
cargo test --bin lain init_opencode opencode_agents_md
```

Expected: 6 tests pass (5 new init_opencode + 1 AGENTS.md content pin).

- [ ] **Step 7: Run the full test suite to confirm no regressions**

```bash
cargo test --lib
cargo test --bin lain cmds::init::tests cmds::agents
cargo test --test e2e_agents
cargo test --test e2e_portable
cargo test --test auto_workspace
```

Expected: all green, same totals as before plus the new tests.

- [ ] **Step 8: Commit**

```bash
git add src/cmds/init.rs src/main.rs
git commit -m "feat(init): add init_opencode writing verified mcp config and AGENTS.md"
```

---

## Task 4: End-to-end test (`tests/e2e_opencode.rs`)

**Files:**
- Create: `tests/e2e_opencode.rs`

**Interfaces:**
- Consumes: `lain_bin()` = `PathBuf::from(env!("CARGO_BIN_EXE_lain"))`.
- Produces: a passing `cargo test --test e2e_opencode` run.

- [ ] **Step 1: Create the e2e test file**

```rust
//! End-to-end test for `lain init --agent opencode`.
//!
//! Runs the real binary in a temp git repo and verifies the produced
//! `opencode.json` matches the schema at
//! <https://opencode.ai/docs/mcp-servers/>.

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

#[test]
fn lain_init_opencode_writes_verified_opencode_json_and_agents_md() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);

    let status = Command::new(lain_bin())
        .args(["init", "--agent", "opencode", "--yes"])
        .args(["--workspace", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .expect("spawn lain init");
    assert!(status.success(), "lain init exited with {status:?}");

    let body = std::fs::read_to_string(repo.join("opencode.json")).expect("read opencode.json");
    let doc: serde_json::Value = serde_json::from_str(&body).expect("parse opencode.json");

    let lain = doc.pointer("/mcp/lain").expect("mcp.lain present");
    assert_eq!(lain["type"], "local");
    let cmd = lain["command"].as_array().expect("command is a JSON array");
    let cmd: Vec<String> = cmd.iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(cmd.first().map(String::as_str), Some("lain"),
        "command[0] must be the bare name `lain`, got {:?}", cmd.first());
    assert!(cmd.windows(2).any(|w| w == ["--workspace", "auto"]));
    assert!(cmd.windows(2).any(|w| w == ["--transport", "stdio"]));
    assert_eq!(lain["enabled"], true);
    assert_eq!(lain["timeout"], 30000);

    let agents = repo.join("AGENTS.md");
    assert!(agents.exists(), "AGENTS.md not written to project root");
    let agents_body = std::fs::read_to_string(&agents).expect("read AGENTS.md");
    assert!(agents_body.contains("When to use lain"));
    assert!(agents_body.contains("find_anchors"));
}

#[test]
fn lain_init_opencode_scope_user_writes_global_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo");
    git_init_quiet(&repo);

    let original_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", tmp.path());

    let status = Command::new(lain_bin())
        .args(["init", "--agent", "opencode", "--yes", "--scope", "user"])
        .args(["--workspace", repo.to_str().unwrap()])
        .current_dir(&repo)
        .status()
        .expect("spawn lain init");
    assert!(status.success(), "lain init exited with {status:?}");

    if let Some(h) = &original_home { std::env::set_var("HOME", h); }
    else { std::env::remove_var("HOME"); }

    let global = tmp.path().join(".config/opencode/opencode.json");
    assert!(global.exists(), "user-scope must write ~/.config/opencode/opencode.json");
    assert!(!repo.join("opencode.json").exists(),
        "user-scope must NOT write project opencode.json");
    assert!(!repo.join("AGENTS.md").exists(),
        "user-scope must NOT write AGENTS.md");
}
```

- [ ] **Step 2: Run the e2e test**

```bash
cargo test --test e2e_opencode
```

Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e_opencode.rs
git commit -m "test(e2e): verify lain init --agent opencode writes verified opencode.json"
```

---

## Task 5: Full test sweep + manual smoke check

- [ ] **Step 1: Run the full library test suite**

```bash
cargo test --lib
```

Expected: 469 (or current count) + 0 failed.

- [ ] **Step 2: Run the bin test suite (init + agents)**

```bash
cargo test --bin lain cmds::init::tests cmds::agents
```

Expected: 9 + tests pass (was 7 before; this work adds init_opencode×5, opencode_adapter×2, and opencode_agents_md×1 — note: the 5 init_opencode tests + 1 AGENTS.md content pin + 2 adapter = 8 new tests; the exact count depends on existing per-adapter tests).

- [ ] **Step 3: Run the e2e tests**

```bash
cargo test --test e2e_opencode
cargo test --test e2e_portable
cargo test --test e2e_agents
cargo test --test auto_workspace
```

Expected: all green.

- [ ] **Step 4: Commit any final fixes**

If adjustments were needed, commit them with a `chore:` or `fix:` message.

- [ ] **Step 5: Do NOT push.**

Stop here. The user (per their instruction "when is done and tested with a real install, test with copilot and vs code") will manually verify by running OpenCode in a repo, then we plan the Copilot/VS Code support.

---

## Out of Scope

- **Migrating the `omp` (oh-mi-pi) adapter** — different agent; left untouched.
- **Remote/HTTP MCP servers in OpenCode** — Lain is stdio only.
- **OAuth / registry flows** — only relevant for remote servers.
- **Extending `--scope` to other agents** — this work adds the flag; only `init_opencode` honors it. Other inits are explicitly out of scope.
- **Windows-specific path quirks** — `dirs::home_dir()` already handles Windows correctly.
- **Live `opencode` behavior test** — gated `#[ignore]` test that spawns `opencode` headless and calls a Lain tool. Add only after the user has confirmed a real OpenCode install works with the produced config.
