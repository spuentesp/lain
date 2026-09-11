#!/usr/bin/env bash
# Regression test for the per-PR blast-radius symbol extraction in
# `health.sh`. The bash script's regex is duplicated here so a test
# can exercise it on a fixture diff without spawning a full
# `actions/checkout`+`bash` workflow. When `health.sh`'s regex
# changes, this test must change too — a deliberate breaking
# signal.
#
# Run: bash .github/actions/lain-health-badge/tests/test_extract_added_fns.sh
# Exit 0 on success, 1 on first failure.

set -euo pipefail

EXTRACT_REGEX='(async def|def|function|class|fn) +\K[a-zA-Z_][a-zA-Z0-9_]*'
PREFIX_FILTER='^\+[^+]'

# Mirrors the production extraction in `health.sh` (currently lines
# 266-270). Asserts the FULL pipeline — not just the grep -oP
# stage — so a regression in the trailing sort -u is caught here.
# (Earlier versions of this test only asserted grep -oP output, which
# let a broken "awk '{print $2}'" stage slip past: with `\K` the
# grep output is a single field, so $2 is always empty, so the
# production ADDED_FNS was silently empty.)
extract_added_fns() {
    local patch="$1"
    printf '%s\n' "$patch" \
        | grep -E "$PREFIX_FILTER" \
        | grep -oP "$EXTRACT_REGEX" \
        | sort -u
}

assert_eq() {
    local actual="$1"
    local expected="$2"
    local label="$3"
    if [ "$actual" != "$expected" ]; then
        echo "FAIL: $label"
        echo "  expected: $expected"
        echo "  actual:   $actual"
        exit 1
    fi
    echo "ok: $label"
}

# Each case: input diff, expected symbol output.
assert_eq "$(extract_added_fns '+async def fetch_data():')" "fetch_data" \
    "async def keyword extracts function name (the bug that was fixed: \
     pre-fix extracted the literal word 'def' from position 6 of the diff)"

assert_eq "$(extract_added_fns '+def fetch_data():')" "fetch_data" \
    "plain def keyword still works"

assert_eq "$(extract_added_fns '+function foo():')" "foo" \
    "function keyword extracts name (JS/TS)"

assert_eq "$(extract_added_fns '+class Bar:')" "Bar" \
    "class keyword extracts name"

assert_eq "$(extract_added_fns '+fn baz() {')" "baz" \
    "fn keyword extracts name (Rust)"

assert_eq "$(extract_added_fns '+async def fetch_data(arg: i32) -> u32 {')" "fetch_data" \
    "function with params + return type still extracts name"

# Multi-line diff: only the + lines should be considered.
# Line 1 (' async def fn_a():') has a space prefix instead of +,
# so the prefix filter excludes it. Only fn_b should be extracted.
assert_eq "$(extract_added_fns "$(printf '%s\n' ' async def fn_a():' '+async def fn_b():' '+  body')")" "fn_b" \
    "only added lines contribute to extraction"

# Function inside a class (method). The pre-fix regex matched `def`
# inside the indentation, post-fix the indentation doesn't break
# the match. The `+` prefix means it's an added line; the leading
# spaces come after the prefix and the regex still picks up
# `def method_a`.
assert_eq "$(extract_added_fns "$(printf '%s\n' '+    def method_a(self):' '+        pass')")" "method_a" \
    "indented method is still extracted"

# A line that is just whitespace after + (e.g. +   \n) doesn't
# contain a keyword, so no extraction. The \+ prefix is stripped
# by the regex; the result is blank for whitespace-only added
# lines.
empty=$(extract_added_fns '+   ' || true)
if [ -n "$empty" ]; then
    echo "FAIL: whitespace-only line should not extract anything; got: $empty"
    exit 1
fi
echo "ok: whitespace-only added line produces no symbol"

# A line that is +++ (file header from unified diff) is filtered
# out by the +[^+] prefix regex.
assert_eq "$(extract_added_fns '+++ b/src/lib.rs')" "" \
    "+++ file header is filtered out"

# No + prefix: not an added line, not extracted.
assert_eq "$(extract_added_fns ' async def never_added():')" "" \
    "non-+ line is filtered out"

echo
echo "All extract_added_fns regression tests passed."
