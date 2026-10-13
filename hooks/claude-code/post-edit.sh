#!/usr/bin/env bash
# Claude Code PostToolUse hook — releases the claim after a successful edit.
# Releases via zoanthrope (the coordination core). Always exits 0.
#
# Porting to another agent framework = two strings (see hooks/README.md).

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

# Identity resolution (same order as pre-edit.sh).
if [ -n "$ZOANTHROPE_AGENT_NAME" ]; then
    AGENT_NAME="$ZOANTHROPE_AGENT_NAME"
elif [ -n "$LAIN_AGENT_NAME" ]; then
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

if ! command -v zoanthrope >/dev/null 2>&1; then
    echo "zoanthrope not on PATH; release skipped (advisory)" >&2
    exit 0
fi

# Pipe the full release output to stderr; never blocks.
zoanthrope release --files "$FILE_PATH" --agent "$AGENT_NAME" >&2
exit 0
