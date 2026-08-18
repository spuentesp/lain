# Pre-Edit Hooks

`lain` ships bash hooks for the popular AI agents (Claude Code, Kimi, Agy, Codex). The hooks run before every Edit/Write/MultiEdit and call `lain hooks claim <path>` to register the agent + claim the file. lain returns conflicts in JSON, which the hook surfaces to the agent's context.

## Install

Pick your agent and follow the README in its directory:

| Agent | Hook dir | README |
|---|---|---|
| Claude Code | [`hooks/claude-code/`](hooks/claude-code/) | [README](hooks/claude-code/README.md) |
| Kimi | [`hooks/kimi/`](hooks/kimi/) | [README](hooks/kimi/README.md) |
| Agy | [`hooks/agy/`](hooks/agy/) | [README](hooks/agy/README.md) |
| Codex | [`hooks/codex/`](hooks/codex/) | [README](hooks/codex/README.md) |

## Common setup

1. `lain server` must be running on HTTP (e.g., `lain server --config ./repos.yaml --transport http --port 9999`).
2. Set `LAIN_URL` if not using the default (`http://localhost:9999` — bare server URL; the MCP `/mcp` path is appended automatically by the CLI).
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

`tests/e2e/multiplayer-hooks.sh` exercises the full hook flow against a real `lain server`. See [the harness README](#) (or run `tests/e2e/multiplayer-hooks.sh --help` once it exists).
