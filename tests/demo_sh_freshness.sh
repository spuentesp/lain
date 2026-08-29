#!/usr/bin/env bash
# Tests for scripts/demo-freshness.sh (D-L3).
#
# Sources the helper and exercises each function with controlled
# inputs (a stub binary in a temp dir, a fake repo_root with known
# mtimes via `touch -d`). Asserts on stdout, stderr, and return
# codes — never invokes demo.sh itself, so no server is booted.
#
# Convention (mirrors tests/install-test.sh and tests/e2e_full.sh):
# - PASS / FAIL counters.
# - Each test name prints on success or failure.
# - Final summary + non-zero exit on any FAIL.
#
# Usage:  bash tests/demo_sh_freshness.sh
# Exit:   0 if all pass, 1 otherwise.

set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$HERE/.." && pwd)"
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/demo-freshness.sh"

PASS=0
FAIL=0

pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; PASS=$((PASS + 1)); }
fail() { printf '  \033[31mFAIL\033[0m %s\n     got: %s\n' "$1" "$2"; FAIL=$((FAIL + 1)); }

# ── fixtures ─────────────────────────────────────────────────────────
WORK=$(mktemp -d -t lain-freshness-XXXXXX)
trap 'rm -rf "$WORK"' EXIT

STUB="$WORK/stub-lain"
cat > "$STUB" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  --version) echo "stub lain 9.9.9" ;;
  *) echo "stub invoked with $*"; exit 0 ;;
esac
EOF
chmod +x "$STUB"

# Fake repo_root: Cargo.toml, Cargo.lock, src/main.rs. All mtimes
# controlled via touch -d so the freshness comparison is deterministic.
FAKE_REPO="$WORK/repo"
mkdir -p "$FAKE_REPO/src"
echo '[package]' > "$FAKE_REPO/Cargo.toml"
echo 'version = "0.1.0"' >> "$FAKE_REPO/Cargo.toml"
echo '# lock' > "$FAKE_REPO/Cargo.lock"
echo 'fn main() {}' > "$FAKE_REPO/src/main.rs"

# Two epoch timestamps: B (binary build time) and S (source mtime).
B=$(date -d '2026-01-01 00:00:00 UTC' +%s)
S_OLD=$(date -d '2025-06-01 00:00:00 UTC' +%s)
S_NEW=$(date -d '2026-06-01 00:00:00 UTC' +%s)

# ── 1. mtime_of returns an integer for an existing file ──────────────
got=$(mtime_of "$STUB")
case "$got" in
  ''|*[!0-9]*) fail "mtime_of returns integer" "non-integer: '$got'" ;;
  *)           pass "mtime_of returns integer" ;;
esac

# ── 2. mtime_of returns 0 for a missing file ─────────────────────────
got=$(mtime_of "$WORK/does-not-exist")
[ "$got" = "0" ] \
  && pass "mtime_of returns 0 on missing path" \
  || fail "mtime_of returns 0 on missing path" "got '$got'"

# ── 3. print_binary_info includes version and mtime lines ────────────
out=$(print_binary_info "$STUB")
case "$out" in
  *"binary:  $STUB"*) : ;;
  *)                   fail "print_binary_info has binary line" "got: $out" ; continue_=1 ;;
esac
if [ "${continue_:-0}" != 1 ]; then
  case "$out" in
    *"version: stub lain 9.9.9"*) pass "print_binary_info has version line" ;;
    *)                            fail "print_binary_info has version line" "got: $out" ;;
  esac
fi
case "$out" in
  *"mtime:  "*) pass "print_binary_info has mtime line" ;;
  *)            fail "print_binary_info has mtime line" "got: $out" ;;
esac

# ── 4. newest_source_mtime picks the newest source file ──────────────
touch -d "@$S_OLD" "$FAKE_REPO/Cargo.toml"
touch -d "@$S_OLD" "$FAKE_REPO/Cargo.lock"
touch -d "@$S_NEW" "$FAKE_REPO/src/main.rs"
got=$(newest_source_mtime "$FAKE_REPO")
[ "$got" = "$S_NEW" ] \
  && pass "newest_source_mtime picks newest source file" \
  || fail "newest_source_mtime picks newest source file" "expected $S_NEW, got $got"

# ── 5. check_binary_freshness returns 0 when source is older ─────────
touch -d "@$B"  "$STUB"
touch -d "@$S_OLD" "$FAKE_REPO/Cargo.toml"
touch -d "@$S_OLD" "$FAKE_REPO/Cargo.lock"
touch -d "@$S_OLD" "$FAKE_REPO/src/main.rs"
err=$(check_binary_freshness "$STUB" "$FAKE_REPO" 2>&1)
rc=$?
if [ "$rc" = 0 ] && [ -z "$err" ]; then
  pass "freshness: returns 0 with no warning when source is older"
else
  fail "freshness: returns 0 with no warning when source is older" "rc=$rc stderr='$err'"
fi

# ── 6. check_binary_freshness returns 1 + warning when stale ─────────
touch -d "@$B"     "$STUB"
touch -d "@$S_NEW" "$FAKE_REPO/src/main.rs"
err=$(check_binary_freshness "$STUB" "$FAKE_REPO" 2>&1)
rc=$?
if [ "$rc" = 1 ] && echo "$err" | grep -q "binary may be stale"; then
  pass "freshness: returns 1 and warns when source is newer"
else
  fail "freshness: returns 1 and warns when source is newer" "rc=$rc stderr='$err'"
fi

# ── 7. check_binary_freshness honors ALLOW_STALE=1 ───────────────────
err=$(ALLOW_STALE=1 check_binary_freshness "$STUB" "$FAKE_REPO" 2>&1)
rc=$?
if [ "$rc" = 0 ] && [ -z "$err" ]; then
  pass "freshness: ALLOW_STALE=1 suppresses warning on stale binary"
else
  fail "freshness: ALLOW_STALE=1 suppresses warning on stale binary" "rc=$rc stderr='$err'"
fi

# ── 8. newest_source_mtime returns 0 when repo has no sources ────────
EMPTY_REPO="$WORK/empty"; mkdir -p "$EMPTY_REPO"
got=$(newest_source_mtime "$EMPTY_REPO")
[ "$got" = "0" ] \
  && pass "newest_source_mtime returns 0 when no sources exist" \
  || fail "newest_source_mtime returns 0 when no sources exist" "got '$got'"

# ── summary ──────────────────────────────────────────────────────────
printf '\n  %d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
