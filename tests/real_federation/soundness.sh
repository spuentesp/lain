#!/usr/bin/env bash
# Phase 4 soundness stress test.
#
# Verifies the manager surfaces incomplete coverage when the join
# can't classify a repo. Four failure modes are exercised:
#
# 1. Unknown repo: prepare_snapshot rejects the request with
#    `repo_not_registered` because the manager has no source for it.
#    This is the "I cannot answer for this repo" soundness guarantee
#    — the tool never silently drops the question.
#
# 2. Known repos: prepare_snapshot over the two real repos reaches
#    `ready` (re-preparing while indexing, since the wait is capped
#    at 60s and a cold tokio index takes longer).
#
# 3. get_coverage.complete == false: tokio's coverage ledger carries
#    unresolved dynamic-SQL records, so `complete=true` would be a
#    false completeness claim. The sound answer — and the ground
#    truth in tests/fixtures/contracts/ground_truth_real.json — is
#    `false`.
#
# 4. Stale snapshot id: get_coverage returns `snapshot_not_found`.
#    This is the "I cannot answer for this id" soundness check.
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
GROUND_TRUTH="tests/fixtures/contracts/ground_truth_real.json"
EXPECTED_COVERAGE=$(python3 -c "import json; print(json.load(open('$GROUND_TRUTH'))['invariants']['get_coverage_complete'])" | tr 'A-Z' 'a-z')
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
  "$LAIN_BIN" server --config repos.yaml --workspace contract-suite \
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

# Assertion 2: prepare_snapshot with the known repos succeeds and
# reaches ready (re-call while indexing: the per-call wait is capped
# at 60s, a cold tokio index takes longer).
SNAP=""
SNAP_STATE=""
for round in 1 2 3 4 5 6 7 8; do
  SNAP_RAW=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
    -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"prepare_snapshot","arguments":{"repos":{"bytes":"master","tokio":"master"},"wait_ms":60000}}}')
  SNAP=$(echo "$SNAP_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; d=json.loads(t); print(d.get('data',{}).get('snapshot','') or d.get('error',{}).get('code','?'))")
  SNAP_STATE=$(echo "$SNAP_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; d=json.loads(t); print(d.get('data',{}).get('state','') or 'error:'+d.get('error',{}).get('code','?'))")
  case "$SNAP_STATE" in
    ready) break ;;
    pending|indexing)
      echo "  waiting for snapshot (state=$SNAP_STATE, round $round)…"
      ;;
    *)
      echo "  ✗ prepare_snapshot failed early: state=$SNAP_STATE"
      break
      ;;
  esac
done
echo "2. prepare_snapshot both real repos: snapshot=$SNAP state=$SNAP_STATE"
if [ "$SNAP_STATE" = "ready" ]; then
  echo "  ✓ bytes+tokio snapshot reached ready"
  PASS=$((PASS+1))
else
  echo "  ✗ expected state=ready, got $SNAP_STATE"
  FAIL=$((FAIL+1))
fi

# Assertion 3: get_coverage must NOT claim complete while tokio's
# ledger carries unresolved records (ground truth: false).
COMP=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"get_coverage\",\"arguments\":{\"snapshot\":\"$SNAP\"}}}" \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(str(json.loads(t)['data']['complete']).lower())")
echo "3. get_coverage.complete = $COMP (expected $EXPECTED_COVERAGE)"
if [ "$COMP" = "$EXPECTED_COVERAGE" ]; then
  echo "  ✓ coverage makes no false completeness claim (complete=$COMP)"
  PASS=$((PASS+1))
else
  echo "  ✗ expected complete=$EXPECTED_COVERAGE, got $COMP"
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
