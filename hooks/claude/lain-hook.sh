#!/bin/bash
# LAIN hook for Claude Code
# Thin delegator - all logic lives in `lain ask` CLI

set -e

CACHE_DIR="${HOME}/.cache/lain"
CACHE_FILE="${CACHE_DIR}/hook-version-ok"

# Check for lain binary
if ! command -v lain >/dev/null 2>&1; then
    exit 0  # Silent pass-through if lain not found
fi

# Version check (cached)
if [ -f "$CACHE_FILE" ] && [ "$(cat "$CACHE_FILE")" = "ok" ]; then
    # Skip version check
    :
else
    # Check version
    if ! lain --version >/dev/null 2>&1; then
        exit 0
    fi
    mkdir -p "$CACHE_DIR"
    echo "ok" > "$CACHE_FILE"
fi

# Read stdin, pass to lain ask
exec lain ask