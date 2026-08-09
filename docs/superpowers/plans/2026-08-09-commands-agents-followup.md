# Commands::Agents Follow-Up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `lain agents {list,install,verify,remove}` into the CLI, fix the per-agent runtime blockers and harness drift surfaced by `tests/e2e/agent_install.rs`, and prove that Kimi and Claude Code can actually list and call Lain MCP tools.

**Architecture:** Add a new `Commands::Agents` variant and `AgentsAction` subcommand in `src/main.rs` that dispatches to the existing `src/cmds/agents/*` functions. Extend the DST harness with per-agent temp-HOME precondition helpers and end-to-end owner/watcher verification. Keep a separate manual smoke-test path for Claude Code because it is auth-gated.

**Tech Stack:** Rust (clap 4, tokio), Cargo integration tests, shell smoke tests for live agents.

## Global Constraints

- `lain` must compile on stable Rust as configured in `rust-toolchain.toml`.
- CLI additions must mirror the existing `Projects`/`Server` subcommand style in `src/main.rs`.
- `InstallScope` must be reused from `src/cmds/agents/adapters/mod.rs` (`User | Project | Workspace`).
- Harness tests are gated on `RUN_E2E_AGENT=1` and use `#[ignore]`.
- Auth-gated agents must not fail the suite when unauthenticated; their live tool-use assertions are skipped or moved to a manual step.

---

### Task 1: Wire `Commands::Agents` into `src/main.rs`

**Files:**
- Modify: `src/main.rs:37-78` (Commands enum), `src/main.rs:95-142` (dispatch match)
- Test: `cargo run -- agents list`

**Interfaces:**
- Consumes: `cmds::agents::list::run_list()`, `cmds::agents::install::run_install(id: Option<&str>, all: bool, scope: InstallScope)`, `cmds::agents::verify::run_verify(all: bool, id: Option<&str>, json: bool)`, `cmds::agents::remove::run_remove(id: &str, scope: InstallScope)`, `cmds::agents::adapters::InstallScope`.
- Produces: `Commands::Agents { action: AgentsAction }` and `AgentsAction` enum parsed by clap.

- [ ] **Step 1: Add `AgentsAction` enum**

Insert after `ProjectsAction`:

```rust
#[derive(Debug, Subcommand)]
enum AgentsAction {
    /// List supported agents.
    List,
    /// Install MCP config for one or all agents.
    Install {
        /// Agent id (e.g. claude, kimi). Omit with --all.
        id: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "user", value_parser = ["user", "project", "workspace"])]
        scope: String,
    },
    /// Verify that installed agents can reach the Lain MCP server.
    Verify {
        /// Agent id. Omit with --all.
        id: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove an agent's MCP config.
    Remove {
        id: String,
        #[arg(long, default_value = "user", value_parser = ["user", "project", "workspace"])]
        scope: String,
    },
}
```

- [ ] **Step 2: Add `Commands::Agents` variant**

Insert into the `Commands` enum:

```rust
/// Manage agent MCP configurations (Claude, Kimi, Cursor, etc.)
Agents {
    #[command(subcommand)]
    action: AgentsAction,
},
```

- [ ] **Step 3: Add dispatch arm**

Insert into the `match cmd { ... }` block before `Commands::Use`:

```rust
Commands::Agents { action } => match action {
    AgentsAction::List => return cmds::agents::list::run_list(),
    AgentsAction::Install { id, all, scope } => {
        let scope = parse_install_scope(&scope)?;
        if !all && id.is_none() {
            anyhow::bail!("--all or <id> is required");
        }
        return cmds::agents::install::run_install(id.as_deref(), all, scope);
    }
    AgentsAction::Verify { id, all, json } => {
        if !all && id.is_none() {
            anyhow::bail!("--all or <id> is required");
        }
        return cmds::agents::verify::run_verify(all, id.as_deref(), json).await;
    }
    AgentsAction::Remove { id, scope } => {
        let scope = parse_install_scope(&scope)?;
        return cmds::agents::remove::run_remove(&id, scope);
    }
},
```

- [ ] **Step 4: Add `parse_install_scope` helper**

Add near `resolve_workspace_path`:

```rust
fn parse_install_scope(s: &str) -> Result<cmds::agents::adapters::InstallScope> {
    use cmds::agents::adapters::InstallScope;
    match s {
        "user" => Ok(InstallScope::User),
        "project" => Ok(InstallScope::Project),
        "workspace" => Ok(InstallScope::Workspace),
        _ => Err(anyhow::anyhow!("unknown scope: {s}")),
    }
}
```

- [ ] **Step 5: Build and run `lain agents list`**

Run: `cargo build`
Expected: compiles.

