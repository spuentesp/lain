#!/usr/bin/env bash
# feat-suite.sh — End-to-end shell-level test of every advertised
# CLI capability of `lain`. Mirrors `tests/feat_suite.rs` for the
# parts that don't require booting an MCP server.
#
# What this exercises:
#   * CLI surface: --version, --help, <cmd> --help for every
#     documented subcommand.
#   * Repos round-trip: init → list → remove.
#   * Schema dump: writes a JSON file, asserts it's valid + 60+ tools.
#   * Doctor: exits 0 and prints `all checks passed`.
#
# Usage:
#   scripts/feat-suite.sh                 # run with the default binary
#   LAIN=path/to/lain scripts/feat-suite.sh
#
# Exit code = count of failed checks (0 = pass).

set -eu

# Resolve the binary to test. Default: the worktree release build
# (`cargo build --release --bin lain` from the repo root). The user
# can override with LAIN=…/lain.
LAIN="${LAIN:-/tmp/lain-defects/target/release/lain}"

# Color output if stdout is a TTY. Plain ASCII otherwise so the
# script's output is greppable in CI logs.
if [ -t 1 ]; then
    C_PASS=$'\033[32m'
    C_FAIL=$'\033[31m'
    C_BOLD=$'\033[1m'
    C_OFF=$'\033[0m'
else
    C_PASS="" C_FAIL="" C_BOLD="" C_OFF=""
fi

note()  { printf '%s== %s ==%s\n' "$C_BOLD" "$*" "$C_OFF"; }
pass()  { printf '  %sPASS%s %s\n' "$C_PASS" "$C_OFF" "$*"; }
fail()  { printf '  %sFAIL%s %s\n' "$C_FAIL" "$C_OFF" "$*"; FAIL=$((FAIL+1)); }

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

End-to-end shell-level test of every advertised CLI capability of lain.

Options:
  --help              Print this message and exit.
  --bin PATH          Override the binary to test (default: $LAIN).

Environment:
  LAIN                Same as --bin.

Exit code = count of failed checks (0 = all pass).
EOF
}

# Parse args.
while [ $# -gt 0 ]; do
    case "$1" in
        --help|-h)
            usage
            exit 0
            ;;
        --bin)
            shift
            [ $# -gt 0 ] || { echo "--bin requires PATH" >&2; exit 2; }
            LAIN="$1"
            shift
            ;;
        *)
            echo "unknown arg: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

# Sanity: the binary must exist. Refuse to "pass" against a stale
# build by accident.
if [ ! -x "$LAIN" ]; then
    echo "error: $LAIN is not executable; rebuild with \`cargo build --release --bin lain\`" >&2
    exit 2
fi

FAIL=0

# ─── 1. CLI surface ──────────────────────────────────────────────
note "1. CLI surface"

# `lain --version` works and prints the version banner.
VERSION_OUT="$("$LAIN" --version 2>&1)"
case "$VERSION_OUT" in
    *"lain "*) pass "--version works ($VERSION_OUT)" ;;
    *)         fail "--version unexpected: $VERSION_OUT" ;;
esac

# `lain --help` lists every documented subcommand.
HELP_OUT="$("$LAIN" --help 2>&1)"
for sub in server mcp workspaces repos query oneshot ask init hooks schema doctor; do
    if printf '%s\n' "$HELP_OUT" | grep -qE "^[[:space:]]+${sub}[[:space:]]"; then
        pass "--help lists subcommand \`${sub}\`"
    else
        fail "--help missing subcommand \`${sub}\`"
    fi
done

# `lain <cmd> --help` exits 0 for every documented subcommand.
for sub in server mcp workspaces repos query oneshot init ask hooks doctor schema; do
    if "$LAIN" "$sub" --help >/dev/null 2>&1; then
        pass "\`${sub} --help\` exits 0"
    else
        fail "\`${sub} --help\` non-zero"
    fi
done

# `lain hooks --help` lists all 5 subcommands.
HOOKS_HELP="$("$LAIN" hooks --help 2>&1)"
for hs in claim release overlap-check lock unlock; do
    if printf '%s\n' "$HOOKS_HELP" | grep -qE "^[[:space:]]+${hs}[[:space:]]"; then
        pass "hooks --help lists \`${hs}\`"
    else
        fail "hooks --help missing \`${hs}\`"
    fi
done

