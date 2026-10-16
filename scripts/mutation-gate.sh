#!/usr/bin/env bash
# Incremental mutation gate: prove that changed product code is OBSERVED by a
# test. Generates mutants only on lines the diff touches (cargo-mutants
# `--in-diff`) and fails when a mutant survives both test phases.
# Surviving mutants on a diff = hardcoded branches / dead code / assertions
# that assert less than they claim.
#
#   scripts/mutation-gate.sh                       # diff from merge-base(HEAD, origin/dev)
#   scripts/mutation-gate.sh 72eebebc~2            # diff against an explicit base
#   scripts/mutation-gate.sh 72eebebc~2 key_compat # only changed paths containing "key_compat"
#   scripts/mutation-gate.sh HEAD key_compat --confirm-cmd 'cargo test --test foo'
#   (--confirm-cmd may appear anywhere; it may also be written
#    `--confirm-cmd=<cmd>`)
#
# Args: optional base ref (default: `git merge-base HEAD origin/dev`, falling
# back to `HEAD~1` when origin/dev is unavailable), optional filter matching a
# substring of the changed file paths (same shape as check-formal.sh's
# filter), optional `--confirm-cmd` overriding the phase-2 test command.
#
# Diff semantics: the patch is `git diff <merge-base(base, HEAD)>` -- the
# branch's committed changes since the merge base PLUS any uncommitted work
# in the tree, so the gate can run before committing. Its "new" side is the
# working tree by construction, which is what `--in-diff` checks against (a
# stale patch fails with cargo-mutants exit 5). In CI the checkout is clean
# and the base is the PR base SHA, so this is exactly the PR diff. Untracked
# files are invisible to `git diff` -- `git add -N` them first.
#
# --in-diff semantics (cargo-mutants 27.1.0, checked against its source in
# src/in_diff.rs): the patch is parsed as a unified diff; only mutants whose
# span intersects a *changed* line of a `.rs` file (added lines, or lines
# adjacent to a deletion) are examined. An empty diff, a diff with no Rust
# changes, or one intersecting no mutants exits 0 without building anything --
# that is this gate's "no mutants in diff" path.
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
# TWO PHASES -- a `--lib` survival is a CANDIDATE, not a verdict:
#
#   Phase 1 (census): every mutant in the diff is tested against
#   `cargo test --lib` (`--cargo-arg=--lib`; ~2500 unit tests, ~35s of tests /
#   ~45s wall here; ~45-55s per mutant in steady state). This bounds per-mutant
#   cost and avoids compiling the ~85 integration-test binaries per mutant.
#   Blind spot: integration tests under `tests/` never run in phase 1, so a
#   mutant observed only by an integration test (e.g. `src/server/query/
#   executor.rs`, driven by tests/query_engine_oracle.rs) shows up as MISSED
#   even though the suite would catch it. Doc tests never run either.
#   Both phases also skip the workspace-root-as-git-fixture tests (see the
#   `fixture_skips` block below): they cannot run inside cargo-mutants'
#   copied scratch tree and would poison outcomes into "caught".
#
#   Phase 2 (confirm): every phase-1 MISSED/TIMEOUT is re-tested as its own
#   mutant against the fuller command -- default `cargo test`, i.e. the
#   package's full suite (lib + bins + all integration tests + doc tests);
#   `--confirm-cmd` may adjust it (it must stay `cargo test` with optional
#   args -- the confirm run is executed by cargo-mutants' test runner; args
#   before a bare `--` become `cargo test` options, args after it become
#   test-name filters). Only a mutant that SURVIVES phase 2 is a real
#   survivor and fails the gate. A mutant caught in phase 2 is reported
#   `caught-on-confirm` -- a phase-1 false alarm -- and does not fail.
#   Candidates are checked one mutant at a time (that is inherent to
#   mutation testing) but share a single confirm run so the full test-binary
#   set is built once.
#
#   Cost profile: well-tested diffs pay only phase 1. A diff whose mutants
#   phase 1 cannot see pays one full-suite confirm round on top: build of all
#   test targets (~5-10 min, warm deps) + ~12 min per candidate here. The
#   CI job timeout bounds the worst case.
#
# Baseline: `--baseline skip` in both phases -- the unmutated tree is known
# green (CI's `test` job runs the full suite on every PR), so the budget goes
# to mutants. Implication: if the confirm environment were itself red,
# candidates would be misjudged caught-on-confirm; the `test` job's green
# suite is the evidence that it is not. Timeouts are absolute (no measured
# baseline to multiply from): phase 1 `--timeout 300` vs the measured ~45s
# focused run (~7x headroom), `--build-timeout 600`; phase 2 `--timeout 1800`
# vs the ~12 min full suite, `--build-timeout 900`. A mutant that hangs past
# its timeout is reported TIMEOUT and fails the gate (a phase-1 TIMEOUT that
# hangs again in phase 2 is a confirmed survivor).
#
# Disk policy: `CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0` keep
# each scratch build small (the default debuginfo build of the lib test
# harness alone is ~640MB; `make mutants` makes the same choice). Set them to
# something else in the environment to override. Phase 2 additionally builds
# every integration-test binary (a few GB even without debuginfo).
#
# Host quirk (this is why the header documents TMPDIR instead of hardcoding
# one): on the current dev machine `/tmp` is quota-exhausted, and a TMPDIR
# inside the repo breaks 9 git walk-up tests. Local runs therefore need
#     TMPDIR=/data/agents/tmp-lain-tests
# (and `JAVA_TOOL_OPTIONS=-Djava.io.tmpdir=/data/agents/tmp-lain-tests` for
# TLC runs -- not needed here). CI has none of these problems. The script
# itself stays host-agnostic: it only uses `mktemp -d`, which honours the
# caller's TMPDIR (and so does the confirm command).
#
# Exit: 0 = no mutants in diff, or every mutant CAUGHT/CAUGHT-ON-CONFIRM/
#           UNVIABLE
#       1 = at least one survivor confirmed by phase 2 (MISSED/TIMEOUT)
#       2 = tool/setup error (cargo-mutants missing, bad base, stale patch, ...)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

