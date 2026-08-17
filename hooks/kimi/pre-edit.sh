#!/usr/bin/env bash
# Kimi PreToolUse hook for file write/edit tools.
# Reads the file path from stdin (Kimi passes JSON) and claims it via lain.
# Always exits 0 — failure must NEVER block Kimi. Stderr is for diagnostics.
#
# Identity resolution order:
#   1. $LAIN_AGENT_NAME (explicit lain override)
#   2. Generic agent env vars any framework may set:
#      CLAUDE_AGENT_NAME, MCP_CLIENT_NAME, AGENT_NAME (and per-kind if set)
#   3. Fallback: "<kind>-<ppid>-<hostname-short>" (stable for parent process)

set +e  # disable errexit for the rest of the script — must exit 0
trap 'exit 0' ERR  # any error → silent exit 0

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
    AGENT_NAME="kimi-${PPID:-?}-${SHORT_HOST}"
fi

if ! command -v lain >/dev/null 2>&1; then
    echo "lain not on PATH; skipping claim" >&2
    exit 0
fi

# Always exit 0 — capture stderr for diagnostics but never propagate.
lain hooks claim \
    --url "$LAIN_URL" \
    --path "$FILE_PATH" \
    --agent-name "$AGENT_NAME" \
    --agent-kind "kimi" \
    --intent edit 2>&1 | head -1 >&2
exit 0