# `lain workspaces --help` lists create/add/remove.
WS_HELP="$("$LAIN" workspaces --help 2>&1)"
for ws in create add remove; do
    if printf '%s\n' "$WS_HELP" | grep -qE "^[[:space:]]+${ws}[[:space:]]"; then
        pass "workspaces --help lists \`${ws}\`"
    else
        fail "workspaces --help missing \`${ws}\`"
    fi
done

# `lain repos --help` lists add/list/remove.
REPOS_HELP="$("$LAIN" repos --help 2>&1)"
for r in add list remove; do
    if printf '%s\n' "$REPOS_HELP" | grep -qE "^[[:space:]]+${r}[[:space:]]"; then
        pass "repos --help lists \`${r}\`"
    else
        fail "repos --help missing \`${r}\`"
    fi
done

# ─── 2. Repos CLI round-trip ─────────────────────────────────────
note "2. Repos CLI round-trip"

WORK="$(mktemp -d -t lain-feat-suite.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

# `lain init --workspace` writes a repos.yaml. `--workspace` requires
# the dir to have a `.git`; init one so the contract holds.
(
    cd "$WORK"
    git init -q
    "$LAIN" init --workspace "$WORK" >/dev/null
)
if [ -f "$WORK/repos.yaml" ]; then
    pass "init --workspace wrote repos.yaml"
else
    fail "init --workspace did not write repos.yaml"
fi