usage_error() {
  echo "usage: scripts/mutation-gate.sh [base-ref] [path-filter] [--confirm-cmd <'cargo test' args>]" >&2
  exit 2
}

base=""
filter=""
confirm_cmd="cargo test"
pos=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --confirm-cmd)
      [[ $# -ge 2 ]] || usage_error
      confirm_cmd="$2"
      shift 2 ;;
    --confirm-cmd=*)
      confirm_cmd="${1#--confirm-cmd=}"
      shift ;;
    *)
      pos=$((pos + 1))
      case "$pos" in
        1) base="$1" ;;
        2) filter="$1" ;;
        *) usage_error ;;
      esac
      shift ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  echo "mutation-gate: cargo not found on PATH" >&2
  exit 2
fi
if ! cargo mutants --help >/dev/null 2>&1; then
  echo "mutation-gate: cargo-mutants not installed (cargo install cargo-mutants)" >&2
  exit 2
fi

# --confirm-cmd must remain `cargo test` (the confirm run is executed by
# cargo-mutants' test runner); its optional args are forwarded (see header).
# Validated up front so a bad value fails before any build.
read -r -a confirm_words <<<"$confirm_cmd"
if [[ "${confirm_words[0]:-}" != "cargo" || "${confirm_words[1]:-}" != "test" ]]; then
  echo "mutation-gate: --confirm-cmd must be 'cargo test' with optional args" >&2
  exit 2
fi
confirm_args=()
trailing=()
seen_sep=0
for w in "${confirm_words[@]:2}"; do
  if [[ $seen_sep -eq 1 ]]; then
    trailing+=("$w")
  elif [[ "$w" == "--" ]]; then
    seen_sep=1
  else
    confirm_args+=("--cargo-test-arg=$w")
  fi
done

# Fixture-dependent tests: these open the workspace root (the build-time
# CARGO_MANIFEST_DIR) as a git repository. cargo-mutants builds and runs
# every mutant in a COPIED scratch tree under TMPDIR that carries no
# `.git`, so each of them fails on an unrelated `Git("could not find
# repository at …")` and poisons every mutant outcome into "caught" —
# proven by the red demo, where a dead `#[allow(dead_code)]` helper looked
# caught on exactly these 24 failures. Skipped in BOTH phases; the same
# modules' mutants are excluded in .cargo/mutants.toml so a PR touching
# them cannot false-MISS either. Known cost: these tests cannot observe
# mutants until they fixture their own temp repos (tracked follow-up).
# NOTE the double `--`: the first ends cargo-mutants' own args, the second is
# passed through to `cargo test` as its libtest separator (cargo-mutants
# inserts none — the `make mutants` pattern is `-- -- …`). Without it,
# `--skip` is rejected by cargo, the test phase exits instantly, and every
# mutant looks "caught" (0s test).
fixture_skips=(
  -- --skip server::git_tests::
  --skip gitops_tests::
  --skip job_panic_tests::
  --skip wiring_tests::
  --skip repo_index::tests::
  --skip handles::ingest::tests::sidecar_watchdog_clears_on_cancel
  --skip handler::tests::health_response_surfaces_git_sensor_telemetry
  --skip tools::tests::get_agent_strategy_
)

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

