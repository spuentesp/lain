#!/usr/bin/env bash
# Claude Code PostToolUse hook — releases the claim after a successful edit.
# Always exits 0.

set +e
trap 'exit 0' ERR

FILE_PATH="${1:-}"
if [ -z "$FILE_PATH" ]; then
    STDIN_JSON="$(cat 2>/dev/null || true)"
    if [ -n "$STDIN_JSON" ]; then
        FILE_PATH="$(printf '%s' "$STDIN_JSON" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
    fi
fi

if [ -z "$FILE_PATH" ]; then exit 0; fi

LAIN_URL="${LAIN_URL:-http://localhost:9999/mcp}"

if [ -n "$LAIN_AGENT_NAME" ]; then
    AGENT_NAME="$LAIN_AGENT_NAME"
elif [ -n "$ORCA_PANE_KEY" ] || [ -n "$ORCA_TAB_ID" ] || [ -n "$ORCA_WORKTREE_ID" ]; then
    AGENT_NAME="orca-${ORCA_PANE_KEY:-?}-${ORCA_TAB_ID:-?}-${ORCA_WORKTREE_ID:-?}"
else
    AGENT_NAME="claude-code-${PPID:-?}"
fi

if ! command -v lain >/dev/null 2>&1; then exit 0; fi

lain hooks release \
    --url "$LAIN_URL" \
    --path "$FILE_PATH" \
    --agent-name "$AGENT_NAME" \
    --agent-kind "claude-code" 2>&1 | head -1 >&2
exit 0
