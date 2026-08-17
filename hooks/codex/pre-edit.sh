#!/usr/bin/env bash
# Codex CLI pre-edit hook for file write/edit tools.
# Reads the file path from stdin (Codex passes JSON) and claims it via lain.
# Always exits 0 — failure must NEVER block Codex. Stderr is for diagnostics.
#
# Identity resolution order:
#   1. $LAIN_AGENT_NAME (explicit override)
#   2. Orca env vars (ORCA_PANE_KEY / ORCA_TAB_ID / ORCA_WORKTREE_ID)
#   3. Fallback: "<basename>-<ppid>" (stable for the parent Codex process)

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
elif [ -n "$ORCA_PANE_KEY" ] || [ -n "$ORCA_TAB_ID" ] || [ -n "$ORCA_WORKTREE_ID" ]; then
    AGENT_NAME="orca-${ORCA_PANE_KEY:-?}-${ORCA_TAB_ID:-?}-${ORCA_WORKTREE_ID:-?}"
else
    AGENT_NAME="codex-${PPID:-?}"
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
    --agent-kind "codex" \
    --intent edit 2>&1 | head -1 >&2
exit 0
