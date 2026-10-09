#!/usr/bin/env bash
# Run every TLA+ spec in docs/formal/MANIFEST and check the outcome matches
# its `expect` column. Local-only for now (not wired into CI).
#
#   scripts/check-formal.sh            # all specs
#   scripts/check-formal.sh Readiness  # only manifest rows containing "Readiness"
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FORMAL="$ROOT/docs/formal"
TLC="$ROOT/tools/tla/tla2tools.jar"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
filter="${1:-}"
bad=0 ran=0
while IFS='|' read -r cfg module expect _; do
  cfg="$(echo "$cfg" | xargs)"; module="$(echo "$module" | xargs)"; expect="$(echo "$expect" | xargs)"
  [[ -z "$cfg" || "$cfg" == \#* ]] && continue
  [[ -n "$filter" && "$cfg" != *"$filter"* ]] && continue
  ran=$((ran + 1))
  ( cd "$WORK" && java -jar "$TLC" -deadlock -metadir "$WORK/st_$cfg" \
      -config "$FORMAL/$cfg.cfg" "$FORMAL/$module.tla" >"$WORK/$cfg.out" 2>&1 ); rc=$?
  case "$expect:$rc" in
    pass:0) printf 'ok    %-34s (pass)\n' "$cfg" ;;
    fail:12) printf 'ok    %-34s (counterexample, as documented)\n' "$cfg" ;;
    *) printf 'FAIL  %-34s expected %s, TLC exit %s\n' "$cfg" "$expect" "$rc"; tail -n 8 "$WORK/$cfg.out"; bad=$((bad + 1)) ;;
  esac
done < "$FORMAL/MANIFEST"
echo "$ran specs checked, $bad unexpected"
[[ $bad -eq 0 ]]
