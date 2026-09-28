#!/usr/bin/env bash
# Diagnostic wrapper for the 2026-09-18 Tauri federation Bug #2:
# the CLI server hangs in `Dl` state after the projection logs and
# the HTTP listener never binds.
#
# Launches `lain server` with `--log-level debug --reindex-timeout 0`
# (per the postmortem's recommended recipe), captures all logs,
# polls /proc for kernel wait-channel info if the process appears
# wedged, and prints a verdict based on which log milestones landed.
#
# Usage:
#   scripts/debug-hung-server.sh <path-to-repos.yaml> [extra lain args...]
#
# Examples:
#   scripts/debug-hung-server.sh ~/lain-test-workspace/repos.yaml
#   scripts/debug-hung-server.sh ./repos.yaml --port 9999
#
# The script does NOT kill the process. When Bug #2 reproduces the
# process is hung but recoverable; press Ctrl-C to stop this wrapper
# (which sends SIGINT to the foreground `lain server`), or `kill -9
# $PID` from another terminal after observing the verdict below.
#
# Environment:
#   LAIN_DEBUG_LOG_DIR   where to write the log (default ./target/lain-debug)
#   LAIN_DEBUG_TIMEOUT   seconds to observe before verdict (default 300)

set -uo pipefail

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage 0
fi
if [ $# -lt 1 ]; then
    echo "usage: $0 <path-to-repos.yaml> [extra lain args...]" >&2
    exit 2
fi

CONFIG="$1"
shift

if ! command -v lain >/dev/null 2>&1; then
    echo "lain not on PATH. Add ~/.lain/bin or run from a build dir." >&2
    exit 2
fi

if [ ! -f "$CONFIG" ]; then
    echo "config not found: $CONFIG" >&2
    exit 2
fi

LOG_DIR="${LAIN_DEBUG_LOG_DIR:-./target/lain-debug}"
mkdir -p "$LOG_DIR"
LOG="$LOG_DIR/hung-server-$(date -u +%Y%m%dT%H%M%SZ).log"
PIDFILE="$LOG_DIR/lain.pid"

echo "==> launching lain server with --log-level debug --reindex-timeout 0"
echo "==> logs: $LOG"
echo "==> ctrl-c to stop; verdict printed when the observation window elapses"
echo

# Launch in background. The script does NOT auto-kill; the operator
# observes and decides.
lain server --config "$CONFIG" --log-level debug --reindex-timeout 0 "$@" \
    >"$LOG" 2>&1 &
LAIN_PID=$!
echo "$LAIN_PID" > "$PIDFILE"

cleanup() {
    rm -f "$PIDFILE"
}
trap cleanup EXIT

# Wait up to LAIN_DEBUG_TIMEOUT for the bug to surface. The postmortem
# timeline showed the hang appears after both per-repo projections
# complete (~3 hours for the Tauri trial); for a smaller repo it
# appears within seconds to minutes. Polling every 2 s is fine.
TIMEOUT="${LAIN_DEBUG_TIMEOUT:-300}"
elapsed=0
process_alive=1
seen_binding=0
seen_listening=0
seen_projection=0
seen_index_timeout=0

while [ "$elapsed" -lt "$TIMEOUT" ]; do
    if ! kill -0 "$LAIN_PID" 2>/dev/null; then
        process_alive=0
        break
    fi

    # Update milestone flags. `grep -q` exits 0/1; we don't want
    # `set -e` to fire on the negative case, so guard with `|| true`.
    grep -q "Binding Lain MCP HTTP listener" "$LOG" 2>/dev/null && seen_binding=1 || true
    grep -q "Lain MCP HTTP server listening" "$LOG" 2>/dev/null && seen_listening=1 || true
    grep -qE '\[federation\] "[^"]+": projected [0-9]+ nodes' "$LOG" 2>/dev/null && seen_projection=1 || true
    grep -q "\[federation\] index timed out after" "$LOG" 2>/dev/null && seen_index_timeout=1 || true

    # Bug #2 signature: at least one projection completed but the
    # bind line never landed. This is the postmortem's exact symptom.
    if [ "$seen_projection" = "1" ] && [ "$seen_binding" = "0" ]; then
        break
    fi

    sleep 2
    elapsed=$((elapsed + 2))
done

echo
echo "==> observation window elapsed (or signature tripped)"
echo

print_verdict() {
    if [ "$process_alive" = "0" ]; then
        echo "==> verdict:"
        echo "    process exited cleanly (exit code preserved in last log line)"
        echo "    Bug #2 did NOT reproduce in this run."
        echo
        echo "    last log lines:"
        tail -5 "$LOG" | sed 's/^/      /'
        return
    fi

    echo "==> verdict: process $LAIN_PID is still alive after ${elapsed}s"
    if [ "$seen_index_timeout" = "1" ]; then
        echo "    - '[federation] index timed out after' was logged"
        echo "      Strong evidence for Hypothesis A: RepoIndex::index's"
        echo "      tokio::time::timeout fired, but the inner future likely"
        echo "      kept running and is holding the git mutex."
    fi
    if [ "$seen_binding" = "0" ]; then
        echo "    - 'Binding Lain MCP HTTP listener' NEVER logged"
        echo "      The startup task is wedged before run_http reaches"
        echo "      TcpListener::bind. /proc/$LAIN_PID/wchan below shows"
        echo "      which syscall each thread is stuck on."
    elif [ "$seen_listening" = "0" ]; then
        echo "    - 'Binding' logged but 'listening' NEVER appeared"
        echo "      TcpListener::bind itself is hung. /proc/$LAIN_PID/wchan"
        echo "      below should point at a network or fd-related syscall."
    fi
}

print_verdict

if [ -d "/proc/$LAIN_PID" ]; then
    echo
    echo "==> /proc/$LAIN_PID/wchan (kernel wait channel of the main thread):"
    wchan=$(cat "/proc/$LAIN_PID/wchan" 2>/dev/null | tr -d ' ' || echo "<unreadable>")
    echo "    ${wchan:-<none>}"
    echo
    echo "==> /proc/$LAIN_PID/status (State + Threads):"
    grep -E "^(State|Threads):" "/proc/$LAIN_PID/status" 2>/dev/null | sed 's/^/    /'
    echo
    echo "==> /proc/$LAIN_PID/stack (first 10 lines; needs kernel.symbols for symbols):"
    head -10 "/proc/$LAIN_PID/stack" 2>/dev/null | sed 's/^/    /' \
        || echo "    <unreadable — kernel.symbols probably not mounted>"
elif command -v ps >/dev/null 2>&1; then
    echo
    echo "==> /proc unavailable; ps STAT column for pid $LAIN_PID:"
    ps -o pid,stat,wchan,comm -p "$LAIN_PID" 2>/dev/null | sed 's/^/    /' \
        || echo "    <process gone or ps unavailable>"
fi

echo
echo "==> next steps:"
echo "    - full log: $LOG"
echo "    - PID: $LAIN_PID  (kill -INT $LAIN_PID for cooperative shutdown,"
echo "      kill -9 if unresponsive — the script traps neither)"
echo "    - If /proc/<pid>/wchan names a libgit2 or inotify syscall,"
echo "      Hypothesis A (non-cancellable inner future holding the git"
echo "      mutex) is the prime suspect. Fix: race every libgit2 / FS"
echo "      call inside index_one_repo against the cancel token, or"
echo "      replace tokio::time::timeout with tokio::select! so the"
echo "      inner future is dropped on cancel."
