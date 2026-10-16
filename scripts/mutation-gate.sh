#!/usr/bin/env bash
# Incremental mutation gate: prove that changed product code is OBSERVED by a
# test. Generates mutants only on lines touched by the PR diff (cargo-mutants
# `--in-diff`) and fails when any of them survives the unit-test suite.
# Surviving mutants on a diff = hardcoded branches / dead code / assertions
# that assert less than they claim.
#
#   scripts/mutation-gate.sh                      # diff from merge-base(HEAD, origin/dev)
#   scripts/mutation-gate.sh 72eebebc~2           # diff against an explicit base
#   scripts/mutation-gate.sh 72eebebc~2 key_compat # only changed paths containing "key_compat"
#
# Args: optional base ref (default: `git merge-base HEAD origin/dev`, falling
# back to `HEAD~1` when origin/dev is unavailable), optional filter matching a
# substring of the changed file paths (same shape as check-formal.sh's filter).
#
# --in-diff semantics (cargo-mutants 27.1.0, checked against its source in
# src/in_diff.rs): the patch is parsed as a unified diff; only mutants whose
# span intersects a *changed* line of a `.rs` file (added lines, or lines
# adjacent to a deletion) are examined. The patch's "new" text must match the
# working tree (that is why the diff is taken as `<base>...HEAD`, whose right
# side is HEAD), else cargo-mutants exits 5 (FilterDiffMismatch). An empty
# diff, a diff with no Rust changes, or one intersecting no mutants exits 0
# without building anything -- that is this gate's "no mutants in diff" path.
#
# Product code only: cargo-mutants discovers only the library's sources (no
# `tests/` integration targets, no `#[cfg(test)]` modules; verified via
# `--list-files`), and `.cargo/mutants.toml` (read automatically) excludes the
# triaged equivalent-mutant classes. New exclusions belong there, with a
# comment justifying each one -- never silently widen exclusions to turn a
# MISSED green. The gate adds two scope filters of its own on the command
# line (`-e src/main.rs -e src/bin/*`): bin-target sources are never compiled
# by `cargo test --lib`, so every mutant there would "survive" vacuously
# instead of saying anything about test strength. Bin entry-point behavior is
# pinned by tests/cli_surface.rs and the e2e suites in the `test` job.
#
# Per-mutant cost is bounded by `--cargo-arg=--lib`: both the build and the
# test phase are restricted to the library target (`cargo test --lib`), which
# on this tree is ~2500 unit tests finishing in ~35s (wall ~45s) and avoids
# compiling the ~85 integration-test binaries (the Makefile `mutants` target
# makes the same choice for the same disk reason).
#
#   Blind spots of `--cargo-arg=--lib` (know them before trusting a MISSED):
#   - integration tests under `tests/` never run, so a mutant observed only
#     by an integration test is reported MISSED even though the full suite
#     would catch it. This is a live risk for modules pinned only from
#     `tests/` (e.g. `src/server/query/executor.rs`, driven by
#     tests/query_engine_oracle.rs) -- re-check before believing a MISSED
#     there;
#   - doc tests never run either;
#   - bin targets are excluded outright (see above), so the gate is silent
#     about src/main.rs / src/bin/* diffs.
#   Re-check a reported survivor against the full suite before treating it as
#   a real gap:
#       cargo test --workspace -- <test filters>
#   (re-apply the named mutant by hand, or run `cargo mutants` on that file
#   without `--cargo-arg=--lib`). A survivor the full suite also misses is a
#   real finding: add the test, do not add an exclusion.
#
# Baseline: `--baseline skip` -- the unmutated tree is known green (CI ran the
# full suite on it), so per-mutant budget goes to mutants, not to re-proving
# the baseline. Timeouts are absolute (baseline is skipped, so there is no
# measured baseline to multiply): `--timeout 300` for the test phase vs the
# measured ~45s focused run (~7x headroom for a loaded runner; a mutant that
# hangs longer is reported TIMEOUT and fails the gate), `--build-timeout 600`
# for the build phase (cold scratch trees pay a full `lain` compile, measured
# ~70s warm-deps).
#
# Disk policy: `CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0` keep
# each scratch build small (the default debuginfo build of the lib test
# harness alone is ~640MB; `make mutants` makes the same choice). Set them to
# something else in the environment to override.
#
# Host quirk (this is why the header documents TMPDIR instead of hardcoding
# one): on the current dev machine `/tmp` is quota-exhausted, and a TMPDIR
# inside the repo breaks 9 git walk-up tests. Local runs therefore need
#     TMPDIR=/data/agents/tmp-lain-tests
# (and `JAVA_TOOL_OPTIONS=-Djava.io.tmpdir=/data/agents/tmp-lain-tests` for
# TLC runs -- not needed here). CI has none of these problems. The script
# itself stays host-agnostic: it only uses `mktemp -d`, which honours the
# caller's TMPDIR.
#
# Exit: 0 = no mutants in diff, or every mutant CAUGHT/UNVIABLE
#       1 = at least one MISSED/TIMEOUT (the surviving list is printed)
#       2 = tool/setup error (cargo-mutants missing, bad base, stale patch, ...)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

