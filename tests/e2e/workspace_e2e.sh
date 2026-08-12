#!/usr/bin/env bash
# E2E test for workspace-aware federation mode. Builds a tempdir with
# repos.yaml + workspaces.yaml, starts `lain server --workspace <name>`,
# exercises the 4 workspace-aware MCP tools (list_workspaces,
# get_active_workspace, get_workspace, get_workspace_graph), repeats
# with a different workspace to verify workspace switching.
#
# Requires: curl, python3 (for JSON parsing — no jq), a built `lain`
# binary. Gated: runs on nightly or manual, not per-PR. Network is NOT
# required (the test uses empty fixtures that don't trigger LSP
# hydration — `RepoIndex::index` finds no files and returns quickly).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${REPO_ROOT}/target/release/lain"
PORT="${LAIN_E2E_PORT:-19997}"

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

# Build 3 fake repo dirs. Empty source files are enough to make
# `git init` + a non-empty commit succeed; we don't index any actual code
# (RepoIndex::index finds no files and returns successfully). The
# federation loads the empty per-repo graphs — enough to satisfy the
# 4 workspace tool assertions on tool surface + member counts.
for r in alpha beta gamma; do
    mkdir -p "${WORKDIR}/${r}/src"
    echo "pub fn ${r}_fn() {}" > "${WORKDIR}/${r}/src/lib.rs"
    (
        cd "${WORKDIR}/${r}"
        git init --quiet --initial-branch=main .
        git -c user.email=test@example.com -c user.name=WorkspaceTest add -A
        git -c user.email=test@example.com -c user.name=WorkspaceTest commit --quiet -m initial
    )
done

# repos.yaml: 3 repos.
cat > "${WORKDIR}/repos.yaml" <<EOF
data_dir: ${WORKDIR}/data
repos:
  - id: alpha
    source: { type: workspace_dir, path: ${WORKDIR}/alpha }
  - id: beta
    source: { type: workspace_dir, path: ${WORKDIR}/beta }
  - id: gamma
    source: { type: workspace_dir, path: ${WORKDIR}/gamma }
EOF

# workspaces.yaml: 2 workspaces.
cat > "${WORKDIR}/workspaces.yaml" <<EOF
workspaces:
  - name: ab
    members: [alpha, beta]
  - name: cg
    members: [gamma]
EOF

call_tool() {
    local name="$1"
    local args="${2:-{}}"
    curl -fsS -X POST "http://localhost:${PORT}/mcp" \
        -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"${name}\",\"arguments\":${args}},\"id\":1}"
}

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

start_server() {
    local ws_arg="$1"
    LAIN_BIN="${BIN}" "${BIN}" server \
        --config "${WORKDIR}/repos.yaml" \
        --workspace "${ws_arg}" \
        --transport http \
        --port "${PORT}" \
        --log-level info \
        > "${WORKDIR}/server-${ws_arg}.log" 2>&1 &
    LAIN_PID=$!
    trap 'kill "${LAIN_PID}" 2>/dev/null || true; rm -rf "${WORKDIR}"' EXIT
    for i in $(seq 1 60); do
        if call_tool "get_federation_health" '{}' >/dev/null 2>&1; then break; fi
        sleep 2
        if ! kill -0 "${LAIN_PID}" 2>/dev/null; then
            echo "ERROR: lain server exited early. Log: ${WORKDIR}/server-${ws_arg}.log" >&2
            exit 1
        fi
    done
}

stop_server() {
    kill "${LAIN_PID}" 2>/dev/null || true
    wait "${LAIN_PID}" 2>/dev/null || true
    LAIN_PID=""
}

echo "==> Starting lain server on port ${PORT} with --workspace ab..."
start_server ab

echo "==> Calling list_workspaces..."
list_text="$(call_tool "list_workspaces" '{}' | mcp_text)"
count="$(printf '%s' "${list_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))')"
if [[ "${count}" -ne 2 ]]; then
    echo "ERROR: list_workspaces returned ${count} workspaces, expected 2." >&2
    echo "    Payload: ${list_text}" >&2
    exit 1
fi
echo "    list_workspaces: ${count} workspaces"

echo "==> Calling get_active_workspace..."
active_text="$(call_tool "get_active_workspace" '{}' | mcp_text)"
active_name="$(printf '%s' "${active_text}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["name"])')"
if [[ "${active_name}" != "ab" ]]; then
    echo "ERROR: get_active_workspace returned ${active_name}, expected ab." >&2
    echo "    Payload: ${active_text}" >&2
    exit 1
fi
member_count="$(printf '%s' "${active_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["members"]))')"
if [[ "${member_count}" -ne 2 ]]; then
    echo "ERROR: active workspace has ${member_count} members, expected 2." >&2
    exit 1
fi
echo "    get_active_workspace: ${active_name} (${member_count} members)"

echo "==> Calling get_workspace for 'cg' (different from active)..."
detail_text="$(call_tool "get_workspace" '{"name":"cg"}' | mcp_text)"
detail_count="$(printf '%s' "${detail_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["members"]))')"
if [[ "${detail_count}" -ne 1 ]]; then
    echo "ERROR: cg workspace has ${detail_count} members, expected 1." >&2
    exit 1
fi
echo "    get_workspace(cg): ${detail_count} member"

echo "==> Calling get_workspace_graph (no filter)..."
graph_text="$(call_tool "get_workspace_graph" '{}' | mcp_text)"
node_count="$(printf '%s' "${graph_text}" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["nodes"]))')"
echo "    workspace graph: ${node_count} nodes (empty per-repo graphs, so 0 expected)"

echo "==> Restarting server with --workspace cg..."
stop_server
start_server cg

echo "==> Calling get_active_workspace after switch..."
active_text="$(call_tool "get_active_workspace" '{}' | mcp_text)"
active_name="$(printf '%s' "${active_text}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["name"])')"
if [[ "${active_name}" != "cg" ]]; then
    echo "ERROR: get_active_workspace returned ${active_name} after switch, expected cg." >&2
    exit 1
fi
echo "    get_active_workspace: ${active_name} (after restart)"

echo "==> E2E PASSED"
trap 'rm -rf "${WORKDIR}"' EXIT
