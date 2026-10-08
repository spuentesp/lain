#!/usr/bin/env bash
# Phase 2 tool-surface regression test.
#
# Boots the server, prepares a snapshot over all fixture repos (the two
# real libraries plus the synthetic `probe` repo, whose axum route gives
# the snapshot one real endpoint), then calls every one of the 13
# contract tools and asserts isError=false on each. This is the "no
# silent regression" check for the MCP surface — if a future refactor
# wires a tool to a removed subsystem, this test catches it before it
# ships.
#
# Prereqs:
#   - fixture built via `scripts/demo-federation-fixture.sh`
#   - `cargo build` produced target/debug/lain
#   - port 19876 free
#
# Usage: tests/real_federation/tools_smoke.sh [fixture_dir] [port]
#   defaults: /tmp/real-federation-test  19876

set -euo pipefail

FIXTURE_DIR="${1:-/tmp/real-federation-test}"
PORT="${2:-19876}"
HOST="http://127.0.0.1:${PORT}/mcp"
PROJECT_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LAIN_BIN="${LAIN_BIN:-$PROJECT_ROOT/target/debug/lain}"
PASS=0
FAIL=0
SESSION=$(uuidgen)

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
  --transport http --port "$PORT" --log-level info >/tmp/smoke_server.log 2>&1 &
SERVER_PID=$!
cd - >/dev/null

# Wait for the listener
for i in {1..120}; do
  if curl -sf -o /dev/null "http://127.0.0.1:${PORT}/mcp" -X POST \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0.1"}}}'; then
    break
  fi
  sleep 1
done

curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0.1"}}}' >/dev/null
curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' >/dev/null

# Read the list of repos (and their pinned refs) from the fixture so
# the test works with any fixture shape.
REPOS_JSON=$(python3 -c "
import yaml
with open('$FIXTURE_DIR/repos.yaml') as f:
  cfg = yaml.safe_load(f)
def ref(r):
  return r.get('source', {}).get('ref', 'master')
print(','.join(f'\"{r[\"id\"]}\":\"{ref(r)}\"' for r in cfg['repos']))
")

# Prepare a snapshot over all repos. The per-call wait is capped at
# 60s while a cold index takes longer; re-call until ready.
SNAP=""
SNAP_STATE=""
for round in 1 2 3 4 5 6 7 8; do
  SNAP_RAW=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"prepare_snapshot\",\"arguments\":{\"repos\":{$REPOS_JSON},\"wait_ms\":60000}}}")
  SNAP=$(echo "$SNAP_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; d=json.loads(t); print(d.get('data',{}).get('snapshot','') or d.get('error',{}).get('code','?'))")
  SNAP_STATE=$(echo "$SNAP_RAW" | python3 -c "import json,sys; t=json.loads(sys.stdin.read())['result']['content'][0]['text']; d=json.loads(t); print(d.get('data',{}).get('state','') or 'error:'+d.get('error',{}).get('code','?'))")
  case "$SNAP_STATE" in
    ready) break ;;
    pending|indexing)
      echo "  waiting for snapshot (state=$SNAP_STATE, round $round)…" >&2
      ;;
    *)
      echo "FAIL: prepare_snapshot state=$SNAP_STATE (expected ready)"
      exit 1
      ;;
  esac
done
if [ "$SNAP_STATE" != "ready" ]; then
  echo "FAIL: snapshot did not reach ready (state=$SNAP_STATE)"
  exit 1
fi
echo "snapshot: $SNAP"

check() {
  local name="$1" args="$2" label="$3"
  local raw iserror
  raw=$(curl -s -X POST "$HOST" -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" -H "Mcp-Session-Id: $SESSION" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$name\",\"arguments\":$args}}")
  iserror=$(echo "$raw" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('result',{}).get('isError','?'))" 2>/dev/null || echo "PARSE")
  if [ "$iserror" = "False" ]; then
    echo "  ✓ $label"
    PASS=$((PASS+1))
  else
    err=$(echo "$raw" | python3 -c "import json,sys; d=json.load(sys.stdin); t=d.get('result',{}).get('content',[{}])[0].get('text',''); print(json.loads(t).get('error',{}).get('code','?'))" 2>/dev/null || echo "?")
    echo "  ✗ $label (is_error=$iserror, code=$err)"
    FAIL=$((FAIL+1))
  fi
}

echo "=== 13 contract tools (fixture: $FIXTURE_DIR) ==="
check "get_snapshot"     "{\"snapshot\":\"$SNAP\",\"wait_ms\":500}"   "1. get_snapshot"
check "list_services"    "{\"snapshot\":\"live\",\"limit\":3}"       "2. list_services (live)"
check "get_service"      "{\"snapshot\":\"live\",\"service\":\"bytes\",\"depth\":1}" "3. get_service (live)"
check "list_contracts"   "{\"snapshot\":\"$SNAP\",\"limit\":5}"       "4. list_contracts"
check "get_contract"     "{\"snapshot\":\"$SNAP\",\"key\":\"http:GET /probe\"}" "5. get_contract (probe endpoint)"
check "list_unresolved"  "{\"snapshot\":\"$SNAP\",\"limit\":5}"       "6. list_unresolved"
check "check_binding"    "{\"snapshot\":\"$SNAP\",\"consumer\":\"bytes:HttpClientCall:src/lib.rs:probe:1\",\"endpoint\":{\"service\":\"bytes\",\"key\":\"http:GET /x\"}}" "7. check_binding (synthetic)"
check "diff_contracts"   "{\"snapshot\":\"$SNAP\",\"base\":\"$SNAP\",\"head\":\"$SNAP\"}" "8. diff_contracts"
check "trace_impact"     "{\"snapshot\":\"$SNAP\",\"from\":{\"endpoint\":{\"service\":\"probe\",\"key\":\"http:GET /probe\"}},\"depth\":2}" "9. trace_impact (probe endpoint)"
check "get_coverage"     "{\"snapshot\":\"$SNAP\"}"                  "10. get_coverage"
check "resolve_evidence" "{\"snapshot\":\"$SNAP\",\"refs\":[\"bytes::src/lib.rs:42\"]}" "11. resolve_evidence (synthetic)"
check "read_source"      "{\"snapshot\":\"$SNAP\",\"repo\":\"bytes\",\"path\":\"src/lib.rs\",\"start\":0,\"end\":5}" "12. read_source"
check "prepare_snapshot" "{\"repos\":{$REPOS_JSON},\"wait_ms\":100}"  "13. prepare_snapshot (no-wait)"

echo
echo "=== Result: $PASS passed, $FAIL failed (out of 13) ==="
exit $FAIL
