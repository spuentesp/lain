#!/usr/bin/env bash
# End-to-end for `lain mcp` workspace resolution under any agent
# harness. The contract: any agent hands `lain mcp` the repo(s) it's
# working on, and `lain mcp` prepares for requests.
#
# Covers:
#   1. Explicit `--workspace PATH` (repeatable).
#   2. `LAIN_WORKSPACE` env var (single path).
#   3. Auto-discover from process cwd (.git ancestor).
#   4. /proc/$PPID/cwd lookup (Kimi's plugin-security cwd-pinning).
#   5. Multi-workspace boots federation on stdio (was previously
#      rejected; now delegated to `run_server --transport stdio`).
#   6. Old flat-arg shape (--workspace auto --transport stdio ...)
#      fails clearly: clap rejects unknown top-level args.
#   7. Explicit --workspace overrides /proc lookup.
#
# Each MCP test spawns `lain mcp` over stdio, sends initialize +
# tools/call, and asserts the relevant field of the response.

set -u

# Source shared helpers (LAIN_MCP_PROTOCOL_VERSION, ok/no/chk). The
# protocol version is queried from the binary — see lib.sh.
source "$(dirname "$0")/lib.sh"

TMP="${TMPDIR:-/tmp}/lain-mcp-resolve-$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

L="${LAIN_BIN:-$(git rev-parse --show-toplevel)/target/debug/lain}"
if [[ ! -x "$L" ]]; then
  echo "build first: cargo build" >&2
  exit 2
fi

# Build minimal git repos for resolution tests.
make_repo() {
  local name="$1"
  mkdir -p "$TMP/$name"
  (cd "$TMP/$name" && git init -q . && git config user.email t@t && git config user.name t \
    && touch "$name.txt" && git add . && git commit -q -m "init $name")
}
make_repo "repo_a"
make_repo "repo_b"

# Plugin-dir is a directory with NO .git — what `lain mcp` would see
# if a Kimi-style plugin harness pinned cwd to a non-repo directory.
mkdir -p "$TMP/plugin_dir"

# Spawn `lain mcp` with cwd=THIS_CWD and capture the get_health
# response. We give the server ~6s to bootstrap before sending
# get_health — the first call triggers the initial index.
mcp_health() {
  local cwd="$1"; shift
  (
    cd "$cwd" || exit 99
    {
      printf '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"'$LAIN_MCP_PROTOCOL_VERSION'","capabilities":{},"clientInfo":{"name":"t","version":"1"}},"id":1}\n'
      sleep 4
      printf '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":2}\n'
      sleep 3
    } | timeout 25 "$L" mcp "$@"
  ) 2>/dev/null
}

# Spawn `lain mcp` mimicking Kimi's plugin-security model: parent cwd
# is the agent's actual workspace (has .git), the forked process
# pins its own cwd to plugin_dir (no .git). From inside `lain mcp`,
# /proc/$PPID/cwd should point to the agent cwd, and the workspace
# resolution should pick up repo_a from there. Additional args are
# forwarded to `lain mcp` (e.g. `--workspace PATH` to override).
kimi_style_health() {
  local agent_cwd="$1"
  local plugin_cwd="$2"
  shift 2
  (
    # Outer subshell: parent cwd = agent_cwd. This is what the
    # forked bash -c will see as /proc/$PPID/cwd.
    cd "$agent_cwd" || exit 99
    {
      printf '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"'$LAIN_MCP_PROTOCOL_VERSION'","capabilities":{},"clientInfo":{"name":"t","version":"1"}},"id":1}\n'
      sleep 4
      printf '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_health","arguments":{}},"id":2}\n'
      sleep 3
    } | bash -c '
        cd "$1"
        shift
        exec "$1" mcp "${@:2}"
      ' bash "$plugin_cwd" "$L" "$@"
  ) 2>/dev/null
}

# --- Tests ----------------------------------------------------------

echo "==> Test 1: explicit --workspace PATH"
out=$(mcp_health "$TMP" --workspace "$TMP/repo_a")
chk "explicit --workspace" "repo_a" "$out"

echo "==> Test 2: LAIN_WORKSPACE env var (single path)"
out=$(LAIN_WORKSPACE="$TMP/repo_b" mcp_health "$TMP")
chk "LAIN_WORKSPACE single" "repo_b" "$out"

echo "==> Test 3: auto-discover from process cwd (existing behavior)"
out=$(mcp_health "$TMP/repo_a")
chk "auto-discover from cwd" "repo_a" "$out"

echo "==> Test 4: /proc/\$PPID/cwd lookup (Kimi cwd-pinning scenario)"
# Parent shell at repo_a (has .git); child pinned to plugin_dir (no .git).
# `lain mcp` inside the child must walk up from the PARENT's cwd.
out=$(kimi_style_health "$TMP/repo_a" "$TMP/plugin_dir")
chk "parent-cwd lookup (Kimi-style)" "repo_a" "$out"

echo "==> Test 5: explicit --workspace overrides /proc lookup"
# Same Kimi model, but flag forces a different workspace.
out=$(kimi_style_health "$TMP/repo_a" "$TMP/plugin_dir" --workspace "$TMP/repo_b")
chk "explicit flag beats /proc" "repo_b" "$out"

echo "==> Test 6: multi --workspace boots the federation on stdio"
# Two workspaces via repeated flag — `lain mcp` must delegate to
# the federation boot path, NOT error with "currently supports one".
# We assert by sending initialize + list_repos and checking the
# response names both repos.
out=$(
  cd "$TMP"
  {
    printf '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"'$LAIN_MCP_PROTOCOL_VERSION'","capabilities":{},"clientInfo":{"name":"t","version":"1"}},"id":1}\n'
    sleep 6
    printf '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_repos","arguments":{}},"id":2}\n'
    sleep 3
  } | timeout 30 "$L" mcp --workspace "$TMP/repo_a" --workspace "$TMP/repo_b"
) 2>&1
chk "multi --workspace boots" "repo_a" "$out"
chk "multi --workspace boots" "repo_b" "$out"

echo "==> Test 7: LAIN_WORKSPACE multi boots the federation"
out=$(
  cd "$TMP"
  {
    printf '{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":"'$LAIN_MCP_PROTOCOL_VERSION'","capabilities":{},"clientInfo":{"name":"t","version":"1"}},"id":1}\n'
    sleep 6
    printf '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_repos","arguments":{}},"id":2}\n'
    sleep 3
  } | LAIN_WORKSPACE="$TMP/repo_a,$TMP/repo_b" timeout 30 "$L" mcp
) 2>&1
chk "LAIN_WORKSPACE multi boots" "repo_a" "$out"
chk "LAIN_WORKSPACE multi boots" "repo_b" "$out"

echo "==> Test 8: old flat-arg shape fails clearly (no silent translation)"
# The pre-fix bug: `lain --workspace auto --transport stdio ...`
# produced "unexpected argument '--workspace'". The new binary must
# still reject it — but with no wrapper script doing silent translation.
err=$("$L" --workspace auto --transport stdio --embedding-model /tmp/m 2>&1 || true)
chk "old flat-arg rejected at top level" "unexpected argument" "$err"

echo
echo "==> summary: $PASS pass, $FAIL fail"
exit "$FAIL"
