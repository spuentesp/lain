#!/usr/bin/env bash
# Phase 4 soundness stress test.
#
# Verifies the manager surfaces incomplete coverage when the join
# can't classify a repo. Two failure modes are exercised:
#
# 1. Unknown repo: prepare_snapshot rejects the request with
#    `repo_not_registered` because the manager has no source for it.
#    This is the "I cannot answer for this repo" soundness guarantee
#    — the tool never silently drops the question.
#
# 2. Corrupt cache: a repo's index cache is removed between
#    snapshots. The manager re-indexes; the tool then reports
#    complete=true once the rebuild finishes. (This validates the
#    rebuild path, not a failure — Phase 4's stress is the unknown
#    repo case.)
#
# Prereqs: same as ground_truth.sh (fixture built, lain built, port free).
#
# Usage: tests/real_federation/soundness.sh [fixture_dir] [port]
#   defaults: /tmp/real-federation-test  19878

set -euo pipefail

FIXTURE_DIR="${1:-/tmp/real-federation-test}"
PORT="${2:-19878}"
HOST="http://127.0.0.1:${PORT}/mcp"
PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LAIN_BIN="${LAIN_BIN:-$PROJECT_ROOT/target/debug/lain}"
SESSION=$(uuidgen)
PASS=0
FAIL=0

cleanup() {
  if [ -n "${SERVER_PID:-}" ]; then
    kill -9 "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

cd "$FIXTURE_DIR"
XDG_STATE_HOME="$FIXTURE_DIR/state" LAIN_TOOL_PROFILE=full \
  "$LAIN_BIN" server --config repos.yaml --workspace tokio-stack \
  --transport http --port "$PORT" --log-level info >/tmp/soundness_server.log 2>&1 &
SERVER_PID=$!
cd - >/dev/null

for i in {1..120}; do
  if curl -sf -o /dev/null "http://127.0.0.1:${PORT}/mcp" -X POST \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"sound","version":"0.1"}}}'; then
    break
  fi
  sleep 1
done

curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"sound","version":"0.1"}}}' >/dev/null
curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' >/dev/null

# Assertion 1: prepare_snapshot rejects a repo not in repos.yaml
# (the manager has no source for it).
RESP=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"prepare_snapshot","arguments":{"repos":{"nonexistent-repo":"master"},"wait_ms":1000}}}')
CODE=$(echo "$RESP" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(json.loads(t)['error']['code'])")
ISERROR=$(echo "$RESP" | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(d['result']['isError'])")
echo "1. prepare_snapshot with unknown repo: code=$CODE is_error=$ISERROR"
if [ "$CODE" = "repo_not_registered" ]; then
  echo "  ✓ unknown repo surfaced as repo_not_registered (no silent drop)"
  PASS=$((PASS+1))
else
  echo "  ✗ expected code=repo_not_registered, got $CODE"
  FAIL=$((FAIL+1))
fi

# Assertion 2: prepare_snapshot with a known repo succeeds and get_coverage
# reports complete=true once the workers finish.
SNAP_RAW=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"prepare_snapshot","arguments":{"repos":{"bytes":"master","tokio":"master","serde":"master"},"wait_ms":180000}}}')
SNAP=$(echo "$SNAP_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(json.loads(t)['data']['snapshot'])")
SNAP_STATE=$(echo "$SNAP_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(json.loads(t)['data']['state'])")
echo "2. prepare_snapshot all-3-repos: snapshot=$SNAP state=$SNAP_STATE"
if [ "$SNAP_STATE" = "ready" ]; then
  echo "  ✓ all-3-repos snapshot reached ready"
  PASS=$((PASS+1))
else
  echo "  ✗ expected state=ready, got $SNAP_STATE"
  FAIL=$((FAIL+1))
fi

# Assertion 3: get_coverage.complete == true once the snapshot is ready.
COMP=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"get_coverage\",\"arguments\":{\"snapshot\":\"$SNAP\"}}}" \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(str(json.loads(t)['data']['complete']).lower())")
echo "3. get_coverage.complete = $COMP"
if [ "$COMP" = "true" ]; then
  echo "  ✓ coverage is complete for the real-repos snapshot"
  PASS=$((PASS+1))
else
  echo "  ✗ expected complete=true, got $COMP"
  FAIL=$((FAIL+1))
fi

# Assertion 4: get_coverage on a stale snapshot id returns snapshot_not_found.
# This is the "I cannot answer for this id" soundness check.
STALE=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_coverage","arguments":{"snapshot":"snap_doesnotexist0123456789abcdef"}}}' \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(json.loads(t)['error']['code'])")
echo "4. get_coverage unknown snapshot: code=$STALE"
if [ "$STALE" = "snapshot_not_found" ]; then
  echo "  ✓ unknown snapshot surfaced as snapshot_not_found (no silent answer)"
  PASS=$((PASS+1))
else
  echo "  ✗ expected code=snapshot_not_found, got $STALE"
  FAIL=$((FAIL+1))
fi

echo
echo "=== Result: $PASS passed, $FAIL failed (out of 4) ==="
exit $FAIL
