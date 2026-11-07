#!/usr/bin/env bash
# Smoke: the NeMo Agent Toolkit workspace must show HTTP contracts that
# static analysis cannot see (declared in `provides`), no ANY-method
# garbage routes, and the UI's Next.js API routes.
#
# Usage: scripts/eval/nat-http-contract-smoke.sh <workspace-dir> [port]
# <workspace-dir> must contain the NVIDIA clones:
#   NeMo-Agent-Toolkit/ NeMo-Agent-Toolkit-UI/ NeMo-Agent-Toolkit-Examples/
# Set NAT_REQUIRE_NON_TEST_HTTP_BINDINGS=1 to require a bound caller outside
# tests, examples, and CI. Static source scope does not prove deployment.
# KEEP_WORK=1 retains the captured contract and source-scope JSON for review.
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
# The UI proxy dials the host expression UPSTREAM_HTTP_ORIGIN, built from
# NAT_BACKEND_URL. The const lives in the request handler and the fetch
# sits in a nested closure; the analyzer follows the const across the
# scope and then through the assignments that build the origin, so the
# host resolves to the NAT_BACKEND_URL env var and the env: list of the
# nat service routes it. The alias below stays as the fallback for a
# host the analyzer cannot trace.
host_aliases:
  - {host: UPSTREAM_HTTP_ORIGIN, service: nat,
     source: "nat-ui proxy/server.js (NAT_BACKEND_URL)"}
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
  if [[ "$states" == "ready ready ready" ]]; then
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

# Source scope matters: a test caller is not evidence of a production caller.
# Runtime deployment is not established by any static source classification.
printf '%s' "$HTTP" > "$WORK/http-contracts.json"
python3 - "$PORT" "$WORK/http-contracts.json" "${NAT_REQUIRE_NON_TEST_HTTP_BINDINGS:-0}" <<'PY'
import collections, json, pathlib, sys, urllib.request

port, contracts_path, require_source = sys.argv[1:]
contracts = json.loads(pathlib.Path(contracts_path).read_text())

def scope(path):
    if not path or path.startswith('<'):
        return 'unknown'
    parts = path.replace('\\', '/').lower().split('/')
    if any(p in {'test', 'tests', '__tests__', 'fixtures', 'testdata'} for p in parts) or \
            parts[-1].startswith('test_') or '.test.' in parts[-1] or '.spec.' in parts[-1]:
        return 'test'
    if parts[0] in {'examples', 'example', 'samples'}:
        return 'example'
    if parts[0] in {'ci', '.github'}:
        return 'ci'
    if path.startswith('repos.yaml#provides'):
        return 'declared'
    return 'non_test_source'

provider_scopes = collections.Counter()
consumer_scopes = collections.Counter()
bound_details = []
for contract in contracts:
    provider_kinds = {scope(p['path']) for p in contract['providers']}
    provider_scopes[next(iter(provider_kinds)) if len(provider_kinds) == 1 else 'mixed'] += 1
    if not contract['bound_consumers']:
        continue
    args = {'snapshot': 'live', **contract['endpoint']}
    body = {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',
            'params': {'name': 'get_contract', 'arguments': args}}
    request = urllib.request.Request(f'http://127.0.0.1:{port}/mcp',
        data=json.dumps(body).encode(), headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=30) as response:
        envelope = json.load(response)
    if 'error' in envelope or envelope.get('result', {}).get('isError'):
        sys.exit('FAIL: get_contract could not verify bound-consumer source scope')
    payload = json.loads(envelope['result']['content'][0]['text'])
    bound_details.extend(payload['data']['items'])
    consumers = [c for endpoint in payload['data']['items'] for c in endpoint['consumers']]
    if len(consumers) != contract['bound_consumers']:
        sys.exit('FAIL: bound-consumer detail count differs from its summary')
    consumer_scopes.update(scope(c['site']['path']) for c in consumers)
print('HTTP provider source scopes:', dict(sorted(provider_scopes.items())))
print('HTTP bound-consumer source scopes:', dict(sorted(consumer_scopes.items())))
print('Non-test source HTTP bindings:', consumer_scopes['non_test_source'])
print('Runtime production coverage: not established by this static smoke')
pathlib.Path(contracts_path).with_name('http-source-scope.json').write_text(json.dumps({
    'provider_scopes': dict(provider_scopes), 'consumer_scopes': dict(consumer_scopes),
    'non_test_source_bindings': consumer_scopes['non_test_source'],
    'bound_details': bound_details, 'runtime_coverage': 'not_established',
}, indent=2) + '\n')
if require_source == '1' and not consumer_scopes['non_test_source']:
    sys.exit('FAIL: no non-test source HTTP consumer binding was demonstrated')