usage_error() {
  echo "usage: scripts/mutation-gate.sh [base-ref] [path-filter]" >&2
  exit 2
}
[[ $# -gt 2 ]] && usage_error

base="${1:-}"
filter="${2:-}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "mutation-gate: cargo not found on PATH" >&2
  exit 2
fi
if ! cargo mutants --help >/dev/null 2>&1; then
  echo "mutation-gate: cargo-mutants not installed (cargo install cargo-mutants)" >&2
  exit 2
fi

if [[ -z "$base" ]]; then
  if base="$(git merge-base HEAD origin/dev 2>/dev/null)" && [[ -n "$base" ]]; then
    :
  else
    base="HEAD~1"
    echo "mutation-gate: origin/dev unavailable; falling back to HEAD~1" >&2
  fi
fi
if ! git rev-parse --verify --quiet "${base}^{commit}" >/dev/null; then
  echo "mutation-gate: no such base commit: $base" >&2
  exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Three-dot diff: `<base>...HEAD` = changes since the merge base, with HEAD as
# the "new" side -- exactly the content of the working tree `--in-diff` checks
# the patch against.
if [[ -n "$filter" ]]; then
  git diff "${base}...HEAD" -- "*${filter}*" >"$WORK/diff.patch" || exit 2
else
  git diff "${base}...HEAD" >"$WORK/diff.patch" || exit 2
fi

if [[ ! -s "$WORK/diff.patch" ]]; then
  echo "no mutants in diff (no changes between $base and HEAD)"
  exit 0
fi

# Fast path before cargo-mutants is even spawned: a diff that touches no
# library Rust source cannot produce mutants (tests/, docs/ and scripts/ are
# outside cargo-mutants' examine set; `src/**/*_tests.rs` files are cfg(test)
# modules and produce none either -- cargo-mutants exits 0 on those in about
# two seconds, without building).
src_rs_files="$(grep -cE '^diff --git a/src/.*\.rs$' "$WORK/diff.patch" || true)"
if [[ "$src_rs_files" -eq 0 ]]; then
  echo "no mutants in diff (no product-code Rust changes since $base)"
  exit 0
fi

# Keep each scratch build small; caller may override (see header).
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"

# Sequential (`-j` default 1) keeps one scratch tree and one target on disk.
# `--baseline skip`: tree known green (see header). Output streams through so
# a long CI run is not a dark window; the structured per-mutant table and
# summary come after.
cargo mutants \
  --in-diff "$WORK/diff.patch" \
  --baseline skip \
  --timeout 300 \
  --build-timeout 600 \
  --cargo-arg=--lib \
  -e 'src/main.rs' -e 'src/bin/*' \
  --output "$WORK" \
  --unviable --caught \
  2>&1 | tee "$WORK/cargo-mutants.log"
cm_rc=${PIPESTATUS[0]}

OUT="$WORK/mutants.out"
missed=0 timeout=0 unviable=0 caught=0
[[ -s "$OUT/missed.txt" ]] && missed=$(wc -l <"$OUT/missed.txt")
[[ -s "$OUT/timeout.txt" ]] && timeout=$(wc -l <"$OUT/timeout.txt")
[[ -s "$OUT/unviable.txt" ]] && unviable=$(wc -l <"$OUT/unviable.txt")
[[ -s "$OUT/caught.txt" ]] && caught=$(wc -l <"$OUT/caught.txt")
total=$((missed + timeout + unviable + caught))

# Per-mutant lines, worst first.
if [[ -s "$OUT/missed.txt" ]]; then
  while IFS= read -r m; do printf 'MISSED   %s\n' "$m"; done <"$OUT/missed.txt"
fi
if [[ -s "$OUT/timeout.txt" ]]; then
  while IFS= read -r m; do printf 'TIMEOUT  %s\n' "$m"; done <"$OUT/timeout.txt"
fi
if [[ -s "$OUT/unviable.txt" ]]; then
  while IFS= read -r m; do printf 'UNVIABLE %s\n' "$m"; done <"$OUT/unviable.txt"
fi
if [[ -s "$OUT/caught.txt" ]]; then
  while IFS= read -r m; do printf 'CAUGHT   %s\n' "$m"; done <"$OUT/caught.txt"
fi

if [[ $total -eq 0 && $cm_rc -eq 0 ]]; then
  echo "no mutants in diff (changes since $base generate no mutants)"
  exit 0
fi

if [[ $total -gt 0 ]]; then
  echo "$total mutants: $caught caught, $missed missed, $timeout timeout, $unviable unviable"
fi

# cargo-mutants exit codes (src/exit_code.rs): 0 all caught, 2 missed,
# 3 timeout, 4 baseline failed, 5 diff/tree mismatch, 6 invalid diff,
# 1 usage, 70 internal. Only 2 and 3 are findings; the rest are setup/tool
# problems, except 0 which we already handled.
case $cm_rc in
  0) exit 0 ;;
  2 | 3)
    echo "mutation-gate: $((missed + timeout)) surviving mutant(s) — each line above marked MISSED/TIMEOUT names behavior no test observes" >&2
    exit 1 ;;
  4)
    echo "mutation-gate: baseline failed (unmutated tree is not green); refusing to judge mutants" >&2
    exit 2 ;;
  5)
    echo "mutation-gate: diff does not match the working tree (stale patch? wrong base?); re-run against a clean tree" >&2
    exit 2 ;;
  6)
    echo "mutation-gate: could not parse the diff" >&2
    exit 2 ;;
  *)
    echo "mutation-gate: cargo-mutants failed (exit $cm_rc); log above" >&2
    exit 2 ;;
esac
