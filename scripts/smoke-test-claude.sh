#!/usr/bin/env bash
# Smoke test: prove Claude Code can actually list and call Lain MCP tools.
# This uses the caller's real HOME so Claude Code picks up its signed-in state.
# Claude Code requires an authenticated session; run this from an interactive
# shell where `claude` is already signed in.
#
# Usage:
#   scripts/smoke-test-claude.sh [WORKSPACE]
#
# The workspace defaults to the Lain repository itself. The script expects
# `lain agents install --scope user claude` to have already written the MCP
# config to ~/.claude/settings.json. Claude Code is launched with
# --mcp-config pointing at that file.

set -euo pipefail

WORKSPACE="${1:-$(pwd)}"
PORT="${LAIN_PORT:-9999}"
PROMPT="List every MCP tool you have access to, then call get_health on the Lain tool and print both the tool list and the get_health response verbatim."

CONFIG="${HOME}/.claude/settings.json"
echo "===> Claude Code smoke test with --mcp-config ${CONFIG}"
echo "===> workspace: ${WORKSPACE}"

# Lain's installer writes the Claude Code MCP config under the real HOME.
# Make sure it is installed for the current user before spawning Claude.
if ! lain agents list | grep '^claude' >/dev/null 2>&1; then
    echo "Installing Lain plugin for Claude Code..."
    lain agents install --scope user claude
fi

OUT=$(mktemp)
ERR=$(mktemp)
trap 'rm -f "$OUT" "$ERR"' EXIT

if ! claude --permission-mode bypassPermissions --mcp-config "$CONFIG" -- "$PROMPT" >"$OUT" 2>"$ERR"; then
    echo "ERROR: claude exited non-zero (likely needs authentication)"
    echo "--- stdout ---"
    cat "$OUT"
    echo "--- stderr ---"
    cat "$ERR"
    exit 1
fi

echo "--- claude stdout ---"
cat "$OUT"
echo "--- claude stderr ---"
cat "$ERR"

# Accept either the stdio plugin naming (mcp__plugin-lain_lain__*) or the
# HTTP URL naming (mcp__lain__*) depending on how the agent loaded the server.
if ! grep -qE 'mcp__(plugin-lain_lain|lain)__\w+' "$OUT"; then
    echo "ERROR: stdout does not mention any mcp__lain__* or mcp__plugin-lain_lain__* tool"
    exit 1
fi

if ! grep -qi 'operational' "$OUT"; then
    echo "ERROR: stdout does not contain an Operational get_health response"
    exit 1
fi

echo "===> Claude Code smoke test PASSED"
