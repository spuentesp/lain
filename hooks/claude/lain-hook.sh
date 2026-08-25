#!/bin/bash
# LAIN hook for Claude Code
# Thin delegator - all logic lives in `lain ask` CLI
#
# Fail-open, always (wishlist #1): a coordination layer that goes down
# must degrade to "no awareness", never to "no tool calls". Every exit
# path in this script is 0.

set +e

CACHE_DIR="${HOME}/.cache/lain"
CACHE_FILE="${CACHE_DIR}/hook-version-ok"

# Check for lain binary
if ! command -v lain >/dev/null 2>&1; then
    exit 0  # Silent pass-through if lain not found
fi

# Version check (cached)
if [ -f "$CACHE_FILE" ] && [ "$(cat "$CACHE_FILE" 2>/dev/null)" = "ok" ]; then
    # Skip version check
    :
else
    # Check version
    if ! lain --version >/dev/null 2>&1; then
        exit 0
    fi
    mkdir -p "$CACHE_DIR" 2>/dev/null && echo "ok" > "$CACHE_FILE" 2>/dev/null
fi

# Read stdin, pass to lain ask as the positional question (the current
# CLI requires one; invoking it bare exits 2, which a PreToolUse hook
# framework reads as BLOCK — that was the tool-call lockout bug).
question="$(cat 2>/dev/null)"
[ -z "$question" ] && exit 0
lain ask "$question" 2>/dev/null
exit 0