PY
UNRES=$(call list_unresolved '{"snapshot":"live","limit":1000}')
printf '%s' "$UNRES" > "$WORK/unresolved-response.json"
printf '%s' "$(items_of "$UNRES")" | python3 -c '
import json, sys
from collections import Counter
items = json.load(sys.stdin)
print("unresolved consumers:", len(items), dict(Counter(i.get("reason") for i in items)))'

fail=0
echo "--- declared providers present ---"
for want in "http:POST /chat/stream" "http:POST /chat" \
            "http:GET /mcp/client/tool/list" "http:GET /mcp/client/tool/list/per_user"; do
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

echo "--- SQL source is not a GraphQL consumer ---"
if printf '%s' "$(items_of "$UNRES")" | python3 -c '
import json, sys
items = json.load(sys.stdin)
bad = [i for i in items if i["reason"] == "graphql_no_op" and
       i["consumer"]["path"].endswith("mysql/mysql_object_store.py")]
sys.exit(1 if bad else 0)'; then
  echo "OK   no MySQL-to-GraphQL false consumers"
else
  echo "FAIL SQL was interpreted as GraphQL"; fail=1
fi

echo "--- client callback is not a WebSocket provider ---"
WS=$(items_of "$(call list_contracts '{"snapshot":"live","kind":"websocket","limit":1000}')")
printf '%s' "$WS" > "$WORK/websocket-contracts.json"
if printf '%s' "$WS" | python3 -c '
import json, sys
keys = {i["endpoint"]["key"] for i in json.load(sys.stdin)}
sys.exit(1 if "websocket:/async" in keys else 0)'; then
  echo "OK   no /async callback provider"
else
  echo "FAIL client callback was interpreted as a WebSocket server"; fail=1
fi

echo "--- protocol candidates and URL-expression precision ---"
if python3 - "$WORK/unresolved-response.json" <<'PY'
import json, pathlib, sys
envelope = json.loads(pathlib.Path(sys.argv[1]).read_text())
items = json.loads(envelope['result']['content'][0]['text'])['data']['items']
ws = [i for i in items if i['url_expr'].startswith('ws:client:')]
assert ws, 'no WebSocket cases exercised'
assert all(c['endpoint']['key'].startswith('websocket:')
           for i in ws for c in i['candidates']), 'WebSocket received another protocol'
helpers = [i for i in items if i['consumer']['path'] in
           {'pages/api/mcp/clients.ts', 'proxy/server.js'}]
assert helpers, 'no helper cases exercised'
assert all(i['template'] != '/' for i in helpers), 'opaque URL became a root route'
init = [i for i in helpers if i['url_expr'] == 'initUrl']
assert len(init) == 1 and init[0]['template'] == '/init', 'URL constructor lost /init'
assert init[0]['target_service'] == 'nat', 'URL constructor lost the configured origin'
print(f'OK   {len(ws)} WebSocket rows preserve protocol; helper expressions preserve uncertainty and /init')
PY
then
  :
else
  echo "FAIL protocol or URL-expression precision regression"; fail=1
fi

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

echo "--- the UI proxy is attributed to the nat service ---"
# The proxy's URL is built from a variable chain the analyzer cannot
# follow, so it does not bind. With the host alias it is at least
# attributed: known service, unknown route.
if printf '%s' "$(items_of "$UNRES")" | python3 -c '
import json, sys
items = json.load(sys.stdin)
proxy = [i for i in items if i["consumer"]["path"].endswith("proxy/server.js")]
ok = any(i.get("reason") == "no_route_in_service" and i.get("target_service") == "nat"
         for i in proxy)
if not ok:
    for i in proxy[:4]:
        print("  ", i.get("reason"), "->", i.get("target_service"), i["consumer"]["path"])
sys.exit(0 if ok else 1)'; then
  echo "OK   proxy/server.js -> no_route_in_service (service nat)"
else
  echo "MISS the proxy is not attributed to the nat service"; fail=1
fi

if [[ "$fail" -ne 0 ]]; then
  echo "FAIL: one or more expectations missed" >&2
  exit 1
fi
echo "PASS: expected routes and attribution verified; source-scope coverage reported separately"