# `lain repos list` shows the one repo.
# `repos` accepts `--config` as a global flag (between `repos` and
# the subcommand), not on the subcommand itself.
LIST_OUT="$("$LAIN" repos --config "$WORK/repos.yaml" list 2>&1)"
REPO_ID="$(printf '%s\n' "$LIST_OUT" | awk '!/^$/ && $1 != "(no" {print $1; exit}')"
if [ -n "$REPO_ID" ]; then
    pass "repos list showed repo \`${REPO_ID}\`"
else
    fail "repos list returned empty: $LIST_OUT"
fi

# `lain repos remove <id>` removes it.
"$LAIN" repos --config "$WORK/repos.yaml" remove "$REPO_ID" >/dev/null 2>&1
LIST_AFTER="$("$LAIN" repos --config "$WORK/repos.yaml" list 2>&1)"
if printf '%s\n' "$LIST_AFTER" | grep -qF "no repos registered"; then
    pass "repos remove <id> cleared the entry"
elif printf '%s\n' "$LIST_AFTER" | grep -q "^${REPO_ID}[[:space:]]"; then
    fail "repos remove <id> did NOT clear: $LIST_AFTER"
else
    # Some other shape — fail with the actual output.
    fail "repos remove <id> produced unexpected list output: $LIST_AFTER"
fi

# ─── 3. Schema dump ──────────────────────────────────────────────
note "3. Schema dump"

SCHEMA_OUT="$WORK/schema.json"
"$LAIN" schema dump --out "$SCHEMA_OUT" >/dev/null
if [ ! -s "$SCHEMA_OUT" ]; then
    fail "schema dump produced empty/missing file"
else
    # Validate JSON via python (always available in CI).
    if python3 -c "import json,sys; d=json.load(open('$SCHEMA_OUT')); assert isinstance(d, list), 'root not array'; print(len(d))" >"$WORK/count.out" 2>"$WORK/err.out"; then
        COUNT="$(cat "$WORK/count.out")"
        if [ "$COUNT" -ge 60 ]; then
            pass "schema dump valid JSON with ${COUNT} tools (>= 60)"
        else
            fail "schema dump has only ${COUNT} tools (< 60)"
        fi
        # Specific headline tools must appear.
        for tool in find_anchors get_health claim_files query_graph; do
            if python3 -c "import json; d=json.load(open('$SCHEMA_OUT')); names={t['name'] for t in d}; print('${tool}' in names)" >"$WORK/h.out" 2>/dev/null \
                && [ "$(cat "$WORK/h.out")" = "True" ]; then
                pass "schema dump contains \`${tool}\`"
            else
                fail "schema dump missing \`${tool}\`"
            fi
        done
    else
        fail "schema dump not valid JSON: $(cat "$WORK/err.out")"
    fi
fi

# ─── 4. Doctor ───────────────────────────────────────────────────
note "4. Doctor"

if "$LAIN" doctor >"$WORK/doctor.out" 2>&1; then
    pass "doctor exited 0"
else
    fail "doctor exited non-zero: $(cat "$WORK/doctor.out")"
fi
if grep -q "all checks passed" "$WORK/doctor.out"; then
    pass "doctor printed \`all checks passed\`"
else
    fail "doctor missing \`all checks passed\`: $(cat "$WORK/doctor.out")"
fi

# ─── 5. Command Center SPA e2e ───────────────────────────────────
# Spawns `lain server` against a fresh Rust fixture, drives the system
# Chromium binary through every advertised tab, captures screenshots, and
# verifies the SSE feed endpoint. Skipped if Chromium is not installed at
# /usr/bin/chromium — the feat suite should still run cleanly on hosts
# without a browser (CI runners, headless containers).
note "5. Command Center SPA e2e"

CHROMIUM_BIN="${CHROMIUM_BIN:-/usr/bin/chromium}"
if [ ! -x "$CHROMIUM_BIN" ]; then
    echo "  -- skip: chromium not found at $CHROMIUM_BIN"
elif [ ! -d "$(dirname "$0")/../tests/js/node_modules/playwright" ]; then
    echo "  -- skip: tests/js/node_modules/playwright not installed"
    echo "     run: PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --prefix tests/js --save-dev playwright"
else
    SPA_WORK="$(mktemp -d -t lain-feat-spa.XXXXXX)"
    SPA_LOG="$SPA_WORK/spa.log"
    if PLAYWRIGHT_BROWSERS_PATH=0 LAIN_BIN="$LAIN" \
            node "$(dirname "$0")/../tests/js/spa_e2e.test.js" \
            >"$SPA_LOG" 2>&1; then
        SPA_PASS="$(grep -cE '^  PASS ' "$SPA_LOG" || true)"
        SPA_FAIL="$(grep -cE '^  FAIL ' "$SPA_LOG" || true)"
        pass "SPA e2e ran cleanly (${SPA_PASS} pass, ${SPA_FAIL} fail)"
    else
        SPA_RC=$?
        fail "SPA e2e exited ${SPA_RC} (see $SPA_LOG)"
        # Surface the per-tab roll-up so the failure is actionable.
        grep -E '^  (overview|repos|query|tools|graph|sse|chrome)' "$SPA_LOG" \
            | head -20 || true
    fi
    rm -rf "$SPA_WORK"
fi


# ─── 6. Negative-path tests ───────────────────────────────────────
note "6. Negative-path tests"

if PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path "$(dirname "$0")/../Cargo.toml" \
        --test feat_negative_paths --quiet 2>"$WORK/np.err"; then
    pass "feat_negative_paths passed"
else
    fail "feat_negative_paths failed: $(tail -20 "$WORK/np.err")"
fi

# ─── 7. Concurrent-agent race tests ──────────────────────────────
note "7. Concurrent-agent race tests"

if PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path "$(dirname "$0")/../Cargo.toml" \
        --test multi_agent_concurrency --quiet -- --test-threads=1 2>"$WORK/cc.err"; then
    pass "multi_agent_concurrency passed"
else
    fail "multi_agent_concurrency failed: $(tail -20 "$WORK/cc.err")"
fi

# ─── 8. Federation end-to-end tests ─────────────────────────────
note "8. Federation end-to-end tests"

if PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path "$(dirname "$0")/../Cargo.toml" \
        --test federation_e2e --quiet -- --test-threads=1 2>"$WORK/fe.err"; then
    pass "federation_e2e passed"
else
    fail "federation_e2e failed: $(tail -20 "$WORK/fe.err")"
fi

# ─── 9. Failure-mode tests ───────────────────────────────────────
note "9. Failure-mode tests"

if PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path "$(dirname "$0")/../Cargo.toml" \
        --test failure_modes --quiet -- --test-threads=1 2>"$WORK/fm.err"; then
    pass "failure_modes passed"
else
    fail "failure_modes failed: $(tail -20 "$WORK/fm.err")"
fi

# ─── 10. Performance budgets ─────────────────────────────────────
note "10. Performance budgets"

if PATH="$HOME/.cargo/bin:$PATH" cargo test --manifest-path "$(dirname "$0")/../Cargo.toml" \
        --test performance_budgets --quiet -- --test-threads=1 2>"$WORK/pb.err"; then
    pass "performance_budgets passed"
else
    fail "performance_budgets failed: $(tail -20 "$WORK/pb.err")"
fi

# ─── Wrap-up ─────────────────────────────────────────────────────
echo
if [ "$FAIL" -eq 0 ]; then
    printf '%sALL CHECKS PASSED%s\n' "$C_PASS" "$C_OFF"
    exit 0
else
    printf '%s%d FAILURE(S)%s\n' "$C_FAIL" "$FAIL" "$C_OFF"
    exit "$FAIL"
fi
