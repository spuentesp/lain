#!/usr/bin/env bash
# Dev SPA override — serve the SPA from disk instead of the
# embedded bytes, so JS/CSS edits don't require a cargo rebuild.
#
# Usage:
#   ./scripts/dev-spa.sh                   # default port 9999, http transport
#   ./scripts/dev-spa.sh --port 7777       # custom port
#   ./scripts/dev-spa.sh --transport stdio # stdio MCP for editor integration
#
# Pair with a browser pointed at http://localhost:9999 — edit any
# file under src/server/mcp/command_center/, save, hit refresh.
# The next request reads the new bytes from disk.
#
# Set LAIN_DEV_SPA_DIR to override the SPA root (default: the
# canonical src/server/mcp/command_center/ directory).
set -euo pipefail

export LAIN_DEV_SPA_DIR="${LAIN_DEV_SPA_DIR:-$(cd "$(dirname "$0")/.." && pwd)/src/server/mcp/command_center}"

if [[ ! -d "$LAIN_DEV_SPA_DIR" ]]; then
  echo "error: LAIN_DEV_SPA_DIR=$LAIN_DEV_SPA_DIR is not a directory" >&2
  exit 1
fi

echo "dev SPA: serving from $LAIN_DEV_SPA_DIR" >&2
echo "(unset LAIN_DEV_SPA_DIR or run cargo run directly to embed again)" >&2

exec cargo run --quiet --bin lain -- server "$@"
