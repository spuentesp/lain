# Kimi Pre-Edit Hook for lain

This hook auto-claims files in `lain` before Kimi edits them. Kimi's hook
config lives in `~/.kimi-code/config.toml` (TOML, with `[[hooks]]` arrays).

## Config path (verified)

- `~/.kimi-code/config.toml` — TOML with a top-level `[[hooks]]` array. Each
  entry has `event`, `command`, and `timeout`.

## Install

1. Make sure `lain` is on `$PATH` and a `lain server` is running with HTTP
   transport.
2. Set `LAIN_URL` if lain is not on `http://localhost:9999` (bare URL; the MCP `/mcp` path is appended automatically).
3. Append to `~/.kimi-code/config.toml` (preserving existing content):

```toml
# lain: claim files before edit
[[hooks]]
event = "PreToolUse"
command = "/path/to/hooks/kimi/pre-edit.sh"
timeout = 10
```

Replace `/path/to/hooks/...` with the absolute path in this repo.

## Behavior

- **pre-edit.sh**: Calls `lain hooks claim` to register Kimi as an agent and
  claim the file. Conflicts are surfaced to Kimi on stderr. Lain unreachable
  → exit 0 (don't break Kimi's workflow).

## Defaults

- `--agent-name` = `kimi`
- `--agent-kind` = `kimi`

## Multiple Kimi windows

All Kimi sessions share the same agent name (`kimi`) and persistent session
token. If you need per-window tracking, set `LAIN_AGENT_NAME` differently per
shell.


## Dynamic Dispatch Caveat

An empty `get_blast_radius` means **no static edges found**, not **no impact**. Before mutating any symbol whose blast radius is empty, run **all three**:
1. `get_coupling_radar` on the touched paths (git co-change partners).
2. `find_anchors` on the file to surface nearby high-fan-in hubs.
3. Check `docs/dynamic-boundaries.md` in the repo for documented dispatch points (message buses, DI containers, schema-driven routers).

If the file is not in `dynamic-boundaries.md` and all three queries are empty, proceed with the smoke command. Otherwise treat the result as **evidence insufficient** and require a broader smoke run before the mutation lands on disk.

You can also call `explain_dispatch <symbol>` for a single-shot synthesis with a `verdict` field — `insufficient_evidence` is the explicit "do not assume safe" signal.

Full reference: `docs/dynamic-dispatch.md` in the lain repo.
