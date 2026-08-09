#!/usr/bin/env bash
# E2E test for the Federated Indexer. Starts `lain server` against 3 public
# repos, waits for them to become Ready, then exercises the federation MCP
# tools via HTTP. Requires: curl, python3 (for JSON parsing — the brief's
# script uses jq, but jq is not installed in this environment and the brief
# explicitly allows "use a different JSON parser"), network access.
#
# Marked optional/slow: clones 3 public GitHub repos and waits for indexing,
# so it is NOT part of the regular `cargo test` / CI matrix. Run on demand
# with `tests/e2e/federation_e2e.sh` from the repo root after
# `cargo build --release`.
set -euo pipefail

# Resolve repo root regardless of where the script is invoked from.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${REPO_ROOT}/target/release/lain"
PORT="${LAIN_E2E_PORT:-19999}"

if [[ ! -x "${BIN}" ]]; then
    echo "ERROR: ${BIN} not found. Run \`cargo build --release\` first." >&2
    exit 2
fi

for tool in curl python3; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        echo "ERROR: required tool '${tool}' not on PATH." >&2
        exit 2
    fi
done

WORKDIR="$(mktemp -d)"
trap 'kill "${LAIN_PID:-}" 2>/dev/null || true; rm -rf "${WORKDIR}"' EXIT

cat > "${WORKDIR}/repos.yaml" <<EOF
data_dir: ${WORKDIR}/data
repos:
  - id: hello-rust
    source: { type: shallow_clone, url: "https://github.com/rayon-rs/rayon.git", ref: main, refresh_interval_secs: 3600 }
  - id: ripgrep
    source: { type: shallow_clone, url: "https://github.com/BurntSushi/ripgrep.git", ref: master, refresh_interval_secs: 3600 }
  - id: serde
    source: { type: shallow_clone, url: "https://github.com/serde-rs/serde.git", ref: master, refresh_interval_secs: 3600 }
EOF

echo "==> Starting lain server on port ${PORT}..."
"${BIN}" server \
    --config "${WORKDIR}/repos.yaml" \
    --transport http \
    --port "${PORT}" \
    --log-level info \
    > "${WORKDIR}/server.log" 2>&1 &
LAIN_PID=$!
trap 'kill "${LAIN_PID}" 2>/dev/null || true; rm -rf "${WORKDIR}"' EXIT

call_tool() {
    local name="$1"
    local args="${2:-{\}}"
    curl -fsS -X POST "http://localhost:${PORT}/mcp" \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"${name}\",\"arguments\":${args}},\"id\":1}"
}

# Wait for the server to respond on /mcp. We poll list_repos because it
# returns 200 once the HTTP listener is up regardless of indexing state;
# the tool-call assertions below handle readiness separately.
echo "==> Waiting for server to be reachable on /mcp..."
for i in $(seq 1 60); do
    if call_tool "get_federation_health" '{}' >/dev/null 2>&1; then
        break
    fi
    sleep 2
    if ! kill -0 "${LAIN_PID}" 2>/dev/null; then
        echo "ERROR: lain server exited early. Log:" >&2
        cat "${WORKDIR}/server.log" >&2
        exit 1
    fi
done

# Extract the text payload (server returns MCP-shaped JSON: result.content[0].text
# holds the tool's serialized result; the tool itself returns a JSON array/object
# inside that text block).
mcp_text() {
    python3 -c '
import json, sys
data = json.load(sys.stdin)
content = data.get("result", {}).get("content", [])
if not content:
    sys.exit("no content block")
print(content[0]["text"])
'
}

echo "==> Calling list_repos..."
list_text="$(call_tool "list_repos" '{}' | mcp_text)"
echo "    list_repos payload (truncated): $(printf '%s' "${list_text}" | head -c 200)..."
list_count="$(printf '%s' "${list_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
if [[ "${list_count}" != "3" ]]; then
    echo "ERROR: list_repos returned ${list_count} repos, expected 3." >&2
    echo "    Payload: ${list_text}" >&2
    exit 1
fi
echo "    list_repos: ${list_count} repos"

echo "==> Calling get_federation_health..."
health_text="$(call_tool "get_federation_health" '{}' | mcp_text)"
total_repos="$(printf '%s' "${health_text}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["total_repos"])')"
if [[ "${total_repos}" != "3" ]]; then
    echo "ERROR: get_federation_health.total_repos = ${total_repos}, expected 3." >&2
    echo "    Payload: ${health_text}" >&2
    exit 1
fi
echo "    get_federation_health.total_repos = ${total_repos}"

echo "==> Calling search_org for 'serialize'..."
hits_text="$(call_tool "search_org" '{"query":"serialize","limit":5}' | mcp_text)"
hits_count="$(printf '%s' "${hits_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
if [[ "${hits_count}" -lt 1 ]]; then
    echo "ERROR: search_org returned 0 hits for 'serialize', expected >= 1." >&2
    echo "    Payload: ${hits_text}" >&2
    exit 1
fi
echo "    search_org 'serialize': ${hits_count} hits"

echo "==> E2E PASSED"
trap 'rm -rf "${WORKDIR}"' EXIT
