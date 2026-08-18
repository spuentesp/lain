# Claude Code Pre/Post-Edit Hooks for lain

These hooks auto-claim files in `lain` before Claude Code edits them, and release after.

## Install

1. Make sure `lain` is on `$PATH` and a `lain server` is running with HTTP transport.
2. Set `LAIN_URL` (default: `http://localhost:9999`). The MCP `/mcp` path is appended automatically by the CLI; passing `http://localhost:9999/mcp` also still works.
3. Add to `~/.claude/settings.json` (project or user):

```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Edit|Write|MultiEdit", "hooks": [{ "type": "command", "command": "/path/to/hooks/claude-code/pre-edit.sh" }] }
    ],
    "PostToolUse": [
      { "matcher": "Edit|Write|MultiEdit", "hooks": [{ "type": "command", "command": "/path/to/hooks/claude-code/post-edit.sh" }] }
    ]
  }
}
```

Replace `/path/to/hooks/...` with the absolute path in this repo.

## Behavior

- **pre-edit.sh**: Calls `lain hooks claim` to register Claude Code as an agent and claim the file. If lain reports conflicts, the script writes the conflict JSON to stderr — Claude Code surfaces it in the conversation.
- **post-edit.sh**: Calls `lain hooks release` after the edit. If lain is unreachable, the script silently exits 0 (don't break Claude's workflow).

## Verification

Edit a file with Claude Code and run `curl http://localhost:9999/mcp -d '...list_active_agents...'` to see Claude Code's session.

## Multiple Claude Code windows

All Claude Code sessions share the same agent name (`claude-code`) and session token. They appear as ONE agent in lain. If you need per-window tracking, set `LAIN_AGENT_NAME` differently per shell.
