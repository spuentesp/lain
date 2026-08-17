> **Status:** Superseded by `docs/superpowers/specs/2026-08-14-lain-consolidation-design.md`.

# Commands::Agents Follow-Up Design

## Goal
Wire the existing agent install/list/verify/remove subsystem into the `lain` CLI as a first-class `agents` command, fix the runtime blockers that prevent per-agent end-to-end tests from passing, and harden the DST harness so every supported agent can be installed and verified deterministically.

## Background
`src/cmds/agents/` already implements the core logic:

| Module | Function | Signature |
|--------|----------|-----------|
| `list` | `run_list` | `pub fn run_list() -> Result<()>` |
| `install` | `run_install` | `pub fn run_install(id: Option<&str>, all: bool, scope: InstallScope) -> Result<()>` |
| `remove` | `run_remove` | `pub fn run_remove(id: &str, scope: InstallScope) -> Result<()>` |
| `verify` | `run_verify` | `pub async fn run_verify(all: bool, id: Option<&str>, json: bool) -> Result<()>` |

`InstallScope` lives in `src/cmds/agents/adapters/mod.rs` (`User | Project | Workspace`). `src/cmds/mod.rs` already re-exports `pub mod agents`. The only missing piece is the CLI dispatch in `src/main.rs`.

A committed DST harness (`tests/e2e/agent_install.rs`) runs each agent through install/verify under a temporary `HOME`. It surfaced three classes of failures:

1. **CLI gap**: there is no `lain agents ...` command, so the harness cannot exercise the production path.
2. **Per-agent runtime blockers**: some adapters need extra config in the temp `HOME` before the target agent will start cleanly (Kimi needs `default_model`, omp needs an `ollama` provider entry, auth-gated agents need documented sign-in expectations).
3. **Harness drift**: the `agy` case id does not match the manifest row id `antigravity`, causing `run_install` to fail with "unknown agent id".

## Design

### A. Production CLI wiring

Add to `src/main.rs`:

```rust
#[derive(Debug, Subcommand)]
enum Commands {
    // ... existing variants ...
    /// Manage agent MCP configurations (Claude, Kimi, Cursor, etc.)
    Agents {
        #[command(subcommand)]
        action: AgentsAction,
    },
}

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

Dispatch:

- `AgentsAction::List` → `cmds::agents::list::run_list()`
- `AgentsAction::Install { id, all, scope }` → parse `scope` into `InstallScope`, then `cmds::agents::install::run_install(id.as_deref(), all, scope)`
- `AgentsAction::Verify { id, all, json }` → `cmds::agents::verify::run_verify(all, id.as_deref(), json).await`
- `AgentsAction::Remove { id, scope }` → parse `scope` into `InstallScope`, then `cmds::agents::remove::run_remove(&id, scope)`

Validation rules:
- `install` requires exactly one of `--all` or `<id>`.
- `verify` requires exactly one of `--all` or `<id>`.
- `remove` requires `<id>`.
- `workspace` scope is rejected for `remove` by the existing `run_remove` implementation.

### B. Per-agent runtime setup

The harness will prepare the temp `HOME` before calling `run_install` for agents that need it:

| Agent | Precondition helper | Why |
|-------|---------------------|-----|
| `kimi` | Write `~/.kimi-code/config.toml` with `default_model = "kimi-k2"` | Prevents the Kimi CLI from prompting for a model on first launch. |
| `omp` | Write `~/.config/omp/config.json` (or equivalent) registering an `ollama` provider | The omp adapter currently emits an `ollama` transport; the target config must accept that provider without error. |
| `claude`, `cursor`, `windsurf`, `cline`, `codex`, `continue`, `vscode_copilot` | Document that live sign-in is required for the agent to actually call tools, but the harness only asserts config-written + MCP-reachable. | Auth is out of `lain`'s scope; we only verify the MCP wire-up. |

No `lain` source changes are required for the auth-gated agents. The Kimi and omp helpers live in the harness test support module.

### C. Harness polish

1. **Fix id mapping**: change the harness case id `agy` to `antigravity` so it matches `AgentEntry::id` in the manifest.
2. **End-to-end verification per agent**: after install, each test case will:
   - Start a temporary `lain` owner process on a throwaway workspace.
   - Wait for the HTTP MCP endpoint to be reachable.
   - Send `tools/list` and assert `lain` tools are present.
   - Create a new file in the workspace, wait for the watcher to pick it up, and query the volatile overlay to confirm it appears.
3. **Isolation**: each agent test still uses a unique temp `HOME` and workspace; the owner process is killed at the end of the test.

## Success Criteria

1. `lain agents list` prints all supported agents.
2. `lain agents install --all` writes config for every agent without error under a temp `HOME`.
3. `lain agents verify --all` reports `config=yes` and `mcp=yes` for every agent when a Lain owner is running.
4. `lain agents remove <id>` removes that agent's config.
5. `RUN_E2E_AGENT=1 cargo test --test agent_install` passes for `kimi`, `claude`, `agy`/`antigravity`, and all other manifest rows.
6. The watcher bug (unreadable `infra/neo4j/import`) does not block the harness; the harness uses minimal workspaces without such directories.

## Out of Scope

- Fixing the `notify` EACCES watcher bug in `src/watcher.rs`. That is tracked separately and is not required for the harness to pass.
- Authenticating with third-party agents. The harness verifies MCP config wire-up only.
- Adding new adapters. Only existing adapters are wired and tested.

## File Changes Expected

- `src/main.rs`: add `Agents`/`AgentsAction` variants and dispatch.
- `tests/e2e/agent_install.rs`: fix `agy`→`antigravity`, add Kimi/omp `HOME` precondition helpers, add end-to-end owner + watcher verification.
- `tests/e2e/README.md`: update the per-agent status table and the `RUN_E2E_AGENT` instructions if needed.
