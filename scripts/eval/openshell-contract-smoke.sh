#!/usr/bin/env bash
# Smoke: OpenShell's proto surface and its Python SDK must show a live
# RPC contract with a bound consumer in the federation view.
#
# Usage: scripts/eval/openshell-contract-smoke.sh <workspace-dir> [port]
# <workspace-dir> must contain the NVIDIA clones:
#   OpenShell/ NeMo-Fabric/
set -euo pipefail

LAIN_BIN="${LAIN_BIN:-./target/release/lain}"
WS="${1:?usage: $0 <workspace-dir> [port]}"
PORT="${2:-9971}"
# One OpenShell index pass measured 128 s; the per-repo default is 60 s.
INDEX_TIMEOUT="${LAIN_INDEX_TIMEOUT:-600}"
WORK="$(mktemp -d)"

cat > "$WORK/repos.yaml" <<EOF
data_dir: $WORK/.lain/federation
max_concurrent_indexers: 3
repos:
  - id: openshell
    packages: ["@nvidia/openshell-sdk", "openshell"]
    source:
      type: workspace_dir
      path: $WS/OpenShell
  - id: nemo-fabric
    packages: ["nemo-fabric"]
    source:
      type: workspace_dir
      path: $WS/NeMo-Fabric
services:
  - name: openshell-gateway
    repo: openshell
    paths: [proto, crates]
    hosts: [localhost, 127.0.0.1]
    env: [OPENSHELL_GATEWAY]
  - name: openshell-sdk
    repo: openshell
    paths: [python, sdk]
  - name: nemo-fabric
    repo: nemo-fabric
EOF

LAIN_INDEX_TIMEOUT="$INDEX_TIMEOUT" "$LAIN_BIN" server --config "$WORK/repos.yaml" \
  --transport http --port "$PORT" > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true; [[ "${KEEP_WORK:-0}" == 1 ]] || rm -rf "$WORK"' EXIT

# Wait for every repo to leave `indexing`. Liveness alone is not enough:
# /health answers before the index passes finish.
ready=0
states="unreachable"
for _ in $(seq 1 360); do
  body=$(curl -sf -m 5 "http://127.0.0.1:$PORT/health" || true)
  states=$(printf '%s' "$body" | python3 -c '
import json, sys
try:
    repos = json.load(sys.stdin)["federation"]["repos"]
    print(" ".join(r["health"] for r in repos) or "none")
except Exception:
    print("unreachable")')
  if [[ "$states" != *indexing* && "$states" != "unreachable" && "$states" != "none" ]]; then
    ready=1
    break
  fi
  sleep 5
done
if [[ "$ready" -ne 1 ]]; then
  echo "FAIL: federation did not finish indexing (last states: $states)" >&2
  tail -20 "$WORK/server.log" >&2
  exit 1
fi
echo "federation states: $states"

# One MCP tools/call over the streamable HTTP endpoint.
call() {
  curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}

RPC=$(call list_contracts '{"snapshot":"live","kind":"rpc","limit":1000}')
# The result carries the page as JSON text; find its `items` array wherever
# the envelope nests it. No `items` array is a failure, not an empty page.
SUMMARY=$(printf '%s' "$RPC" | python3 -c '
import json, sys

def find_items(node):
    if isinstance(node, dict):
        if isinstance(node.get("items"), list):
            return node["items"]
        for v in node.values():
            found = find_items(v)
            if found is not None:
                return found
    elif isinstance(node, list):
        for v in node:
            found = find_items(v)
            if found is not None:
                return found
    return None

envelope = json.load(sys.stdin)
text = envelope.get("result", {}).get("content", [{}])[0].get("text", "")
items = find_items(json.loads(text)) if text else None
if items is None:
    sys.exit("no items array in list_contracts response: " + json.dumps(envelope)[:500])
print(len(items), sum(i.get("bound_consumers", 0) for i in items))
') || { echo "FAIL: could not read list_contracts response" >&2; exit 1; }
read -r CONTRACTS BOUND <<<"$SUMMARY"

echo "rpc contracts: $CONTRACTS"
echo "bound consumers: $BOUND"

if [ "$BOUND" -lt 1 ]; then
  echo "FAIL: no RPC contract has a bound consumer" >&2
  exit 1
fi
echo "PASS: at least one RPC contract binds a consumer"
