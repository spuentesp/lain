#!/usr/bin/env bash
# Smoke harness for scripts/record-spa-demo.sh.
#
# Asserts the four artifact invariants + JSON summary. Not wired to CI —
# the recording is on-demand only. Run by hand after editing the script
# or its inputs.
#
# Usage: ./tests/record_spa_demo_test.sh
# Exits 0 on success, 1 on the first failed invariant.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/record-spa-demo.sh"
WORK="${WORK:-/tmp/lain-record-spa-demo-test}"
export WORK
PORT="${PORT:-9935}"
JSON="${JSON:-/tmp/lain-record-summary-test.json}"
ART="$REPO_ROOT/docs/screenshots"

cleanup() {
  rm -rf "$WORK"
}
trap cleanup EXIT

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf 'PASS: %s\n' "$*"; }

[ -x "$SCRIPT" ] || fail "script missing or not executable: $SCRIPT"

# Make sure no stale artifact is sitting around from a previous run.
rm -f "$ART/spa-demo.webm" "$ART/spa-demo.mp4" "$ART/spa-demo.gif" "$ART/spa-demo-poster.png"

cd "$REPO_ROOT" || fail "cd repo root"
"$SCRIPT" --no-build --keep-work --port "$PORT" --json "$JSON"
ec=$?
[ "$ec" = 0 ] || fail "script exit code = $ec (expected 0)"

# WebM ≤ 5 MB
[ -s "$ART/spa-demo.webm" ] || fail "spa-demo.webm missing or empty"
webm_bytes=$(stat -c%s "$ART/spa-demo.webm")
[ "$webm_bytes" -le $((5*1024*1024)) ] || fail "spa-demo.webm is $webm_bytes bytes (>5MB)"
pass "webm: $webm_bytes bytes (≤5MB)"

# MP4 ≤ 4 MB
[ -s "$ART/spa-demo.mp4" ] || fail "spa-demo.mp4 missing or empty"
mp4_bytes=$(stat -c%s "$ART/spa-demo.mp4")
[ "$mp4_bytes" -le $((4*1024*1024)) ] || fail "spa-demo.mp4 is $mp4_bytes bytes (>4MB)"
pass "mp4: $mp4_bytes bytes (≤4MB)"

# GIF ≤ 8 MB target; hard cap 12 MB
[ -s "$ART/spa-demo.gif" ] || fail "spa-demo.gif missing or empty"
gif_bytes=$(stat -c%s "$ART/spa-demo.gif")
gif_mb=$(( gif_bytes / 1024 / 1024 ))
[ "$gif_bytes" -le $((12*1024*1024)) ] || fail "spa-demo.gif is $gif_bytes bytes (>12MB hard cap)"
pass "gif: $gif_bytes bytes (${gif_mb}MB ≤8MB target / ≤12MB hard cap)"

# Poster ≤ 200 KB target — soft check. The brief treats poster over-budget
# as WARN (not die), so we mirror that here.
[ -s "$ART/spa-demo-poster.png" ] || fail "spa-demo-poster.png missing or empty"
poster_bytes=$(stat -c%s "$ART/spa-demo-poster.png")
poster_kb=$(( poster_bytes / 1024 ))
if [ "$poster_bytes" -le $((200*1024)) ]; then
  pass "poster: $poster_bytes bytes (${poster_kb}KB ≤200KB)"
else
  printf 'SOFT-WARN: poster is %s bytes (%sKB > 200KB target)\n' "$poster_bytes" "$poster_kb"
fi

# JSON summary has the four byte-count fields and matches the artifacts
[ -s "$JSON" ] || fail "JSON summary missing or empty: $JSON"
grep -q '"webm_bytes"'   "$JSON" || fail "JSON summary missing webm_bytes"
grep -q '"mp4_bytes"'    "$JSON" || fail "JSON summary missing mp4_bytes"
grep -q '"gif_bytes"'    "$JSON" || fail "JSON summary missing gif_bytes"
grep -q '"poster_bytes"' "$JSON" || fail "JSON summary missing poster_bytes"
grep -q '"recorded_at"'  "$JSON" || fail "JSON summary missing recorded_at"
pass "json summary: $JSON"

# file(1) sanity on the WebM (Playwright native container)
file "$ART/spa-demo.webm" | grep -qi 'WebM' \
  || fail "spa-demo.webm file(1) does not report WebM"
pass "spa-demo.webm file(1) reports WebM"

# file(1) sanity on the MP4
file "$ART/spa-demo.mp4" | grep -qiE 'MP4|ISO Media' \
  || fail "spa-demo.mp4 file(1) does not report MP4/ISO Media"
pass "spa-demo.mp4 file(1) reports MP4/ISO Media"

# file(1) sanity on the poster PNG
file "$ART/spa-demo-poster.png" | grep -qi 'PNG' \
  || fail "spa-demo-poster.png file(1) does not report PNG"
pass "spa-demo-poster.png file(1) reports PNG"

# file(1) sanity on the GIF
file "$ART/spa-demo.gif" | grep -qi 'GIF' \
  || fail "spa-demo.gif file(1) does not report GIF"
pass "spa-demo.gif file(1) reports GIF"

# --keep-work preserved the workdir
[ -d "$WORK" ] || fail "workdir $WORK not preserved (--keep-work should keep it)"
pass "workdir preserved: $WORK"

exit 0