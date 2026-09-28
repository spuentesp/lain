# Codex Pre-Edit Hook for lain

This hook auto-claims files in `lain` before Codex CLI edits them. Codex's
hook config lives in `~/.codex/config.toml` (TOML, with `[[hooks.PreToolUse]]`
arrays). Codex also accepts `~/.codex/hooks.json` for the same purpose; the
TOML form is canonical on this host.

## Config path (verified)

- `~/.codex/config.toml` — TOML with a top-level `[[hooks.PreToolUse]]`
  array. Each entry has `matcher` and a nested `[[hooks.PreToolUse.hooks]]`
  array of `{type, command, timeout}` entries.
- `~/.codex/hooks.json` — alternative JSON form: a top-level `hooks.PreToolUse`
  array of `{matcher, hooks:[{type, command, timeout}]}` entries.

## Install

1. Make sure `lain` is on `$PATH` and a `lain server` is running with HTTP
   transport.
2. Set `LAIN_URL` if lain is not on `http://localhost:9999` (bare URL; the MCP `/mcp` path is appended automatically).
3. Append to `~/.codex/config.toml` (preserving existing content):

```toml
# lain: claim files before edit
[[hooks.PreToolUse]]
matcher = "edit_file|apply_patch|create_file"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "/path/to/hooks/codex/pre-edit.sh"
timeout = 10
```

Replace `/path/to/hooks/...` with the absolute path in this repo. Adjust
the `matcher` to the actual write/edit tool names exposed by your Codex
version (e.g. `edit_file`, `write_file`, `apply_patch`, `create_file`).

If you prefer JSON, write `~/.codex/hooks.json` instead:

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "edit_file|apply_patch|create_file",
        "hooks": [{ "type": "command", "command": "/path/to/hooks/codex/pre-edit.sh", "timeout": 10 }] }
    ]
  }
}
```

## Behavior

- **pre-edit.sh**: Calls `lain hooks claim` to register Codex as an agent
  and claim the file. Conflicts are surfaced to Codex on stderr. Lain
  unreachable → exit 0 (don't break Codex's workflow).

## Defaults

- `--agent-name` = `codex`
- `--agent-kind` = `codex`

## Multiple Codex windows

All Codex sessions share the same agent name (`codex`) and persistent
session token. If you need per-window tracking, set `LAIN_AGENT_NAME`
differently per shell.


## Dynamic Dispatch Caveat

An empty `get_blast_radius` means **no static edges found**, not **no impact**. Before mutating any symbol whose blast radius is empty, run **all three**:
1. `get_coupling_radar` on the touched paths (git co-change partners).
2. `find_anchors` on the file to surface nearby high-fan-in hubs.
3. Check `docs/dynamic-boundaries.md` in the repo for documented dispatch points (message buses, DI containers, schema-driven routers).

If the file is not in `dynamic-boundaries.md` and all three queries are empty, proceed with the smoke command. Otherwise treat the result as **evidence insufficient** and require a broader smoke run before the mutation lands on disk.

You can also call `explain_dispatch <symbol>` for a single-shot synthesis with a `verdict` field — `insufficient_evidence` is the explicit "do not assume safe" signal.

Full reference: `docs/dynamic-dispatch.md` in the lain repo.
