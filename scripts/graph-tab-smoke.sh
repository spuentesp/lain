#!/usr/bin/env bash
# Manual smoke test for the Command Center Graph tab (defect D-M8).
#
# The repo has no headless-browser harness, so this asserts everything the
# browser needs, one layer below the browser:
#   1. the server serves index.html containing the graph tab shell
#   2. the server serves app.js containing renderGraphTab
#   3. the vendored D3 asset is reachable (no CDN)
#   4. get_workspace_graph returns a {nodes, edges} payload
#
# Usage: scripts/graph-tab-smoke.sh [port]
set -euo pipefail

PORT="${1:-9878}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"; if [ -n "${SRV_PID:-}" ]; then kill "$SRV_PID" 2>/dev/null || true; fi' EXIT

echo "==> building fixture"
"$ROOT/scripts/demo-fixture.sh" "$TMP/demo"

# `lain server --config` reads `data_dir` + `repos[].source: workspace_dir`.
# (The brief showed `path:` and a sidecar `workspaces.yaml`; --workspace
# alone is enough for this smoke, so we keep the proven single-repo shape.)
cat > "$TMP/repos.yaml" <<EOF
data_dir: $TMP/data
repos:
  - id: demo
    source: { type: workspace_dir, path: $TMP/demo }
EOF

echo "==> building lain"
cargo build --manifest-path "$ROOT/Cargo.toml" 2>&1 | tail -3

echo "==> starting server on :$PORT"
"$ROOT/target/debug/lain" server \
  --config "$TMP/repos.yaml" \
  --transport http \
  --port "$PORT" >"$TMP/server.log" 2>&1 &
SRV_PID=$!

for _ in $(seq 1 40); do
  curl -sf "http://127.0.0.1:$PORT/index.html" >/dev/null && break
  sleep 0.5
done

fail() { echo "FAIL: $1"; echo "--- server log ---"; cat "$TMP/server.log"; exit 1; }

echo "==> 1/4 index.html carries the graph tab shell"
HTML="$(curl -sf "http://127.0.0.1:$PORT/index.html")" || fail "index.html not served"
grep -q 'id="graph-workspace"' <<<"$HTML" || fail "no #graph-workspace select in index.html"
grep -q 'id="graph-canvas"'    <<<"$HTML" || fail "no #graph-canvas svg in index.html"
grep -q 'id="graph-empty"'     <<<"$HTML" || fail "no #graph-empty div in index.html"

echo "==> 2/4 app.js defines the render functions"
JS="$(curl -sf "http://127.0.0.1:$PORT/app.js")" || fail "app.js not served"
grep -q 'function renderGraphTab'      <<<"$JS" || fail "renderGraphTab missing"
grep -q 'function renderGraphTabEmpty' <<<"$JS" || fail "renderGraphTabEmpty missing"
grep -q 'function pickWorkspaceForGraph' <<<"$JS" || fail "pickWorkspaceForGraph missing"

echo "==> 3/4 vendored D3 is reachable (no CDN)"
curl -sf "http://127.0.0.1:$PORT/assets/d3.v7.min.js" >/dev/null || fail "d3 asset 404"
# `if` rather than `&&` — under `set -e` a trailing failing `&&` list aborts
# the script, and grep *failing* is the good case here.
if grep -q 'd3js.org' <<<"$HTML"; then fail "index.html reaches for a CDN copy of D3"; fi

echo "==> 4/4 get_workspace_graph returns nodes"
RESP="$(curl -sf -X POST "http://127.0.0.1:$PORT/mcp" \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_workspace_graph","arguments":{}},"id":1}')" \
  || fail "get_workspace_graph call failed"
grep -q '"nodes"' <<<"$RESP" || fail "no nodes key in response: $RESP"
grep -q '"edges"' <<<"$RESP" || fail "no edges key in response: $RESP"

echo "PASS: graph tab smoke test"
