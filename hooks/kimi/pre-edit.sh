#!/usr/bin/env bash
# Claude Code PreToolUse hook for Edit/Write/MultiEdit.
# Reads the file path from stdin (Claude Code passes JSON) and claims it
# via zoanthrope — the coordination core this hook family guards for.
# Always exits 0 — failure must NEVER block Claude Code. Stderr is for diagnostics.
#
# Identity resolution order:
#   1. $ZOANTHROPE_AGENT_NAME (explicit override) / $LAIN_AGENT_NAME (legacy)
#   2. Generic agent env vars any framework may set:
#      CLAUDE_AGENT_NAME, MCP_CLIENT_NAME, AGENT_NAME (and per-kind if set)
#   3. Fallback: "<kind>-<ppid>-<hostname-short>" (stable for parent process)
#
# Porting to another agent framework = two strings: the comment header
# and the identity prefix (see hooks/README.md).

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

# Identity resolution.
if [ -n "$ZOANTHROPE_AGENT_NAME" ]; then
    AGENT_NAME="$ZOANTHROPE_AGENT_NAME"
elif [ -n "$LAIN_AGENT_NAME" ]; then
    AGENT_NAME="$LAIN_AGENT_NAME"
elif [ -n "$CLAUDE_AGENT_NAME" ]; then
    AGENT_NAME="kimi-$CLAUDE_AGENT_NAME"
elif [ -n "$MCP_CLIENT_NAME" ]; then
    AGENT_NAME="$MCP_CLIENT_NAME"
elif [ -n "$AGENT_NAME" ]; then
    AGENT_NAME="$AGENT_NAME"
else
    SHORT_HOST=$(hostname -s 2>/dev/null || echo "host")
    AGENT_NAME="kimi-${PPID:-?}-${SHORT_HOST}"
fi

if ! command -v zoanthrope >/dev/null 2>&1; then
    echo "zoanthrope not on PATH; claim skipped (advisory, non-blocking)" >&2
    exit 0
fi

# Always exit 0 — claim conflicts are reported, never enforced here.
# zoanthrope prints the grant shape on success and the conflict shape
# (naming the holder) on refusal; pipe it to stderr for diagnostics.
zoanthrope claim --files "$FILE_PATH" --agent "$AGENT_NAME" --intent edit >&2
exit 0
