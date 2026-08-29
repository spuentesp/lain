#!/usr/bin/env bash
# Sourced by scripts/demo.sh and tests/demo_sh_freshness.sh.
#
# Pure helpers — no I/O side effects beyond what each function's name
# promises. Color codes ($YEL, $RST) and the $ALLOW_STALE toggle are
# read from the calling scope so this file works in both a terminal
# (demo.sh) and a test harness (which leaves color empty and sets
# ALLOW_STALE explicitly when exercising that path).
#
# Functions:
#   mtime_of <path>                     -> integer epoch seconds (0 on error)
#   newest_source_mtime <repo_root>     -> integer epoch seconds (0 if empty)
#   print_binary_info <binary_path>     -> 3 lines on stdout
#   check_binary_freshness <bin> <root> -> 0 if fresh or ALLOW_STALE; 1 + stderr warn if stale
#
# "Source" means what the binary is built from: Cargo.toml, Cargo.lock,
# and src/**/*.rs. Build artifacts under target/ and the test tree are
# deliberately not consulted — they do not change the binary. If a
# future build starts depending on files outside that set (a build.rs
# under tests/, say), widen newest_source_mtime to match.
#
# GNU coreutils only (`stat -c`, `find -printf`). demo.sh already
# targets Linux CI; macOS `stat -f` is out of scope.

mtime_of() {
  # GNU coreutils: %Y is mtime as epoch seconds (integer).
  # 2>/dev/null + || echo 0 keeps the function from breaking callers
  # that compare numerically against the result.
  stat -c %Y "$1" 2>/dev/null || echo 0
}

newest_source_mtime() {
  local root="$1" t max=0
  # Merge all source mtimes, sort, take the largest.
  # If the find glob matches nothing AND no Cargo manifest exists,
  # the loop never runs, so we default to 0.
  while IFS= read -r t; do
    # `find -printf %T@` emits a float ("1787954479.5054490680") while
    # `stat -c %Y` emits an integer. Truncate the fractional part so
    # both feed the integer comparison below; sub-second resolution is
    # noise at the granularity we care about.
    t="${t%%.*}"
    [ -n "$t" ] && [ "$t" -gt "$max" ] && max="$t"
  done < <(
    {
      find "$root/src" -name '*.rs' -printf '%T@\n' 2>/dev/null
      [ -f "$root/Cargo.toml" ] && stat -c %Y "$root/Cargo.toml"
      [ -f "$root/Cargo.lock" ] && stat -c %Y "$root/Cargo.lock"
    } | sort -n
  )
  printf '%s\n' "$max"
}

print_binary_info() {
  local bin="$1" ver mtime
  ver=$("$bin" --version 2>/dev/null | head -1)
  mtime=$(stat -c %y "$bin" 2>/dev/null || echo unknown)
  printf '  binary:  %s\n' "$bin"
  printf '  version: %s\n' "${ver:-unknown}"
  printf '  mtime:  %s\n'  "$mtime"
}

check_binary_freshness() {
  local bin="$1" root="$2" src_mtime bin_mtime yel rst
  yel="${YEL:-}"; rst="${RST:-}"
  bin_mtime=$(mtime_of "$bin")
  src_mtime=$(newest_source_mtime "$root")
  # Numeric compare without bc (bc isn't always installed).
  if [ "${src_mtime:-0}" -le "${bin_mtime:-0}" ]; then
    return 0  # fresh or equal
  fi
  if [ "${ALLOW_STALE:-0}" = 1 ]; then
    return 0  # stale but explicitly allowed
  fi
  printf '%s  binary may be stale — newer source files exist; use --force-build to rebuild or --allow-stale to suppress%s\n' \
    "$yel" "$rst" >&2
  return 1
}
