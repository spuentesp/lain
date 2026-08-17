# Agy Pre-Edit Hook for lain

This hook auto-claims files in `lain` before Agy (Antigravity CLI) edits them.

> **Note:** Agy is not installed on every host. The exact config path below
> is assumed; if Agy's home is different, point this hook at the right file.
> Current best guess: `~/.agy/mcp.json`, with a documented fallback to
> `~/.config/agy/mcp.json`.

## Config path (best-effort, verify on your host)

Agy MCP server config typically lives at:

- `~/.agy/mcp.json` (brief assumption) — verify with `ls ~/.agy`
- `~/.config/agy/mcp.json` (fallback)
- `~/.gemini/antigravity-cli/settings.json` (Gemini-migrated layout)

Hooks registration is documented per Agy's installed version; consult the
agent's own settings for the exact key shape. The hook script itself only
expects a JSON config containing a `hooks` map with `PreToolUse` entries.

## Install

1. Make sure `lain` is on `$PATH` and a `lain server` is running with HTTP
   transport.
2. Set `LAIN_URL` if lain is not on `http://localhost:9999/mcp`.
3. Edit Agy's MCP/hooks config (path above) to register the pre-edit hook,
   pointing at the absolute path of `pre-edit.sh` in this repo.

## Behavior

- **pre-edit.sh**: Calls `lain hooks claim` to register Agy as an agent and
  claim the file. Conflicts are surfaced to Agy on stderr. Lain unreachable
  → exit 0 (don't break Agy's workflow).

## Defaults

- `--agent-name` = `agy`
- `--agent-kind` = `agy`

## Multiple Agy windows

All Agy sessions share the same agent name (`agy`) and persistent session
token. If you need per-window tracking, set `LAIN_AGENT_NAME` differently
per shell.
