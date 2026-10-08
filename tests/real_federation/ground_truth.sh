#!/usr/bin/env bash
# Phase 1 ground-truth test for the real-repos fixture (bytes + tokio
# plus the synthetic `probe` repo from demo-federation-fixture.sh).
#
# Asserts, in order:
#   1. Precision invariant: two unrelated Rust libraries joined by
#      toolchain only must yield zero contracts (zero endpoints, hence
#      zero cross-repo Binds). This is the "no false positives" floor
#      for `rejoin_contracts` — the joiner must not hallucinate edges
#      when the consumer/provider set has no actual service relationship.
#   2. No unresolved or ambiguous consumers on that snapshot.
#   3. get_coverage must NOT claim complete: tokio's coverage ledger
#      carries unresolved dynamic-SQL records, so `complete=false` is
#      the sound answer (claiming true would be a false completeness
#      claim, invariant I3).
#   4. A real change verdict: diffing probe@route (base) against
#      probe@seed (head) removes exactly one endpoint. The diff must
#      report THAT change — `EndpointRemoved` for service `probe`,
#      key `http:GET /probe`, compat Breaking, class NeedsInvestigation,
#      reason unresolved_candidates — never silence and never a
#      NoKnownImpact while any in-scope repo's coverage is incomplete.
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
  "$LAIN_BIN" server --config repos.yaml --workspace contract-suite \
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

# prepare_snapshot is idempotent but its wait is capped at 60s while a
# cold index of tokio takes longer; re-call until the state settles.
# Any state other than pending/indexing/ready is a hard failure.
prepare_ready() {
  local tag="$1" repos_json="$2" raw snap state round=0
  while :; do
    raw=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
      -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"prepare_snapshot\",\"arguments\":{\"repos\":$repos_json,\"wait_ms\":60000}}}")
    snap=$(echo "$raw" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; d=json.loads(t); print(d.get('data',{}).get('snapshot','') or d.get('error',{}).get('code','?'))")
    state=$(echo "$raw" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; d=json.loads(t); print(d.get('data',{}).get('state','') or 'error:'+d.get('error',{}).get('code','?'))")
    case "$state" in
      ready)
        echo "$snap"
        return 0
        ;;
      pending|indexing)
        round=$((round + 1))
        if [ "$round" -ge 8 ]; then
          echo "FAIL: $tag snapshot not ready after ${round} rounds (state=$state)" >&2
          return 1
        fi
        # Progress goes to stderr: callers capture this function's
        # stdout as the snapshot id (`SNAP=$(prepare_ready …)`), so a
        # line here would corrupt the id and the next tool call would
        # embed a literal newline in JSON. That is what made the suite
        # die with `KeyError: 'result'` on every cold run.
        echo "  waiting for $tag snapshot (state=$state, round $round)…" >&2
        ;;
      *)
        echo "FAIL: $tag prepare_snapshot failed (state=$state, snap=$snap)" >&2
        return 1
        ;;
    esac
  done
}

# Prepare a snapshot covering the two real repos
SNAP=$(prepare_ready ground-truth '{"bytes":"master","tokio":"master"}')
SNAP_STATE="ready"
echo "snapshot: $SNAP, state: $SNAP_STATE"

# Assertion 1: list_contracts.items == 0
ITEMS=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"list_contracts\",\"arguments\":{\"snapshot\":\"$SNAP\",\"limit\":200}}}" \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(len(json.loads(t)['data']['items']))")
echo "list_contracts.items = $ITEMS (expected $EXPECTED_ITEMS)"
[ "$ITEMS" = "$EXPECTED_ITEMS" ] || { echo "FAIL: precision regression — found $ITEMS contracts where $EXPECTED_ITEMS expected"; exit 1; }

# Assertion 2: list_unresolved has no items (unresolved + ambiguous)
UNRESOLVED=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"list_unresolved\",\"arguments\":{\"snapshot\":\"$SNAP\",\"limit\":200}}}" \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; d=json.loads(t)['data']; print(len(d['items'])+len(d.get('ambiguous',[])))")
echo "list_unresolved.items+ambiguous = $UNRESOLVED (expected $EXPECTED_UNRESOLVED)"
[ "$UNRESOLVED" = "$EXPECTED_UNRESOLVED" ] || { echo "FAIL: unresolved regression — $UNRESOLVED where $EXPECTED_UNRESOLVED expected"; exit 1; }

