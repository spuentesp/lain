#!/usr/bin/env bash
# lain-health-badge helper: boot a lain HTTP server, call get_health and
# architectural_observations, format the result, emit GitHub Actions
# outputs (level, summary, body).
#
# Always emits outputs, even on the failure path, so the action's
# downstream steps (commit status, sticky comment) can post a
# meaningful state instead of empty strings.

set -uo pipefail

MIN_FAN_OUT="${INPUT_MIN_FAN_OUT:-15}"
WORKSPACE="${GITHUB_WORKSPACE:-$PWD}"

emit_outputs() {
  local level="$1"
  local summary="$2"
  local body="$3"
  {
    echo "level=$level"
    echo "summary=$summary"
    echo "body<<EOF_BODY"
    echo "$body"
    echo "EOF_BODY"
  } >> "$GITHUB_OUTPUT"
}

if ! command -v lain >/dev/null 2>&1; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed before computing metrics._"
    echo
    echo "**Error:** \`lain\` is not on PATH. The install step may not have added \`\$HOME/.local/lain\` to PATH; check the \`Install lain\` step log."
  } > "$BODY"
  emit_outputs "error" "lain binary not on PATH" "$(cat "$BODY")"
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed before computing metrics._"
    echo
    echo "**Error:** \`jq\` is not on PATH. The action requires \`jq\` on the runner; it is preinstalled on \`ubuntu-latest\`."
  } > "$BODY"
  emit_outputs "error" "jq not on PATH" "$(cat "$BODY")"
  exit 1
fi

if ! command -v nc >/dev/null 2>&1; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed before computing metrics._"
    echo
    echo "**Error:** \`nc\` is not on PATH. The action uses \`nc -z\` for the readiness check. Preinstalled on \`ubuntu-latest\`; if you are on a custom runner, install \`netcat\`."
  } > "$BODY"
  emit_outputs "error" "nc not on PATH" "$(cat "$BODY")"
  exit 1
fi

# Boot the server in the background, against the consumer's workspace.
lain server --transport http --port 9999 --workspace "$WORKSPACE" \
  --log-level warn \
  > /tmp/lain-server.log 2>&1 &
LAIN_PID=$!
trap 'kill "$LAIN_PID" 2>/dev/null || true' EXIT

# Wait for the server to be ready. The HTTP listener opens before
# the cold-start re-index completes, so a port-check is the right
# readiness signal — it confirms the server is up without waiting
# for the re-index to finish. The actual tool calls below will
# block until the re-index is done, with their own long timeout.
READY=0
for _ in $(seq 1 30); do
  if nc -z 127.0.0.1 9999 >/dev/null 2>&1; then
    READY=1
    break
  fi
  sleep 1
done

if [ "$READY" != "1" ]; then
  BODY=$(mktemp)
  {
    echo "## Architecture health"
    echo
    echo "_Action failed: lain server did not start._"
    echo
    echo "**Error:** no process listening on \`127.0.0.1:9999\` after 30s. The server crashed during startup."
    echo
    echo "**Server log tail:**"
    echo
    echo '```'
    tail -40 /tmp/lain-server.log 2>/dev/null || echo "(no log)"
    echo '```'
  } > "$BODY"
  emit_outputs "error" "lain server did not start in 30s" "$(cat "$BODY")"
  exit 1
fi

# Tool 1: get_health (no args). The call itself blocks until the
# cold re-index is done; --max-time bounds the wait at 900s.
HEALTH=$(curl -fsS --max-time 900 -X POST http://127.0.0.1:9999/mcp \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":2}' \
  | jq -r '.result.content[0].text // "error: empty result"')

# Tool 2: architectural_observations (with threshold).
ARCH=$(curl -fsS --max-time 900 -X POST http://127.0.0.1:9999/mcp \
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

emit_outputs "$LEVEL" "$SUMMARY" "$(cat "$BODY")"
