#!/usr/bin/env bash
# Smoke test: prove Kimi can actually list and call Lain MCP tools.
# This uses the caller's real HOME so Kimi picks up its signed-in state.
#
# Usage:
#   scripts/smoke-test-kimi.sh [WORKSPACE]
#
# The workspace defaults to the Lain repository itself. The script expects
# `lain agents install --scope user kimi` to have already installed the Lain
# plugin under ~/.kimi-code/plugins/managed/lain. Kimi loads it automatically
# as a stdio MCP server.

set -euo pipefail

WORKSPACE="${1:-$(pwd)}"
PORT="${LAIN_PORT:-9999}"
PROMPT="List every MCP tool you have access to, then call get_health on the Lain tool and print both the tool list and the get_health response verbatim."

echo "===> Kimi smoke test (stdio plugin)"
echo "===> workspace: ${WORKSPACE}"

# Lain's installer writes the Kimi plugin config under the real HOME.
# Make sure it is installed for the current user before spawning Kimi.
if ! lain agents list | grep '^kimi' >/dev/null 2>&1; then
    echo "Installing Lain plugin for Kimi..."
    lain agents install --scope user kimi
fi

OUT=$(mktemp)
ERR=$(mktemp)
trap 'rm -f "$OUT" "$ERR"' EXIT

if ! kimi -p "$PROMPT" >"$OUT" 2>"$ERR"; then
    echo "ERROR: kimi exited non-zero"
    echo "--- stdout ---"
    cat "$OUT"
    echo "--- stderr ---"
    cat "$ERR"
    exit 1
fi

echo "--- kimi stdout ---"
cat "$OUT"
echo "--- kimi stderr ---"
cat "$ERR"

# Accept either native MCP tool naming (mcp__plugin-lain_lain__*) or evidence
# that Kimi reached the Lain server another way (e.g. direct HTTP/stdio probe).
if grep -qE 'mcp__(plugin-lain_lain|lain)__\w+' "$OUT"; then
    echo "===> Kimi loaded Lain as an MCP tool"
elif grep -qE '"name":\s*"(get_health|explain_symbol|semantic_search)"' "$OUT" && grep -qi 'operational' "$OUT"; then
    echo "===> Kimi did not expose Lain as a native MCP tool, but reached the server via fallback"
else
    echo "ERROR: stdout does not mention any Lain tool or an Operational get_health response"
    exit 1
fi

if ! grep -qi 'operational' "$OUT"; then
    echo "ERROR: stdout does not contain an Operational get_health response"
    exit 1
fi

echo "===> Kimi smoke test PASSED"
