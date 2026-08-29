#!/usr/bin/env bash
# Test suite for install.sh non-interactive (non-TTY) behavior.
#
# Mirrors tests/install-test.sh style: sourced-script pattern, pass/fail
# counters, exits non-zero if any test fails. No bats dependency.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SCRIPT="$SCRIPT_DIR/../install.sh"

# Each test runs in a fresh temp dir so ~/.bashrc / ~/.zshrc writes can
# be asserted against an empty file. Tests that need to *drive* install.sh
# (rather than source it) set LAIN_INSTALL_DIR to a per-test tmp path so
# nothing in the real filesystem is touched.
TEST_TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP_DIR"' EXIT

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
passed=0; failed=0
pass() { echo -e "${GREEN}✓${NC} $1"; passed=$((passed + 1)); }
fail() { echo -e "${RED}✗${NC} $1"; failed=$((failed + 1)); }
note() { echo -e "${YELLOW}⚠${NC} $1"; }

# T1: When stdin is not a TTY and --yes was not passed, apply_noninteractive_defaults
#     must set OPT_YES=yes and print a banner.
test_auto_yes_on_non_tty() {
  local out rc
  # Force stdin = /dev/null for this subshell; declare OPT_INTERACTIVE=""
  # to mirror a piped `curl … | bash` call.
  out=$( ( OPT_INTERACTIVE="" bash -c '
    source "'"$INSTALL_SCRIPT"'" >/dev/null 2>&1
    apply_noninteractive_defaults </dev/null
    echo "OPT_YES=[$OPT_YES]"
  ' ) 2>&1 )
  rc=$?
  # -F: fixed-string match — without it grep treats "[yes]" as a character class.
  if echo "$out" | grep -qF "OPT_YES=[yes]"; then
    pass "non-TTY + no --yes auto-sets OPT_YES"
  else
    fail "non-TTY + no --yes did not auto-set OPT_YES (rc=$rc, output=$out)"
  fi
  # Grep for the unique banner substring "stdin is not a TTY" — using a
  # looser pattern like "non-interactive" would also match the function
  # name `apply_noninteractive_defaults` in the "command not found"
  # error before Task 3 lands.
  if echo "$out" | grep -q "stdin is not a TTY"; then
    pass "non-TTY auto-yes prints a banner"
  else
    fail "non-TTY auto-yes did not print a banner (output=$out)"
  fi
}

# T2: When stdin IS a TTY (simulated by NOT redirecting), apply_noninteractive_defaults
#     must NOT touch OPT_YES. We approximate "is TTY" by leaving stdin inherited —
#     when this test is run from a real terminal, [ -t 0 ] is true.
test_auto_yes_skipped_on_tty() {
  local out
  out=$(bash -c '
    source "'"$INSTALL_SCRIPT"'" >/dev/null 2>&1
    apply_noninteractive_defaults
    echo "OPT_YES=[$OPT_YES]"
  ' 2>&1)
  if [ -t 0 ]; then
    # -F: fixed-string match (brackets would otherwise form a char class).
    if echo "$out" | grep -qF "OPT_YES=[]"; then
      pass "TTY leaves OPT_YES unset"
    else
      fail "TTY left OPT_YES=[$out]"
    fi
  else
    note "TTY check skipped (this test is running under a non-TTY); logic is covered by the inverse test"
  fi
}

# T3: When OPT_YES=yes and stdin is /dev/null, prompt_path_mutation must NOT
#     write to ~/.bashrc. The function prints the manual export line instead.
test_path_block_skips_mutation_on_non_tty() {
  local fake_home rc content
  fake_home="$TEST_TMP_DIR/fakehome"
  mkdir -p "$fake_home"
  : > "$fake_home/.bashrc"   # exists but empty

  out=$( HOME="$fake_home" bash -c '
    source "'"$INSTALL_SCRIPT"'" >/dev/null 2>&1
    OPT_YES=yes
    export HOME="'"$fake_home"'"
    prompt_path_mutation </dev/null
  ' 2>&1 )
  rc=$?
  content=$(cat "$fake_home/.bashrc")
  # The function must exist (rc=0). Without this guard, the file-empty
  # check below would spuriously pass when prompt_path_mutation is
  # missing — `set -e` in install.sh kills the bash subshell before any
  # write, leaving the file empty for the wrong reason.
  if [ "$rc" -ne 0 ]; then
    fail "prompt_path_mutation did not run (rc=$rc, output=$out)"
    return
  fi
  if [ -z "$content" ]; then
    pass "PATH block did not write to ~/.bashrc on non-TTY"
  else
    fail "PATH block wrote to ~/.bashrc on non-TTY (content=$content)"
  fi
  if echo "$out" | grep -q "export PATH="; then
    pass "PATH block prints manual export line on non-TTY"
  else
    fail "PATH block did not print manual export line (output=$out)"
  fi
}

echo "========================================"
echo "install.sh non-interactive behavior tests"
echo "========================================"
echo ""

test_auto_yes_on_non_tty
echo ""
test_auto_yes_skipped_on_tty
echo ""
test_path_block_skips_mutation_on_non_tty
echo ""

# T4: When --interactive is set and stdin is /dev/null, the auto-yes
#     override must NOT fire. The user wants to feed answers via heredoc.
test_interactive_overrides_auto_yes() {
  local out
  out=$( OPT_INTERACTIVE=yes bash -c '
    source "'"$INSTALL_SCRIPT"'" >/dev/null 2>&1
    apply_noninteractive_defaults </dev/null
    echo "OPT_YES=[$OPT_YES]"
  ' 2>&1 )
  if echo "$out" | grep -qF "OPT_YES=[]"; then
    pass "--interactive suppresses non-TTY auto-yes"
  else
    fail "--interactive did not suppress non-TTY auto-yes (output=$out)"
  fi
}

test_interactive_overrides_auto_yes
echo ""

echo "========================================"
echo "Passed: $passed    Failed: $failed"
echo "========================================"

[ "$failed" -eq 0 ]