#!/usr/bin/env bash
# Comprehensive end-to-end harness for the intent + observability
# layer (docs/INTENT_AND_OBSERVABILITY_PLAN.md, PRs 1–6).
#
# Spawns a real `lain server --transport http`, drives ~50 scenarios
# (intent lifecycle, activity observation, evaluation engine,
# cross-agent, error paths, doc accuracy, persistence), then writes
# a per-scenario PASS/FAIL summary to `report.txt`.
#
# Usage:
#   scripts/e2e_full.sh                       # uses target/debug/lain
#   LAIN_BIN=/path/to/lain scripts/e2e_full.sh # explicit binary
#   OUT_DIR=/tmp/foo scripts/e2e_full.sh      # custom output

set -euo pipefail

# Resolve binary.
LAIN_BIN="${LAIN_BIN:-}"
if [ -z "$LAIN_BIN" ]; then
    for sub in target/release/lain target/debug/lain; do
        if [ -x "$sub" ]; then
            LAIN_BIN="$(pwd)/$sub"
            break
        fi
    done
fi
if [ -z "$LAIN_BIN" ] || [ ! -x "$LAIN_BIN" ]; then
    echo "no lain binary found; set LAIN_BIN or run \`cargo build\` first" >&2
    exit 1
fi

# Output dir.
OUT_DIR="${OUT_DIR:-$(mktemp -d -t e2e_full.XXXXXX)}"
mkdir -p "$OUT_DIR"
REPORT="$OUT_DIR/report.txt"
echo "==> output dir: $OUT_DIR"

# Workspace + state dir.
WORKSPACE="$(mktemp -d -t e2e_full_ws.XXXXXX)"
STATE_DIR="$(mktemp -d -t e2e_full_state.XXXXXX)"
git -C "$WORKSPACE" init -q
git -C "$WORKSPACE" config user.email "e2e@lain"
git -C "$WORKSPACE" config user.name "e2e-full"
mkdir -p "$WORKSPACE/src" "$WORKSPACE/docs"
printf 'pub fn contested() {}\n' >"$WORKSPACE/src/contested.rs"
printf 'pub fn release() {}\n' >"$WORKSPACE/src/release.rs"
printf 'pub fn auth() {}\n' >"$WORKSPACE/src/auth.rs"
printf 'pub fn session() {}\n' >"$WORKSPACE/src/session.rs"
printf 'pub fn token() {}\n' >"$WORKSPACE/src/token.rs"
printf 'pub fn a() {}\n' >"$WORKSPACE/src/a.rs"
printf 'pub fn b() {}\n' >"$WORKSPACE/src/b.rs"
printf 'pub fn read_only_target() {}\n' >"$WORKSPACE/src/read_only_target.rs"
printf '# readme\n' >"$WORKSPACE/docs/readme.md"
git -C "$WORKSPACE" add -A
git -C "$WORKSPACE" commit -q -m "fixture"
echo "==> workspace:  $WORKSPACE"
echo "==> state dir:  $STATE_DIR"

# Free port.
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("",0)); print(s.getsockname()[1]); s.close()')"
LAIN_URL="http://localhost:${PORT}"

# Generate a repos.yaml for `lain server`.
REPOS_YAML="$OUT_DIR/repos.yaml"
cat >"$REPOS_YAML" <<EOF
data_dir: $STATE_DIR/federation
max_concurrent_indexers: 1
ready_threshold: 0.5
repos:
  - id: fixture
    source:
      type: workspace_dir
      path: $WORKSPACE
EOF

# Spawn the server, with `setsid` so it survives the wrapper's
# process group; we kill it explicitly at the end.
LAIN_STDERR="$OUT_DIR/server.stderr"
LAIN_STDOUT="$OUT_DIR/server.stdout"
spawn_server() {
    XDG_STATE_HOME="$STATE_DIR" \
    LAIN_REINDEX_TIMEOUT=30 \
        "$LAIN_BIN" server --config "$REPOS_YAML" --transport http --port "$PORT" \
            >"$LAIN_STDOUT" 2>"$LAIN_STDERR" &
    echo "$!" >"$OUT_DIR/server.pid"
}

kill_server() {
    if [ -f "$OUT_DIR/server.pid" ]; then
        local pid
        pid="$(cat "$OUT_DIR/server.pid")"
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            for _ in $(seq 1 30); do
                kill -0 "$pid" 2>/dev/null || break
                sleep 0.1
            done
            kill -9 "$pid" 2>/dev/null || true
        fi
    fi
}

