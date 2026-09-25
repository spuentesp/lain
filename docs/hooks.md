# Pre-Edit Hooks

`lain` ships bash hooks for the popular AI agents (Claude Code, Kimi, Agy, Codex). The hooks run before every Edit/Write/MultiEdit and call `lain hooks claim --path <file>` to register the agent + claim the file. lain returns conflicts in JSON, which the hook surfaces to the agent's context.

## Install

Pick your agent and follow the README in its directory:

| Agent | Hook dir | README |
|---|---|---|
| Claude Code | [`hooks/claude-code/`](../hooks/claude-code/) | [README](../hooks/claude-code/README.md) |
| Kimi | [`hooks/kimi/`](../hooks/kimi/) | [README](../hooks/kimi/README.md) |
| Agy | [`hooks/agy/`](../hooks/agy/) | [README](../hooks/agy/README.md) |
| Codex | [`hooks/codex/`](../hooks/codex/) | [README](../hooks/codex/README.md) |

## Common setup

1. `lain server` must be running on HTTP (e.g., `lain server --config ./repos.yaml --transport http --port 9999`).
2. Set `LAIN_URL` to the server (e.g. `http://localhost:9999` — bare URL; the CLI appends `/mcp`). The hook scripts default to that address; `lain hooks …` itself needs `--url` or `LAIN_URL`.
3. Each agent's hook calls `lain hooks claim --url $LAIN_URL --path <file>`. The first invocation auto-registers the agent and caches the session token to `~/.config/lain/hooks/<agent>.session`.

## Verification

After editing any file via Claude Code / Kimi / Agy / Codex with the hook installed, run:

```bash
curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_active_agents","arguments":{}},"id":1}' \
  | python3 -m json.tool
```

You should see your agent (`claude-code`, `kimi`, `agy`, or `codex`) listed with a non-zero `claims_count` while it's editing.

## Disabling

Remove the hook entry from the agent's MCP config (each agent's README has the exact removal instructions). The next edit won't claim.

## Multi-window / multi-instance

All sessions of one agent kind share the same `~/.config/lain/hooks/<kind>.session` file — they appear as ONE agent in lain. If you want per-window tracking, set `LAIN_AGENT_NAME` differently per shell before starting the agent.

## E2E harness

`tests/e2e/multiplayer-hooks.sh` exercises the full hook flow against a real
`lain server`. See [the harness README](../tests/e2e/README.md), or run
`tests/e2e/multiplayer-hooks.sh --help`.

## Activity observation

The pre-edit hook above only fires on Edit / Write / MultiEdit. The
intent layer also wants to know what the agent was *reading* so
the activity feed (`focus`, `observed_reads`, `last_tool`) reflects
the agent's investigation, not just its edits.

For each agent kind, add a wrapper that POSTs every tool-call
observation to `/hook`:

```bash
# Inside the agent's per-tool wrapper
curl -sf -X POST "$LAIN_URL/hook" \
    -H 'Content-Type: application/json' \
    --data "$(printf '{"session_token":"%s","agent_id":"%s","event":"%s","tool":"%s","target":"%s"}' \
        "$LAIN_SESSION_TOKEN" "$LAIN_AGENT_ID" \
        "tool_start" "$TOOL" "$TARGET")"
```

The wire shape, validation, and auth contract are documented in
`src/server/mcp/hook.rs`; the regression fixture is
`tests/hook_ingest.rs`.

### Per-agent-kind wrappers

| Agent | Wrapper location | Hook events fired |
|---|---|---|
| Kimi | `hooks/kimi/pre-tool.sh` | Read, Edit, Grep, Bash |
| AGY | `hooks/agy/pre-tool.sh` | Read, Edit, Grep, Bash |
| Codex | `hooks/codex/pre-tool.sh` | Read, Edit, Grep, Bash |

Claude Code has no observation wrapper yet: `hooks/claude-code/pre-edit.sh`
and `post-edit.sh` claim and release files around Edit/Write only (see
`hooks/claude-code/README.md`).

All three wrappers are bash, fail-open (always exit 0), parse the
agent's stdin JSON envelope, extract `tool_name` and
`tool_input.{file_path,command,pattern}`, and forward to `lain
hooks observe` which POSTs `/hook`. The CLI subcommand and
wrappers are the agent-agnostic replacement for hand-rolled curl
in the per-agent wrapper. The synchronous pre-edit endpoint is
`POST /hook/evaluate` (used by agents that want a YES/NO answer
before Edit fires); the wrapper script above is for the
observation stream.
