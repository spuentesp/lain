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

# Identity resolution.
if [ -n "$LAIN_AGENT_NAME" ]; then
    AGENT_NAME="$LAIN_AGENT_NAME"
elif [ -n "$CLAUDE_AGENT_NAME" ]; then
    AGENT_NAME="claude-code-$CLAUDE_AGENT_NAME"
elif [ -n "$MCP_CLIENT_NAME" ]; then
    AGENT_NAME="$MCP_CLIENT_NAME"
elif [ -n "$AGENT_NAME" ]; then
    AGENT_NAME="$AGENT_NAME"
else
    SHORT_HOST=$(hostname -s 2>/dev/null || echo "host")
    AGENT_NAME="claude-code-${PPID:-?}-${SHORT_HOST}"
fi

if ! command -v lain >/dev/null 2>&1; then exit 0; fi

lain hooks release \
    --url "$LAIN_URL" \
    --path "$FILE_PATH" \
    --agent-name "$AGENT_NAME" \
    --agent-kind "claude-code" 2>&1 | head -1 >&2
exit 0
