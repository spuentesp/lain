#!/usr/bin/env bash
# Kimi PreToolUse hook for file write/edit tools.
# Reads the file path from stdin (Kimi passes JSON) and claims it via lain.
# Exit codes: 0 = OK (including conflicts — Kimi decides what to do)
#             1 = infrastructure failure (lain unreachable, malformed response)

set -uo pipefail

FILE_PATH="${1:-}"
if [ -z "$FILE_PATH" ]; then
  # Try to read from stdin (Kimi passes JSON).
  STDIN_JSON="$(cat 2>/dev/null || true)"
  if [ -n "$STDIN_JSON" ]; then
    FILE_PATH="$(printf '%s' "$STDIN_JSON" | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
  fi
fi

if [ -z "$FILE_PATH" ]; then
  exit 0
fi

LAIN_URL="${LAIN_URL:-http://localhost:9999/mcp}"

if ! command -v lain >/dev/null 2>&1; then
  exit 0
fi

if ! lain hooks claim --url "$LAIN_URL" --path "$FILE_PATH" --agent-name "kimi" --agent-kind "kimi" --intent edit; then
  # Infrastructure failure (lain unreachable, etc.) — let Kimi edit anyway.
  exit 0
fi
exit 0
