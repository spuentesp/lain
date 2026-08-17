#!/usr/bin/env bash
# Claude Code PostToolUse hook — releases the claim after a successful edit.

set -uo pipefail

FILE_PATH="${1:-}"
if [ -z "$FILE_PATH" ]; then
  STDIN_JSON="$(cat 2>/dev/null || true)"
  if [ -n "$STDIN_JSON" ]; then
    FILE_PATH="$(printf '%s' "$STDIN_JSON" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
  fi
fi

if [ -z "$FILE_PATH" ]; then exit 0; fi

LAIN_URL="${LAIN_URL:-http://localhost:9999/mcp}"

if ! command -v lain >/dev/null 2>&1; then exit 0; fi

lain hooks release --url "$LAIN_URL" --path "$FILE_PATH" --agent-name "${LAIN_AGENT_NAME:-claude-code}" --agent-kind "claude-code" || true
exit 0
