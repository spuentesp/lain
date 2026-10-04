#!/usr/bin/env bash
# Phase 1 ground-truth test for the real-repos fixture (bytes+serde+tokio).
#
# Asserts the precision invariant: for three unrelated Rust libraries
# joined by toolchain only, the contract joiner must find zero
# cross-repo Binds.  This is the "no false positives" floor for
# `rejoin_contracts` and is the test that proves the joiner doesn't
# hallucinate edges when the consumer/provider set has no actual
# service relationship.
#
# Prereqs:
#   - `scripts/demo-federation-fixture.sh <dir>` already ran (network)
#   - `cargo build` produced target/debug/lain
#   - port 19876 free
#
# Usage: tests/real_federation/ground_truth.sh [fixture_dir] [port]
#   defaults: /tmp/real-federation-test  19876

set -euo pipefail

FIXTURE_DIR="${1:-/tmp/real-federation-test}"
PORT="${2:-19876}"
HOST="http://127.0.0.1:${PORT}/mcp"
PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LAIN_BIN="${LAIN_BIN:-$PROJECT_ROOT/target/debug/lain}"
GROUND_TRUTH="tests/fixtures/contracts/ground_truth_real.json"
EXPECTED_ITEMS=$(python3 -c "import json; print(json.load(open('$GROUND_TRUTH'))['invariants']['list_contracts_items'])")
EXPECTED_UNRESOLVED=$(python3 -c "import json; print(json.load(open('$GROUND_TRUTH'))['invariants']['list_unresolved_total'])")
EXPECTED_COVERAGE=$(python3 -c "import json; print(json.load(open('$GROUND_TRUTH'))['invariants']['get_coverage_complete'])" | tr 'A-Z' 'a-z')

cleanup() {
  if [ -n "${SERVER_PID:-}" ]; then
    kill -9 "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# Boot the server
cd "$FIXTURE_DIR"
XDG_STATE_HOME="$FIXTURE_DIR/state" LAIN_TOOL_PROFILE=full \
  "$LAIN_BIN" server --config repos.yaml --workspace tokio-stack \
  --transport http --port "$PORT" --log-level info >/tmp/gt_server.log 2>&1 &
SERVER_PID=$!
cd - >/dev/null

# Wait for the listener
for i in {1..120}; do
  if curl -sf -o /dev/null "http://127.0.0.1:${PORT}/mcp" -X POST \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"gt","version":"0.1"}}}'; then
    break
  fi
  sleep 1
done

SESSION=$(uuidgen)
curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"gt","version":"0.1"}}}' >/dev/null
curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' >/dev/null

# Prepare a snapshot covering all three repos
SNAP_RAW=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"prepare_snapshot","arguments":{"repos":{"bytes":"master","tokio":"master","serde":"master"},"wait_ms":180000}}}')
SNAP=$(echo "$SNAP_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(json.loads(t)['data']['snapshot'])")
SNAP_STATE=$(echo "$SNAP_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(json.loads(t)['data']['state'])")
echo "snapshot: $SNAP, state: $SNAP_STATE"

if [ "$SNAP_STATE" != "ready" ]; then
  echo "FAIL: snapshot did not reach ready (got: $SNAP_STATE)"
  exit 1
fi

# Assertion 1: list_contracts.items == 0
ITEMS=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"list_contracts\",\"arguments\":{\"snapshot\":\"$SNAP\",\"limit\":200}}}" \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(len(json.loads(t)['data']['items']))")
echo "list_contracts.items = $ITEMS (expected $EXPECTED_ITEMS)"
[ "$ITEMS" = "$EXPECTED_ITEMS" ] || { echo "FAIL: precision regression — found $ITEMS cross-repo Binds where 0 expected"; exit 1; }

# Assertion 2: list_unresolved has no items (unresolved + ambiguous)
UNRESOLVED=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"list_unresolved\",\"arguments\":{\"snapshot\":\"$SNAP\",\"limit\":200}}}" \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; d=json.loads(t)['data']; print(len(d['items'])+len(d.get('ambiguous',[])))")
echo "list_unresolved.items+ambiguous = $UNRESOLVED (expected $EXPECTED_UNRESOLVED)"
[ "$UNRESOLVED" = "$EXPECTED_UNRESOLVED" ] || { echo "FAIL: unresolved regression — $UNRESOLVED where 0 expected"; exit 1; }

# Assertion 3: get_coverage.complete == true
COMPLETE=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"get_coverage\",\"arguments\":{\"snapshot\":\"$SNAP\"}}}" \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(str(json.loads(t)['data']['complete']).lower())")
echo "get_coverage.complete = $COMPLETE (expected $EXPECTED_COVERAGE)"
[ "$COMPLETE" = "$EXPECTED_COVERAGE" ] || { echo "FAIL: coverage regression — $COMPLETE where $EXPECTED_COVERAGE expected"; exit 1; }

echo "OK: ground truth holds for bytes+serde+tokio (precision=1.0, complete=true)"