Run: `cargo run -- agents list`
Expected: prints supported agents.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat(cli): wire lain agents {list,install,verify,remove}"
```

---

### Task 2: Add per-agent temp-HOME precondition helpers to the harness

**Files:**
- Modify: `tests/e2e/agent_install.rs`
- Test: `cargo test --test agent_install e2e_kimi -- --ignored --nocapture` with `RUN_E2E_AGENT=1` (after Task 1 and a running owner)

**Interfaces:**
- Consumes: `AgentCase` struct fields.
- Produces: `fn prepare_home(case: &AgentCase, home: &Path)` that writes agent-specific seed config before install.

- [ ] **Step 1: Add `prepare_home` helper**

Insert before `install_into`:

```rust
fn prepare_home(case: &AgentCase, home: &Path) {
    match case.id {
        "kimi" => {
            let dir = home.join(".kimi-code");
            std::fs::create_dir_all(&dir).expect("kimi config dir");
            std::fs::write(
                dir.join("config.toml"),
                "default_model = \"kimi-k2\"\n",
            )
            .expect("kimi config");
        }
        "omp" => {
            let dir = home.join(".config/omp");
            std::fs::create_dir_all(&dir).expect("omp config dir");
            std::fs::write(
                dir.join("config.json"),
                r#"{"providers":{"ollama":{"base_url":"http://localhost:11434"}}}"#,
            )
            .expect("omp config");
        }
        _ => {}
    }
}
```

- [ ] **Step 2: Call `prepare_home` before install**

Modify `run_case`:

```rust
let tmp = tempfile::tempdir().expect("tempdir");
prepare_home(case, tmp.path());
```

Also modify `assert_adapter_round_trip`:

```rust
let home = tempfile::tempdir().expect("tempdir");
prepare_home(case, home.path());
```

- [ ] **Step 3: Verify Kimi precondition is written**

Run: `cargo test --test agent_install e2e_kimi -- --ignored --nocapture` with `RUN_E2E_AGENT=1` and a Lain owner on port 9999.
Expected: Kimi no longer hangs on model selection.

- [ ] **Step 4: Commit**

```bash
git add tests/e2e/agent_install.rs
git commit -m "test(e2e): seed temp HOME for kimi and omp agents"
```

---

### Task 3: Fix harness drift and harden end-to-end verification

**Files:**
- Modify: `tests/e2e/agent_install.rs`
- Modify: `tests/e2e/README.md`
- Test: `RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture`

**Interfaces:**
- Consumes: `AgentCase::id`, `run_case`, `assert_adapter_round_trip`.
- Produces: corrected case id, updated docs.

- [ ] **Step 1: Rename `agy` case to `antigravity`**

Change the case id in `agent_cases`:

```rust
AgentCase {
    id: "antigravity",
    binary: "agy",
    run_args: &["--dangerously-skip-permissions", "--print-timeout", "60s", "-p"],
    requires_auth: false,
    workspace,
},
```

Rename the test function:

```rust
#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_antigravity() { run_case_assert(&agent_cases()[1]) }
```

- [ ] **Step 2: Remove the install-failure fallback path in `run_case`**

Once Task 1 lands, `install_succeeded` should always be true for valid ids. Replace:

```rust
if !install_succeeded {
    let _ = assert_watcher_round_trip(case, tmp.path(), port, &before);
    return Err(format!("{}: install failed; cannot verify adapter", case.id));
}
```

with:

```rust
if !install_succeeded {
    return Err(format!("{}: install failed with status {:?}", case.id, install_status));
}
```

- [ ] **Step 3: Update README status table**

In `tests/e2e/README.md`, update the agent list:

```markdown
- Kimi, antigravity, omp: the harness exercises the live HTTP singleton end
  to end (install + spawn + tool list + get_health + watcher round-trip).
```

- [ ] **Step 4: Run the full harness**

Run: `RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture`
Expected: non-auth agents (`kimi`, `antigravity`, `omp`) pass; auth-gated agents skip inner assertions cleanly.

- [ ] **Step 5: Commit**

```bash
git add tests/e2e/agent_install.rs tests/e2e/README.md
git commit -m "test(e2e): fix antigravity id and tighten install assertions"
```

---

### Task 4: Prove Kimi and Claude Code can actually use Lain

**Files:**
- Create: `scripts/smoke-test-kimi.sh`
- Create: `scripts/smoke-test-claude.sh`
- Modify: `tests/e2e/README.md`
- Test: manual run of both scripts against a live owner

**Interfaces:**
- Consumes: `lain` binary, `kimi` CLI, `claude` CLI, running Lain owner on port 9999.
- Produces: pass/fail output with tool-list and get_health excerpts.

- [ ] **Step 1: Create `scripts/smoke-test-kimi.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail

PORT="${LAIN_PORT:-9999}"
WORKSPACE="${LAIN_WORKSPACE:-$(pwd)}"
TMP_HOME=$(mktemp -d)
trap 'rm -rf "$TMP_HOME"' EXIT

mkdir -p "$TMP_HOME/.kimi-code"
printf 'default_model = "kimi-k2"\n' > "$TMP_HOME/.kimi-code/config.toml"

export HOME="$TMP_HOME"
export XDG_CONFIG_HOME="$TMP_HOME/.config"
export LAIN_PORT="$PORT"

