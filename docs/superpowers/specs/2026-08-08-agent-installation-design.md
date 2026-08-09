# Agent Installation and Verification

**Date:** 2026-08-08
**Status:** Design approved for implementation

## Goal

Make every supported AI coding agent actually use Lain through MCP, and make that property provable in CI and locally with a single command.

The current state on this machine:

- Kimi auto-discovers a `lain` plugin at `~/.kimi-code/plugins/managed/lain`, but the handshake fails because `rust-mcp-sdk 0.9.0` advertises protocol `2024-11-05` and Kimi speaks `2025-11-25`.
- Claude registers Lain in `~/.claude/settings.json` `mcpServers.lain`, but a fresh `claude` session launched without `--mcp-config` sees no Lain tools.
- Other agents supported by the existing hooks (`hooks/cline/lain-hook.sh`, `hooks/cursor/lain-hook.sh`, `hooks/windsurf/lain-hook.sh`, `hooks/claude/lain-hook.sh`) each have their own glue, and there is no single place to install, list, or verify them.
- The only verification path is `curl` against the HTTP singleton. That proves the server is up; it does not prove any agent has Lain wired in.

This spec replaces those with a manifest, a single installer, and a single test harness.

## Design

### Manifest

A new file at `agents/manifest.toml` lists every supported agent. Each row is the canonical description of how Lain is wired into that agent. Example shape:

```toml
[[agent]]
id = "claude"
display_name = "Claude Code"
binary = "claude"
detect_paths = ["~/.claude"]
config_user = "~/.claude/settings.json"
config_project = ".claude/settings.json"
config_format = "json"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"   # or "http" for the singleton
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = ["claude", "--print", "--mcp-config", "{{config_path}}", "list your tools"]

[[agent]]
id = "kimi"
...
```

The manifest is the single source of truth. New agents join by adding a row, not by editing six different scripts.

### Core protocol fix

Bump the protocol version Lain speaks so Kimi and Claude can both connect.

- `Cargo.toml`: replace `rust-mcp-sdk = "=0.9.0"` with `rust-mcp-sdk = "1.0.1"`. Replace `rust-mcp-schema = "0.10.0"` with `rust-mcp-schema = "0.10.3"` and opt in to its `2025_11_25` feature.
- `src/mcp/handler.rs`: change `ProtocolVersion::V2024_11_05` to `ProtocolVersion::V2025_11_25` and update imports to match the new schema.
- The MCP server keeps advertising the same `tools/list` and `tools/call` methods. Only the protocol version string changes.
- This is a no-behavior change for existing static analysis; it is a wire-level change so clients and server agree on the protocol.

### Installer

A new `lain agents` subcommand family. Each adapter is a small file under `src/cmds/agents/` that owns the per-agent config shape.

```
lain agents list                       # print manifest rows, plus installed/registered status
lain agents install <id> [--user|--project|--workspace]
                                       # write config in the requested scope
lain agents install --all --user      # bulk install everything detectable
lain agents remove  <id> [--user|--project|--workspace]
                                       # remove config
lain agents verify  [--all|--agent <id>] [--json]
                                       # the test harness, see below
```

Per-agent adapters write the right config file for that scope. The HTTP singleton is the default transport. Stdio is a fallback only when the singleton is not available; the adapters prefer `http://localhost:${LAIN_PORT:-9999}/mcp` whenever the agent supports an `http` config format. Claude, Cursor, Continue, Windsurf, Cline, and OMP all support it.

Adapters in `src/cmds/agents/`:

- `claude.rs`: writes `~/.claude/settings.json` `mcpServers.lain` plus the existing `PreToolUse` hook chain.
- `kimi.rs`: writes `~/.kimi-code/plugins/managed/lain/kimi.plugin.json` with an absolute `command` path and a launcher that resolves `--workspace` from the active project.
- `cursor.rs`, `continue.rs`, `windsurf.rs`, `cline.rs`: write `.vscode/mcp.json` (project) and the user-level config the existing hooks already produce.
- `codex.rs`, `omp.rs`, `gemini.rs`: write the per-agent project `.mcp.json` and the user-level `~/.config/<agent>/mcp.json`.
- `vscode_copilot.rs`: reuses the `.vscode/mcp.json` produced by the VS Code adapters; kept for completeness.

The existing scripts under `scripts/` and `hooks/` are not removed. They are kept as the reference implementation of their per-agent hook contract and are imported by the corresponding adapter.

### Test harness

`lain agents verify` is the single command that proves whether each agent actually has Lain. It does two things per agent.

**Static check.** Parse the agent’s MCP config file. Assert that a `lain` entry exists, its transport is `http` (singleton) or `stdio` (direct binary), and its `command`/`url` matches what the manifest expects.

