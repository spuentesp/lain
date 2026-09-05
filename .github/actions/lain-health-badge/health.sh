#!/usr/bin/env bash
# lain-health-badge helper: boot a lain HTTP server, call get_health and
# architectural_observations, format the result, emit GitHub Actions
# outputs (level, summary, body).

set -euo pipefail

MIN_FAN_OUT="${INPUT_MIN_FAN_OUT:-15}"
WORKSPACE="${GITHUB_WORKSPACE:-$PWD}"

if ! command -v jq >/dev/null 2>&1; then
  echo "::error::lain-health-badge requires 'jq' on PATH" >&2
  exit 1
fi

# Boot the server in the background, against the consumer's workspace.
lain server --transport http --port 9999 --workspace "$WORKSPACE" \
  --log-level warn \
  > /tmp/lain-server.log 2>&1 &
LAIN_PID=$!
trap 'kill "$LAIN_PID" 2>/dev/null || true' EXIT

# Wait for the server to be ready (poll tools/list).
READY=0
for _ in $(seq 1 60); do
  if curl -fsS -X POST http://127.0.0.1:9999/mcp \
       -H 'Content-Type: application/json' \
       -d '{"jsonrpc":"2.0","method":"tools/list","id":1}' >/dev/null 2>&1; then
    READY=1
    break
  fi
  sleep 1
done

if [ "$READY" != "1" ]; then
  echo "::error::lain server did not become ready on port 9999 in 60s" >&2
  echo "::group::lain server log"
  cat /tmp/lain-server.log || true
  echo "::endgroup::"
  exit 1
fi

# Tool 1: get_health (no args).
HEALTH=$(curl -fsS -X POST http://127.0.0.1:9999/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":2}' \
  | jq -r '.result.content[0].text // "error: empty result"')

# Tool 2: architectural_observations (with threshold).
ARCH=$(curl -fsS -X POST http://127.0.0.1:9999/mcp \
  -H 'Content-Type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"architectural_observations\",\"arguments\":{\"min_fan_out\":${MIN_FAN_OUT}}},\"id\":3}" \
  | jq -r '.result.content[0].text // "error: empty result"')

# Decide level from the prose output. A single rule: fail if the graph
# is degraded. Everything else is success. See the plan doc for the
# rationale — the only condition where the badge output is actively
# misleading is a stale graph, so that's the only thing worth failing.
LEVEL=success
SUMMARY="Architecture health computed"
if grep -q "Degraded" <<<"$HEALTH"; then
  LEVEL=failure
  SUMMARY="Lain reports a degraded graph"
fi

# Render the markdown body.
BODY=$(mktemp)
{
  echo "## Architecture health"
  echo
  echo "_Computed by [lain](https://github.com/spuentesp/lain) — thresholds: min-fan-out=${MIN_FAN_OUT}_"
  echo
  echo "### Server health"
  echo
  echo '```'
  echo "$HEALTH"
  echo '```'
  echo
  echo "### Architectural observations (fan-out >= ${MIN_FAN_OUT})"
  echo
  echo '```'
  echo "$ARCH"
  echo '```'
} > "$BODY"

# Emit GitHub Actions outputs.
{
  echo "level=$LEVEL"
  echo "summary=$SUMMARY"
  echo "body<<EOF_BODY"
  cat "$BODY"
  echo "EOF_BODY"
} >> "$GITHUB_OUTPUT"