# Assertion 3: get_coverage.complete == ground truth (false: tokio's
# ledger carries unresolved dynamic-SQL records).
COMPLETE=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"get_coverage\",\"arguments\":{\"snapshot\":\"$SNAP\"}}}" \
  | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(str(json.loads(t)['data']['complete']).lower())")
echo "get_coverage.complete = $COMPLETE (expected $EXPECTED_COVERAGE)"
[ "$COMPLETE" = "$EXPECTED_COVERAGE" ] || { echo "FAIL: coverage regression — $COMPLETE where $EXPECTED_COVERAGE expected"; exit 1; }

# Assertion 4: verdict over a real change on the fixture.
# probe@master carries the axum route (GET /probe); its parent commit
# is README-only. Diffing route -> seed removes exactly that endpoint.
PROBE_C2=$(git -C "$FIXTURE_DIR/probe" rev-parse master)
PROBE_C1=$(git -C "$FIXTURE_DIR/probe" rev-parse master~1)
BASE_SNAP=$(prepare_ready verdict-base "{\"bytes\":\"master\",\"tokio\":\"master\",\"probe\":\"$PROBE_C2\"}")
HEAD_SNAP=$(prepare_ready verdict-head "{\"bytes\":\"master\",\"tokio\":\"master\",\"probe\":\"$PROBE_C1\"}")
echo "verdict snapshots: base=$BASE_SNAP (probe@route) head=$HEAD_SNAP (probe@seed)"

DIFF_RAW=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"diff_contracts\",\"arguments\":{\"base\":\"$BASE_SNAP\",\"head\":\"$HEAD_SNAP\"}}}")
echo "$DIFF_RAW" | python3 -c "
import json, sys
t = json.loads(sys.stdin.read())['result']['content'][0]['text']
d = json.loads(t)
if 'error' in d:
    print('FAIL: diff_contracts errored: ' + d['error']['code'], file=sys.stderr)
    sys.exit(1)
data = d['data']
changes = data['changes']

# The specific claim: removing probe's GET /probe endpoint must be
# reported as this one change, with this verdict — not silence, and
# not NoKnownImpact while any in-scope repo's coverage is incomplete
# (tokio carries unresolved dynamic-SQL records).
problems = []
if len(changes) != 1:
    problems.append(f'expected exactly 1 change, got {len(changes)}: '
                    + json.dumps([c.get(\"kind\") for c in changes]))
else:
    c = changes[0]
    if c.get('kind') != 'EndpointRemoved':
        problems.append(f\"kind={c.get('kind')} (expected EndpointRemoved)\")
    if c.get('endpoint', {}).get('service') != 'probe':
        problems.append(f\"endpoint.service={c.get('endpoint', {}).get('service')} (expected probe)\")
    if c.get('endpoint', {}).get('key') != 'http:GET /probe':
        problems.append(f\"endpoint.key={c.get('endpoint', {}).get('key')} (expected http:GET /probe)\")
    if c.get('compat') != 'Breaking':
        problems.append(f\"compat={c.get('compat')} (expected Breaking)\")
    if c.get('impact', {}).get('class') != 'NeedsInvestigation':
        problems.append(f\"impact.class={c.get('impact', {}).get('class')} (expected NeedsInvestigation)\")
    reasons = c.get('impact', {}).get('reasons', [])
    if 'unresolved_candidates' not in reasons:
        problems.append(f'impact.reasons={reasons} (expected unresolved_candidates)')
    if problems:
        print('FAIL: verdict regression for the removal of probe GET /probe:', file=sys.stderr)
        for p in problems:
            print('  - ' + p, file=sys.stderr)
        print('full change: ' + json.dumps(c), file=sys.stderr)
        sys.exit(1)
    print('verdict: removing probe http:GET /probe -> '
          + c['impact']['class'] + ' (' + ','.join(reasons) + '), compat=' + c['compat'])
    sys.exit(0)
print('FAIL: verdict regression for the removal of probe GET /probe:', file=sys.stderr)
for p in problems:
    print('  - ' + p, file=sys.stderr)
sys.exit(1)
" || exit 1
COMPAT_CHANGES=$(echo "$DIFF_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; print(json.loads(t)['data']['compatible_changes'])")
echo "  ✓ this change is reported, not silent (1 change, $COMPAT_CHANGES compatible)"

echo "OK: ground truth holds for bytes+tokio (precision=1.0, complete=false, verdict=NeedsInvestigation)"