**Live probe.** Reuse a new crate, `crates/lain-mcp-probe`, that speaks MCP through `rust-mcp-sdk 1.0.1`. For each agent, the probe:

1. Reads the same config the adapter wrote.
2. If the transport is `http`, opens a connection to the singleton on `LAIN_PORT` and runs:
   - `initialize`
   - `tools/list`
   - `tools/call get_health`
3. If the transport is `stdio`, spawns the configured command and runs the same three calls over its stdin/stdout.
4. Records a per-agent row:
   - `installed`: `bool`
   - `config_valid`: `bool`
   - `mcp_reachable`: `bool`
   - `tools_count`: `usize`
   - `health`: `Operational` or `Unreachable` or `Error`
5. Prints either a human table or `--json` machine output for CI.

The probe does not require the agent’s CLI to support `--headless` or any particular flag. It only speaks MCP, so it tests the wiring, not the model.

### CI integration

A new integration test in `tests/agents_install.rs` does the following under a temp `HOME` and `XDG_CONFIG_HOME`:

1. For each row in `agents/manifest.toml`, call `lain agents install <id> --user`.
2. Parse the resulting config file and assert the Lain entry matches the manifest.
3. Call `lain agents verify --all` against a temporary Lain instance bound to a free port.
4. Assert that every installed agent reports `installed: true`, `config: valid`, `mcp: reachable`, and `health: Operational`.

The test is the new gate in `tests/agents_install.rs`. It is the contract: if an adapter stops working, the harness catches it before the change ships.

### Live verification

`lain agents verify --all` is the command a person runs to answer “does each installed agent actually have Lain?”. It is the same code as the CI test, just pointing at the real user config and the real HTTP singleton. Output is a small table:

```
agent     installed  config  mcp  tools  health
claude    yes        ok      ok   41     Operational
kimi      yes        ok      ok   41     Operational
cursor    no         -       -    -      -
...
```

`--json` returns the same data for tooling.

### Documentation

- `docs/agent-installation.md`: canonical install/verify commands, manifest schema, and per-agent install path.
- `README.md`: short section pointing at the new commands.
- `docs/quickstart-tools.md` and `docs/QUICKSTART_AGENTS.md`: update the per-agent installation sections to point at `lain agents install <agent>`.

## Unchanged behavior

- HTTP singleton on port 9999 remains the shared server. Watcher, graph, MCP handler, and project registry behavior is unchanged.
- The existing `hooks/` and `scripts/` per-agent glue keeps working. The new installer is layered on top.
- No new dependencies beyond `rust-mcp-sdk 1.0.1`, `rust-mcp-schema 0.10.3`, and the existing `tempfile` for tests.

## Testing strategy

### Automated tests

- `crates/lain-mcp-probe/tests/probe.rs`: spin up a temporary Lain instance on a free port, call all three probe methods, assert the expected response shape.
- `src/cmds/agents/<adapter>.rs` unit tests: each adapter gets a temp `HOME`/`XDG_CONFIG_HOME` and asserts the resulting file matches the manifest row, plus a negative test for an already-installed config that should be replaced.
- `tests/agents_install.rs`: the end-to-end harness described above. Must pass in CI.

### Live verification

After implementation:

1. Run `cargo test --all-targets`.
2. Run `cargo build --release`, install at `/home/sebastian/.local/lain/lain`, restart the singleton.
3. Run `lain agents verify --all` against the real user config. Expect every installed agent to report `Operational` for `health`.
4. Open one Claude terminal and one Kimi terminal in Orca; ask each to list its tools and run `get_health` through MCP. Expect a clean health report from both, with no protocol-version error.

## Acceptance criteria

- `rust-mcp-sdk 1.0.1` and `rust-mcp-schema 0.10.3` are the pinned versions; the build succeeds on a clean `cargo build --release`.
- `src/mcp/handler.rs` advertises `ProtocolVersion::V2025_11_25`.
- Kimi’s `plugin-lain:lain` handshake no longer times out; the plugin shows `connected` in the session header.
- Claude terminals launched without `--mcp-config` see the Lain tools in their tool list (because the global `~/.claude/settings.json` already wires them).
- `lain agents verify --all` against the real user config returns `Operational` for every installed agent and `--json` is parseable.
- The new `tests/agents_install.rs` passes under a temp `HOME` and temp `XDG_CONFIG_HOME`.
- Existing watcher tests, existing handler tests, and the full `cargo test --all-targets` remain green.
- No agent-specific config in `~/.claude/`, `~/.kimi-code/plugins/`, or the other agent directories is removed; the installer adds entries alongside or upgrades them in place.