# Merge-base semantics for the committed side (same as `base...HEAD`), then
# straight to the working tree so uncommitted work is judged too (see header).
if ! mb="$(git merge-base "$base" HEAD 2>/dev/null)" || [[ -z "$mb" ]]; then
  mb="$base"
fi
if [[ -n "$filter" ]]; then
  git diff "$mb" -- "*${filter}*" >"$WORK/diff.patch" || exit 2
else
  git diff "$mb" >"$WORK/diff.patch" || exit 2
fi

if [[ ! -s "$WORK/diff.patch" ]]; then
  echo "no mutants in diff (no changes since $base)"
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

# Escape regex metacharacters so a mutant name can be matched literally.
re_escape() {
  local s="$1" out="" i c
  for ((i = 0; i < ${#s}; i++)); do
    c="${s:i:1}"
    case "$c" in
      '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\')
        out+="\\$c" ;;
      *) out+="$c" ;;
    esac
  done
  printf '%s' "$out"
}

echo "phase 1 (cargo test --lib): census over the diff"

# Sequential (`-j` default 1) keeps one scratch tree and one target on disk.
# `--baseline skip`: tree known green (see header). Output streams through so
# a long CI run is not a dark window; the structured table and summary come
# after both phases.
cargo mutants \
  --in-diff "$WORK/diff.patch" \
  --baseline skip \
  --timeout 300 \
  --build-timeout 600 \
  --cargo-arg=--lib \
  -e 'src/main.rs' -e 'src/bin/*' \
  --output "$WORK/phase1" \
  --unviable --caught \
  ${fixture_skips[@]+"--" "${fixture_skips[@]}"} \
  2>&1 | tee "$WORK/phase1.log"
cm_rc=${PIPESTATUS[0]}

OUT1="$WORK/phase1/mutants.out"
missed1=0 timeout1=0 unviable1=0 caught1=0
[[ -s "$OUT1/missed.txt" ]] && missed1=$(wc -l <"$OUT1/missed.txt")
[[ -s "$OUT1/timeout.txt" ]] && timeout1=$(wc -l <"$OUT1/timeout.txt")
[[ -s "$OUT1/unviable.txt" ]] && unviable1=$(wc -l <"$OUT1/unviable.txt")
[[ -s "$OUT1/caught.txt" ]] && caught1=$(wc -l <"$OUT1/caught.txt")
total=$((missed1 + timeout1 + unviable1 + caught1))

if [[ $total -eq 0 && $cm_rc -eq 0 ]]; then
  echo "no mutants in diff (changes since $base generate no mutants)"
  exit 0
fi

# Phase-2 candidates: every phase-1 MISSED/TIMEOUT (see header).
candidates=()
for f in missed timeout; do
  if [[ -s "$OUT1/$f.txt" ]]; then
    while IFS= read -r m; do
      candidates+=("$m")
    done <"$OUT1/$f.txt"
  fi
done

declare -A verdict   # candidate name -> final status
while IFS= read -r m; do [[ -n "$m" ]] && verdict["$m"]="CAUGHT"; done <"$OUT1/caught.txt" 2>/dev/null
while IFS= read -r m; do [[ -n "$m" ]] && verdict["$m"]="UNVIABLE"; done <"$OUT1/unviable.txt" 2>/dev/null

cm2_rc=0
if [[ ${#candidates[@]} -gt 0 ]]; then
  # One literal-name regex covering all candidates; each is still mutated and
  # tested on its own, but they share the confirm build (see header).
  alts=""
  for m in "${candidates[@]}"; do
    esc="$(re_escape "$m")"
    [[ -n "$alts" ]] && alts+="|"
    alts+="$esc"
  done

  echo "phase 2 (${confirm_cmd}): confirming ${#candidates[@]} candidate(s)"
  cargo mutants \
    --in-diff "$WORK/diff.patch" \
    --baseline skip \
    --timeout 1800 \
    --build-timeout 900 \
    -e 'src/main.rs' -e 'src/bin/*' \
    -F "^(${alts})\$" \
    ${confirm_args[@]+"${confirm_args[@]}"} \
    --output "$WORK/phase2" \
    --unviable --caught \
    -- "${fixture_skips[@]}" ${trailing[@]+"${trailing[@]}"} \
    2>&1 | tee "$WORK/phase2.log"
  cm2_rc=${PIPESTATUS[0]}

  OUT2="$WORK/phase2/mutants.out"
  for m in "${candidates[@]}"; do
    if [[ -s "$OUT2/caught.txt" ]] && grep -Fxq "$m" "$OUT2/caught.txt"; then
      verdict["$m"]="CAUGHT-ON-CONFIRM"
    elif [[ -s "$OUT2/missed.txt" ]] && grep -Fxq "$m" "$OUT2/missed.txt"; then
      verdict["$m"]="MISSED"
    elif [[ -s "$OUT2/timeout.txt" ]] && grep -Fxq "$m" "$OUT2/timeout.txt"; then
      verdict["$m"]="TIMEOUT"
    elif [[ -s "$OUT2/unviable.txt" ]] && grep -Fxq "$m" "$OUT2/unviable.txt"; then
      verdict["$m"]="UNVIABLE"
    else
      echo "mutation-gate: candidate not judged by the confirm run (tool error): $m" >&2
      exit 2
    fi
  done
  case $cm2_rc in
    0 | 2 | 3) ;;
    *)
      echo "mutation-gate: confirm run failed (exit $cm2_rc); log above" >&2
      exit 2 ;;
  esac
fi

# Per-mutant lines, worst first: confirmed survivors, then the false alarms,
# then the rest.
for st in MISSED TIMEOUT CAUGHT-ON-CONFIRM UNVIABLE CAUGHT; do
  for m in "${!verdict[@]}"; do
    [[ "${verdict[$m]}" == "$st" ]] && printf '%-17s %s\n' "$st" "$m"
  done
done

confirmed=0
for m in "${!verdict[@]}"; do
  [[ "${verdict[$m]}" == "MISSED" || "${verdict[$m]}" == "TIMEOUT" ]] && confirmed=$((confirmed + 1))
done
caught_final=0 caught_on_confirm=0 unviable_final=0 timeout_final=0 missed_final=0
for m in "${!verdict[@]}"; do
  case "${verdict[$m]}" in
    CAUGHT) caught_final=$((caught_final + 1)) ;;
    CAUGHT-ON-CONFIRM) caught_on_confirm=$((caught_on_confirm + 1)) ;;
    UNVIABLE) unviable_final=$((unviable_final + 1)) ;;
    MISSED) missed_final=$((missed_final + 1)) ;;
    TIMEOUT) timeout_final=$((timeout_final + 1)) ;;
  esac