cleanup() {
    kill_server
}
trap cleanup EXIT

# Initial spawn.
spawn_server
SERVER_PID="$(cat "$OUT_DIR/server.pid")"
for _ in $(seq 1 600); do
    if curl -sf -o /dev/null "$LAIN_URL/health"; then
        break
    fi
    sleep 0.1
done
if ! curl -sf -o /dev/null "$LAIN_URL/health"; then
    echo "server failed to bind on $LAIN_URL" >&2
    tail -10 "$LAIN_STDERR" >&2 || true
    exit 1
fi
echo "==> server up: $LAIN_URL"

# Restart function exported via env so the Python harness can
# invoke it without an out-of-band channel.
export LAIN_URL
export OUT_DIR STATE_DIR WORKSPACE

# Define a `restart_server` bash function we expose to Python via
# an env-sentinel + a small script wrapper. Easiest path: write
# a tiny shell helper to $OUT_DIR that Python can call via
# subprocess for the restart.
cat >"$OUT_DIR/restart.sh" <<'EOSH'
#!/usr/bin/env bash
set -e
OUT_DIR="$1"
STATE_DIR="$2"
WORKSPACE="$3"
REPOS_YAML="$OUT_DIR/repos.yaml"
LAIN_BIN="${LAIN_BIN:-}"
if [ -z "$LAIN_BIN" ]; then
    for sub in target/release/lain target/debug/lain; do
        if [ -x "$sub" ]; then LAIN_BIN="$(pwd)/$sub"; break; fi
    done
fi
LAIN_URL="$4"
PORT="${LAIN_URL##*:}"
LAIN_STDERR="$OUT_DIR/server.stderr"
LAIN_STDOUT="$OUT_DIR/server.stdout"
# Kill old.
if [ -f "$OUT_DIR/server.pid" ]; then
    pid="$(cat "$OUT_DIR/server.pid")"
    if kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        for _ in $(seq 1 30); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -9 "$pid" 2>/dev/null || true
    fi
fi
# Spawn new — same state dir, same repos.yaml, same port.
env XDG_STATE_HOME="$STATE_DIR" LAIN_REINDEX_TIMEOUT=30 \
    "$LAIN_BIN" server --config "$REPOS_YAML" --transport http --port "$PORT" \
    >"$LAIN_STDOUT" 2>"$LAIN_STDERR" &
echo "$!" >"$OUT_DIR/server.pid"
# Wait for health. Sidecar child process takes longer on cold
# start; allow up to 60 s before declaring the restart failed.
for _ in $(seq 1 600); do
    if curl -sf -o /dev/null "$LAIN_URL/health"; then
        exit 0
    fi
    sleep 0.1
done
echo "restart: server failed to come up" >&2
exit 1
EOSH
chmod +x "$OUT_DIR/restart.sh"

# Run the Python harness. The harness is in `scripts/`.
HARNESS="$OUT_DIR/harness.py"
# Inline-copy so the script doesn't depend on scripts/ layout
# at run time (the user may relocate OUT_DIR).
cp /data/agents/orca/lain/scripts/e2e_full.py "$HARNESS"

# Patch the harness to call our restart shell script. The
# restart script is invoked with $LAIN_BIN exported so it
# doesn't have to fall back to a pwd-relative search (the
# harness below runs after a `cd`, so pwd may not be the repo
# root by the time restart.sh is called).
python3 -c "
import re
with open('$HARNESS') as f: src = f.read()
patched = src.replace(
    'def _noop_restart() -> LainClient:\n        return LainClient(os.environ.get(\"LAIN_URL\", \"http://localhost:9999\"))',
    'def _noop_restart() -> LainClient:\n        import subprocess\n        subprocess.run([\"$OUT_DIR/restart.sh\", \"$OUT_DIR\", \"$STATE_DIR\", \"$WORKSPACE\", \"$LAIN_URL\"], check=True, env={**os.environ, \"LAIN_BIN\": os.environ[\"LAIN_BIN\"]})\n        return LainClient(os.environ.get(\"LAIN_URL\", \"$LAIN_URL\"))',
)
with open('$HARNESS', 'w') as f: f.write(patched)
"

cd "$(dirname "$OUT_DIR")/.."
export LAIN_BIN
python3 "$HARNESS" 2>&1 | tee "$REPORT"
PY_EXIT=${PIPESTATUS[0]}

echo "==> report: $REPORT"
exit "$PY_EXIT"
