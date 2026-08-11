# Agent Installation and Verification

Lain can be installed into every supported AI coding agent with a single
command. The HTTP singleton on port 9999 stays the shared server.

## Install

```bash
# Single agent, user-scope (recommended)
lain agents install --scope user claude

# All installed agents (rows whose adapter is registered)
lain agents install --all --scope user

# Project-scope (writes .vscode/mcp.json and friends)
lain agents install --scope project claude
```

`--scope` accepts `user` (writes to the agent's home config), `project`
(writes to the project's own config), or `workspace` (uses the active
Orca worktree context). The `install` loop currently honors both
`user` and `project` for every row; only `opencode` and `copilot`
expose both scopes to `Init` (Claude and Kimi are user-only, the
remainder are project-only).

### Supported agents

Every row in `agents/manifest.toml` is a candidate target. The current
list is:

| ID             | Display name                | Status on this host |
|----------------|-----------------------------|---------------------|
| `claude`       | Claude Code                 | wired, `Operational` |
| `kimi`         | Kimi Code                   | wired, `Operational` |
| `cursor`       | Cursor (CLI)                | wired, `Operational` |
| `continue`     | Continue.dev CLI            | wired, `Operational` |
| `antigravity`  | Antigravity CLI (`agy`)     | wired, `Operational` |
| `gemini`       | Legacy Gemini CLI           | init-only (no manifest row; use `lain init --agent gemini`; `antigravity` supersedes it for `agy` users) |
| `cline`        | Cline CLI                   | wired, `Operational` |
| `codex`        | OpenAI Codex CLI            | wired, `Operational` |
| `omp`          | OMP (oh-my-pi)              | wired, `Operational` |
| `windsurf`     | Windsurf (no headless CLI)  | config-only; `not installed` until a Windsurf IDE writes `~/.windsurf/mcp.json` |
| `opencode`     | OpenCode                    | wired, `Operational`; project (default, writes `opencode.json`) or user (`--scope user`, writes `~/.config/opencode/opencode.json`) |
| `copilot`        | VS Code + GitHub Copilot | project (default) or user (`--scope user`) |

Windsurf is listed because the install loop will still write its
config and `verify` will report it correctly as `not installed`; the
shipped Windsurf product has no headless CLI today. `opencode` and
`copilot` are the only agents that honor `--scope {project|user}` on
`Init` — `opencode.json` is project-scope by default (travels with the
repo) and switches to `~/.config/opencode/opencode.json` with
`--scope user`; `copilot` writes `.vscode/mcp.json` by default and
`~/.copilot/mcp-config.json` with `--scope user`. All other agents are
inherently user-scope (Claude, Kimi) or always project-scope (the
remaining rows).

## Verify

```bash
# All installed agents, human-readable
lain agents verify --all

# One agent, machine-readable
lain agents verify --agent claude --json
```

`lain agents verify` always probes the shared HTTP singleton, regardless
of the per-agent transport. The per-agent configs themselves are
stdio-launched by the agent at runtime; the verify path speaks MCP
directly to the singleton so a `Broken pipe` on the stdio side does
not mask the actual wiring.
```

Each row reports whether the agent is installed, whether the config
parses, whether MCP is reachable, the tool count, and the get_health
result.

## List

```bash
lain agents list
```

Prints every supported agent id, display name, install status, and
config path.

## Remove

```bash
lain agents remove --scope user claude
```

Removes the Lain entry from the chosen scope.

## Adding a new agent

Append a new `[[agent]]` row to `agents/manifest.toml` and, if needed,
a new adapter in `src/cmds/agents/adapters/<id>.rs`. Re-run
`cargo test --all-targets`. The integration test
`tests/agents_install.rs` exercises the install + verify path for every
manifest row.

## Sidecar mode

A `lain --mode owner` process owns the graph, watcher, and write paths
for a workspace. Multiple `lain --mode sidecar` processes can attach to
the same workspace to read the graph and subscribe to overlay updates
without touching the writer lock.

```bash
# owner: long-running process, owns the graph and the HTTP singleton
lain --workspace /abs/path --mode owner --transport http --port 9999 \
    --embedding-model ~/.local/lain/models/all-MiniLM-L6-v2.onnx

# sidecar: a second process on the same workspace, e.g. started by an
# agent that only supports the stdio transport
lain --workspace /abs/path --mode sidecar --transport http --port 9998 \
    --embedding-model ~/.local/lain/models/all-MiniLM-L6-v2.onnx
LAIN_OWNER_URL=http://localhost:9999
```

`lain agents install --scope user` writes `url: http://localhost:9999/mcp`
for agents that support the HTTP transport, and `command: lain --mode
sidecar` for the rest. The HTTP singleton is the single source of truth
for the workspace.
