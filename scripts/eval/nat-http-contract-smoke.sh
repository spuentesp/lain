#!/usr/bin/env bash
# Smoke: the NeMo Agent Toolkit workspace must show HTTP contracts that
# static analysis cannot see (declared in `provides`), no ANY-method
# garbage routes, and the UI's Next.js API routes.
#
# Usage: scripts/eval/nat-http-contract-smoke.sh <workspace-dir> [port]
# <workspace-dir> must contain the NVIDIA clones:
#   NeMo-Agent-Toolkit/ NeMo-Agent-Toolkit-UI/ NeMo-Agent-Toolkit-Examples/
set -euo pipefail

LAIN_BIN="${LAIN_BIN:-./target/release/lain}"
WS="${1:?usage: $0 <workspace-dir> [port]}"
PORT="${2:-9972}"
INDEX_TIMEOUT="${LAIN_INDEX_TIMEOUT:-900}"
WORK="$(mktemp -d)"
trap 'kill "${SERVER_PID:-0}" 2>/dev/null || true; [[ "${KEEP_WORK:-0}" == 1 ]] || rm -rf "$WORK"' EXIT

cat > "$WORK/repos.yaml" <<EOF
data_dir: $WORK/.lain/federation
max_concurrent_indexers: 3
repos:
  - id: nat
    source: {type: workspace_dir, path: $WS/NeMo-Agent-Toolkit}
  - id: nat-ui
    source: {type: workspace_dir, path: $WS/NeMo-Agent-Toolkit-UI}
  - id: nat-examples
    source: {type: workspace_dir, path: $WS/NeMo-Agent-Toolkit-Examples}
services:
  - {name: nat, repo: nat, hosts: [localhost, 127.0.0.1], env: [NAT_BACKEND_URL, NAT_SERVER_URL, NAT_URL, NEXT_PUBLIC_NAT_URL]}
  - {name: nat-ui, repo: nat-ui}
  - {name: nat-examples, repo: nat-examples}
# The NAT server registers its routes from its runtime config file, so the
# analyzer cannot read them from source. The operator states them here.
provides:
  - {service: nat, key: "http:POST /chat/stream", source: "nat config.yaml (general.endpoint.path)"}
  - {service: nat, key: "http:POST /chat", source: "nat config.yaml"}
  - {service: nat, key: "ws:/websocket", source: "nat config.yaml (general.endpoint.websocket_path)"}
# The UI proxy dials the host expression UPSTREAM_HTTP_ORIGIN, built from NAT_BACKEND_URL.
host_aliases:
  - {host: UPSTREAM_HTTP_ORIGIN, service: nat, source: "nat-ui proxy/server.js (NAT_BACKEND_URL)"}
EOF

LAIN_INDEX_TIMEOUT="$INDEX_TIMEOUT" "$LAIN_BIN" server --config "$WORK/repos.yaml" \
  --transport http --port "$PORT" > "$WORK/server.log" 2>&1 &
SERVER_PID=$!

# Wait for every repo to leave `indexing`. Liveness alone is not enough.
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

call() {
  curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}

# Parse the list_contracts page: the result carries the page as JSON text,
# and its `items` array is found wherever the envelope nests it. A missing
# array is a failure, not an empty page.
items_of() {
  printf '%s' "$1" | python3 -c '
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
json.dump(items, sys.stdout)
' || { echo "FAIL: could not read list_contracts response" >&2; exit 1; }
}

HTTP=$(items_of "$(call list_contracts '{"snapshot":"live","kind":"http","limit":1000}')")
printf '%s' "$HTTP" | python3 -c '
import json, sys
items = json.load(sys.stdin)
print("http endpoints:", len(items), "bound consumers:", sum(i["bound_consumers"] for i in items))'
UNRES=$(call list_unresolved '{"snapshot":"live","limit":1000}')
printf '%s' "$(items_of "$UNRES")" | python3 -c '
import json, sys
from collections import Counter
items = json.load(sys.stdin)
print("unresolved consumers:", len(items), dict(Counter(i.get("reason") for i in items)))'

fail=0
echo "--- declared providers present ---"
for want in "http:POST /chat/stream" "http:POST /chat"; do
  if printf '%s' "$HTTP" | python3 -c '
import json, sys
want = sys.argv[1]
keys = {i["endpoint"]["key"] for i in json.load(sys.stdin)}
sys.exit(0 if want in keys else 1)' "$want"; then
    echo "OK   $want"
  else
    echo "MISS $want"; fail=1
  fi
done

echo "--- no garbage ANY routes (prose or log lines as paths) ---"
garbage=$(printf '%s' "$HTTP" | python3 -c '
import json, sys
items = json.load(sys.stdin)
# An ANY route is garbage when its path is not a URL path: prose, log
# lines, or template fragments. Next.js pages routes are ANY and valid.
def is_garbage(key):
    template = key.split(" ", 1)[1] if " " in key else ""
    return not template.startswith("/") or " " in template or ":" in template
bad = [i["endpoint"]["key"] for i in items
       if i["endpoint"]["key"].startswith("http:ANY ") and is_garbage(i["endpoint"]["key"])]
print(len(bad))
for k in bad[:10]:
    print("  ", k)')
echo "$garbage" | head -1
if [[ "$(echo "$garbage" | head -1)" != "0" ]]; then
  echo "$garbage" | tail -n +2
  fail=1
fi

echo "--- nat-ui Next.js API route ---"
if printf '%s' "$HTTP" | python3 -c '
import json, sys
keys = {i["endpoint"]["key"] for i in json.load(sys.stdin)}
sys.exit(0 if "http:ANY /api/mcp/clients" in keys else 1)'; then
  echo "OK   http:ANY /api/mcp/clients"
else
  echo "MISS http:ANY /api/mcp/clients"; fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  echo "FAIL: one or more expectations missed" >&2
  exit 1
fi
echo "PASS: declared providers present, no garbage routes, Next.js API route found"
