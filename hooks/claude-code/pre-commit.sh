#!/usr/bin/env bash
# Claude Code pre-commit hook for lain — calls `lain hooks overlap-check`
# against the previous commit and refuses the commit if high-severity
# conflicts are found.
#
# Always exits 0 on infrastructure failure — never block Claude Code.
# Exits 2 ONLY on confirmed conflict detection (which becomes exit 78 in
# git's pre-commit context to signal "skip commit"; Claude Code will then
# surface the message).

set +e
trap 'exit 0' ERR

# Resolve LAIN_URL — default to localhost:9999.
LAIN_URL="${LAIN_URL:-http://localhost:9999}"
HOOK_PREV_COMMIT="${HOOK_PREV_COMMIT:-HEAD~1}"

if ! command -v lain >/dev/null 2>&1; then
    exit 0
fi

# Run the overlap check.
RESULT=$(lain hooks overlap-check \
    --url "$LAIN_URL" \
    --base "$HOOK_PREV_COMMIT" \
    --head HEAD \
    --workspace "${LAIN_WORKSPACE:-backend}" 2>&1)

if [ $? -ne 0 ]; then
    # Infrastructure failure — pass through.
    exit 0
fi

# Parse the JSON; if total_overlaps > 0, refuse.
OVERLAPS=$(echo "$RESULT" | python3 -c "
import json, sys
try:
    d = json.loads(sys.stdin.read())
    print(d.get('total_overlaps', 0))
except Exception:
    print(0)
" 2>/dev/null || echo 0)

if [ "$OVERLAPS" -gt 0 ]; then
    echo "lain pre-commit: $OVERLAPS symbol overlap(s) detected with $HOOK_PREV_COMMIT — refusing commit" >&2
    echo "$RESULT" | python3 -m json.tool 1>&2
    # Git pre-commit hooks use exit code 1 to abort.
    exit 1
fi

exit 0
