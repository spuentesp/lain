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
2. Set `LAIN_URL` if lain is not on `http://localhost:9999` (bare URL; the MCP `/mcp` path is appended automatically).
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


## Dynamic Dispatch Caveat

An empty `get_blast_radius` means **no static edges found**, not **no impact**. Before mutating any symbol whose blast radius is empty, run **all three**:
1. `get_coupling_radar` on the touched paths (git co-change partners).
2. `find_anchors` on the file to surface nearby high-fan-in hubs.
3. Check `docs/dynamic-boundaries.md` in the repo for documented dispatch points (message buses, DI containers, schema-driven routers).

If the file is not in `dynamic-boundaries.md` and all three queries are empty, proceed with the smoke command. Otherwise treat the result as **evidence insufficient** and require a broader smoke run before the mutation lands on disk.

You can also call `explain_dispatch <symbol>` for a single-shot synthesis with a `verdict` field — `insufficient_evidence` is the explicit "do not assume safe" signal.

Full reference: `docs/dynamic-dispatch.md` in the lain repo.