done

if [[ $total -eq 0 ]]; then
  echo "mutation-gate: cargo-mutants produced no results (exit $cm_rc)" >&2
  exit 2
fi

echo "phase 1 (cargo test --lib): $total mutants: $caught1 caught, $missed1 missed, $timeout1 timeout, $unviable1 unviable"
if [[ ${#candidates[@]} -gt 0 ]]; then
  echo "phase 2 (${confirm_cmd}): ${#candidates[@]} candidates: $caught_on_confirm caught-on-confirm, $confirmed confirmed survivors"
fi
echo "$total mutants: $caught_final caught, $caught_on_confirm caught-on-confirm, $missed_final missed, $timeout_final timeout, $unviable_final unviable"

# cargo-mutants exit codes (src/exit_code.rs): 0 all caught, 2 missed,
# 3 timeout, 4 baseline failed, 5 diff/tree mismatch, 6 invalid diff,
# 1 usage, 70 internal. Only confirmed MISSED/TIMEOUT are findings.
if [[ $confirmed -gt 0 ]]; then
  echo "mutation-gate: $confirmed surviving mutant(s) — each line above marked MISSED/TIMEOUT survived the full suite too" >&2
  exit 1
fi
case $cm_rc in
  0) exit 0 ;;
  2 | 3)
    # phase-1 misses/timeouts existed but every one was caught on confirm.
    exit 0 ;;
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