cargo run --quiet -- agents install --scope user kimi

echo "=== Kimi tool list + get_health ==="
kimi -p "list the MCP tools you have, then call get_health on the lain tool, and print both verbatim" \
  --workspace "$WORKSPACE" \
  --print-timeout 60s
```

Make executable: `chmod +x scripts/smoke-test-kimi.sh`

- [ ] **Step 2: Create `scripts/smoke-test-claude.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail

PORT="${LAIN_PORT:-9999}"
WORKSPACE="${LAIN_WORKSPACE:-$(pwd)}"
TMP_HOME=$(mktemp -d)
trap 'rm -rf "$TMP_HOME"' EXIT

export HOME="$TMP_HOME"
export XDG_CONFIG_HOME="$TMP_HOME/.config"
export LAIN_PORT="$PORT"

cargo run --quiet -- agents install --scope user claude

echo "=== Claude Code tool list + get_health ==="
echo "NOTE: This requires you to be signed in to Claude Code."
claude --allow-dangerously-skip-permissions \
  "list the MCP tools you have, then call get_health on the lain tool, and print both verbatim"
```

Make executable: `chmod +x scripts/smoke-test-claude.sh`

- [ ] **Step 3: Run the Kimi smoke test**

Prerequisites:
- A Lain owner is running: `cargo run -- --workspace /path/to/project --transport http --port 9999`
- `kimi` CLI is installed and on `$PATH`.

Run: `LAIN_WORKSPACE=/path/to/project LAIN_PORT=9999 ./scripts/smoke-test-kimi.sh`
Expected: output contains `mcp__plugin-lain_lain__get_health` and `Operational`.

- [ ] **Step 4: Run the Claude Code smoke test (manual/auth required)**

Prerequisites:
- A Lain owner is running on port 9999.
- `claude` CLI is installed and signed in.

Run: `LAIN_WORKSPACE=/path/to/project LAIN_PORT=9999 ./scripts/smoke-test-claude.sh`
Expected: output contains `mcp__plugin-lain_lain__get_health` and `Operational`.

- [ ] **Step 5: Document smoke tests in README**

Append to `tests/e2e/README.md`:

```markdown
## Manual live-agent smoke tests

For agents that require authentication or are hard to drive fully automatically,
use the helper scripts in `scripts/`:

```bash
# Requires a running Lain owner on port 9999.
LAIN_WORKSPACE=/path/to/project LAIN_PORT=9999 ./scripts/smoke-test-kimi.sh
LAIN_WORKSPACE=/path/to/project LAIN_PORT=9999 ./scripts/smoke-test-claude.sh  # requires sign-in
```
```

- [ ] **Step 6: Commit**

```bash
git add scripts/smoke-test-kimi.sh scripts/smoke-test-claude.sh tests/e2e/README.md
git commit -m "test(smoke): add kimi and claude code live MCP smoke tests"
```

---

### Task 5: Final integration and verification

**Files:**
- All changed files.
- Test: `cargo build`, `cargo test`, `RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture`

- [ ] **Step 1: Full build**

Run: `cargo build`
Expected: no warnings or errors.

- [ ] **Step 2: Unit tests**

Run: `cargo test --lib`
Expected: all pass.

- [ ] **Step 3: Integration tests (without e2e agent flag)**

Run: `cargo test --test agent_install`
Expected: ignored tests do not run; any non-ignored tests pass.

- [ ] **Step 4: E2E agent harness**

Run: `RUN_E2E_AGENT=1 cargo test --test agent_install -- --include-ignored --nocapture`
Expected:
- `e2e_kimi` passes.
- `e2e_antigravity` passes.
- `e2e_omp` passes.
- Auth-gated tests (`e2e_claude`, `e2e_cursor`, `e2e_cline`, `e2e_cn`, `e2e_codex`) report `auth-gated: skipped inner assertions` and do not fail the suite.

- [ ] **Step 5: Commit any fixes**

If any test required a fix, commit it with a descriptive message.

- [ ] **Step 6: Final summary**

Report to the user:
- `lain agents {list,install,verify,remove}` is wired.
- Non-auth agents (`kimi`, `antigravity`, `omp`) pass the full DST harness.
- Auth-gated agents skip cleanly; Claude Code can be verified with `scripts/smoke-test-claude.sh` after sign-in.

---

## Self-Review

**1. Spec coverage:**
- A (CLI wiring) → Task 1.
- B (per-agent runtime setup) → Task 2.
- C (harness polish, id mapping, end-to-end verification) → Task 3.
- Live agent proof for Kimi/Claude Code → Task 4.

**2. Placeholder scan:**
- No TBD/TODO/fill-in details.
- All code blocks are concrete.
- Exact file paths and line ranges are provided.

**3. Type consistency:**
- `InstallScope` parsed from string matches enum variants in `src/cmds/agents/adapters/mod.rs`.
- `run_verify` is awaited because it is `async`.
- `run_install`/`run_remove` are sync and not awaited.
