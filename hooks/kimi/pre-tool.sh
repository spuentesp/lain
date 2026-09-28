#!/usr/bin/env bash
# AGY (Antigravity CLI) per-tool-call observation hook.
#
# Fires on every tool invocation (Read, Grep, Bash, Edit, ...).
# Records the observation in the activity feed via `lain hooks
# observe`. The Edit-specific claim lives in `pre-edit.sh`; this
# hook is the per-tool-call complement.
#
# Identity resolution order:
#   1. $LAIN_AGENT_NAME
#   2. CLAUDE_AGENT_NAME / MCP_CLIENT_NAME / AGENT_NAME
#   3. "<kind>-<ppid>-<hostname-short>"
#
# Reads the tool name + target from stdin JSON (AGY passes
# {"tool_name": "...", "tool_input": {...}}).
# Always exits 0 — failure must NEVER block the agent.

set +e
trap 'exit 0' ERR

LAIN_URL="${LAIN_URL:-http://localhost:9999}"

# Identity.
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

# Read the JSON envelope AGY passes.
STDIN_JSON="$(cat 2>/dev/null || true)"
TOOL_NAME=""
TARGET=""
if [ -n "$STDIN_JSON" ]; then
    TOOL_NAME="$(printf '%s' "$STDIN_JSON" \
        | sed -n 's/.*"tool_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -1)"
    # tool_input is an object; pull common fields (file_path /
    # command / pattern) by name. The first match wins.
    TARGET="$(printf '%s' "$STDIN_JSON" \
        | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
        | head -1)"
    if [ -z "$TARGET" ]; then
        TARGET="$(printf '%s' "$STDIN_JSON" \
            | sed -n 's/.*"command"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
            | head -1)"
    fi
    if [ -z "$TARGET" ]; then
        TARGET="$(printf '%s' "$STDIN_JSON" \
            | sed -n 's/.*"pattern"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
            | head -1)"
    fi
fi

# If both tool name and target are empty, the hook received no
# usable payload — exit silently.
if [ -z "$TOOL_NAME" ] && [ -z "$TARGET" ]; then
    exit 0
fi

if ! command -v lain >/dev/null 2>&1; then
    echo "lain not on PATH; skipping observation" >&2
    exit 0
fi

# Fail-open: never propagate the observation error.
lain hooks observe \
    --url "$LAIN_URL" \
    --agent-name "$AGENT_NAME" \
    --agent-kind "kimi" \
    --event "tool_start" \
    --tool "${TOOL_NAME:-unknown}" \
    --target "$TARGET" >&2
exit 0
