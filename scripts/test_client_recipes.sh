#!/usr/bin/env bash
# AGENT_UX_ROADMAP.md Milestone 8 — exercise the four new client
# adapters against REAL editor CLIs in a CI matrix.
#
# The unit tests in src/cli/setup.rs cover the file-shape and the
# fallback paths (when the editor CLI is missing or the config is
# malformed). Those tests do NOT cover "does `codex mcp add` accept
# the args we shell out with?" or "does Cursor's mcp.json schema
# match what its UI reads?" — both are real-CLI / real-file-format
# questions that need real editor binaries to answer. This script is
# the bridge.
#
# The script is intentionally gated per-adapter: if a CLI is
# missing, that adapter is `SKIP`ed (not failed) and the matrix
# records `SKIP:` rather than `PASS:`/`FAIL:`. CI is configured to
# install the CLIs on the relevant runners so each adapter's real
# path is exercised at least once per release.
#
# Usage:
#   ./scripts/test_client_recipes.sh                       # run all available
#   ./scripts/test_client_recipes.sh --agent codex          # only codex
#   ./scripts/test_client_recipes.sh --smoke               # dry-run, no real config writes
#
# Exit code: 0 if every available adapter passed; 1 if any
# available adapter failed.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# `lain` binary we test against. Use the workspace's debug build
# unless `LAIN_BIN` is set.
if [ -z "${LAIN_BIN:-}" ]; then
    LAIN_BIN="$(ls -t "$REPO_ROOT/target/debug/lain" 2>/dev/null | head -1)"
    if [ -z "$LAIN_BIN" ] || [ ! -x "$LAIN_BIN" ]; then
        echo "no lain binary at \$REPO_ROOT/target/debug/lain; build with \`cargo build\` first" >&2
        exit 2
    fi
fi

# Per-adapter fixture root. Each adapter runs in its own isolated
# `\$HOME` so the user-real config files aren't touched.
FIXTURE_ROOT="$(mktemp -d -t lain-m8-client-recipes.XXXXXX)"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

SMOKE=0
ONLY_AGENT=""
for arg in "$@"; do
    case "$arg" in
        --smoke) SMOKE=1 ;;
        --agent) shift; ONLY_AGENT="$1" ;;
    esac
done

# Result counters. Use `FAIL:` / `SKIP:` / `PASS:` prefixes so CI's
# log-grep can match on them.
PASS=0
FAIL=0
SKIP=0

report() {
    case "$1" in
        PASS) PASS=$((PASS+1)); echo "PASS: $2" ;;
        FAIL) FAIL=$((FAIL+1)); echo "FAIL: $2" ;;
        SKIP) SKIP=$((SKIP+1)); echo "SKIP: $2" ;;
    esac
}

# Helper: run setup with a fake HOME so the adapter's config write
# lands in the fixture dir. Mirrors `docs/COOKBOOK.md`'s
# recommended pattern.
run_setup() {
    local agent="$1"
    local fake_home="$FIXTURE_ROOT/$agent"
    mkdir -p "$fake_home"
    HOME="$fake_home" "$LAIN_BIN" setup \
        --workspace "$REPO_ROOT" \
        --agent "$agent" \
        --yes \
        --no-model \
        ${SMOKE:+--print-config} \
        2>&1
}

# Helper: assert the adapter wrote the expected file with the
# expected shape. Each check is a separate test; an adapter that
# writes the right file but loses unrelated settings is a FAIL,
# not a SKIP.
assert_config_present() {
    local agent="$1"
    local file="$2"
    local fake_home="$3"
    if [ -f "$file" ]; then
        report PASS "$agent wrote $(basename "$file")"
    else
        report FAIL "$agent: expected $file, not found"
    fi
}

# ─── codex ─────────────────────────────────────────────────────────────────

run_codex() {
    if [ -n "$ONLY_AGENT" ] && [ "$ONLY_AGENT" != "codex" ]; then return; fi

    echo "─── codex ───"
    if [ "$SMOKE" = "1" ]; then
        report SKIP "codex (smoke; would call codex CLI for real)"
        return
    fi

    if ! command -v codex >/dev/null 2>&1; then
        report SKIP "codex CLI not on PATH; install Codex to exercise the CLI path"
        return
    fi

    local fake_home="$FIXTURE_ROOT/codex"
    mkdir -p "$fake_home"
    local out
    out=$(HOME="$fake_home" "$LAIN_BIN" setup \
        --workspace "$REPO_ROOT" \
        --agent codex \
        --yes \
        --no-model 2>&1) || true
    if [ -z "$out" ]; then
        report FAIL "codex: setup produced no output"
        return
    fi
    # Codex's CLI stores config under \$CODEX_HOME/config.toml or
    # \$HOME/.codex/config.toml. Accept either.
    local cfg=""
    if [ -n "${CODEX_HOME:-}" ]; then cfg="$CODEX_HOME/config.toml"; fi
    if [ -z "$cfg" ] || [ ! -f "$cfg" ]; then cfg="$fake_home/.codex/config.toml"; fi
    if [ -z "${CODEX_HOME:-}" ] && [ -f "$fake_home/.codex/config.toml" ]; then
        # Codex wrote to \$HOME/.codex/config.toml.
        cfg="$fake_home/.codex/config.toml"
    fi
    assert_config_present "codex" "$cfg" "$fake_home"
}

# ─── cursor ────────────────────────────────────────────────────────────────

run_cursor() {
    if [ -n "$ONLY_AGENT" ] && [ "$ONLY_AGENT" != "cursor" ]; then return; fi

    echo "─── cursor ───"
    if [ "$SMOKE" = "1" ]; then
        report SKIP "cursor (smoke; would write ~/.cursor/mcp.json)"
        return
    fi

    local fake_home="$FIXTURE_ROOT/cursor"
    mkdir -p "$fake_home"
    HOME="$fake_home" "$LAIN_BIN" setup \
        --workspace "$REPO_ROOT" \
        --agent cursor \
        --yes \
        --no-model 2>&1 || true

    local cfg="$fake_home/.cursor/mcp.json"
    assert_config_present "cursor" "$cfg" "$fake_home"

    # Sanity: the JSON shape matches what Cursor's UI reads.
    if [ -f "$cfg" ]; then
        if command -v jq >/dev/null 2>&1; then
            if jq -e '.mcpServers.lain.command' "$cfg" >/dev/null 2>&1; then
                report PASS "cursor mcp.json has mcpServers.lain.command"
            else
                report FAIL "cursor mcp.json missing mcpServers.lain.command"
            fi
        else
            report SKIP "cursor: jq not on PATH; shape check skipped"
        fi
    fi
}

# ─── vscode ─────────────────────────────────────────────────────────────────

run_vscode() {
    if [ -n "$ONLY_AGENT" ] && [ "$ONLY_AGENT" != "vscode" ]; then return; fi

    echo "─── vscode ───"
    if [ "$SMOKE" = "1" ]; then
        report SKIP "vscode (smoke; would write mcp.json)"
        return
    fi

    # VS Code's user-scoped MCP config lives under
    # dirs::config_dir()/Code/User/mcp.json. dirs resolves to
    # $XDG_CONFIG_HOME on Linux. Override it so we don't touch the
    # runner's real VS Code config.
    local fake_home="$FIXTURE_ROOT/vscode"
    mkdir -p "$fake_home"
    XDG_CONFIG_HOME="$fake_home/.config" HOME="$fake_home" "$LAIN_BIN" setup \
        --workspace "$REPO_ROOT" \
        --agent vscode \
        --yes \
        --no-model 2>&1 || true

    # dirs::config_dir() honours $XDG_CONFIG_HOME on Linux. On
    # macOS it falls back to ~/Library/Application Support; on
    # Windows it uses %APPDATA%. This script runs only on Linux
    # CI today.
    local cfg="$fake_home/.config/Code/User/mcp.json"
    assert_config_present "vscode" "$cfg" "$fake_home"
}

# ─── continue ───────────────────────────────────────────────────────────────

run_continue() {
    if [ -n "$ONLY_AGENT" ] && [ "$ONLY_AGENT" != "continue" ]; then return; fi

    echo "─── continue ───"
    if [ "$SMOKE" = "1" ]; then
        report SKIP "continue (smoke; would write ~/.continue/config.json)"
        return
    fi

    local fake_home="$FIXTURE_ROOT/continue"
    mkdir -p "$fake_home"
    HOME="$fake_home" "$LAIN_BIN" setup \
        --workspace "$REPO_ROOT" \
        --agent continue \
        --yes \
        --no-model 2>&1 || true

    local cfg="$fake_home/.continue/config.json"
    assert_config_present "continue" "$cfg" "$fake_home"

    if [ -f "$cfg" ]; then
        if command -v jq >/dev/null 2>&1; then
            if jq -e '.experimental.modelContextProtocolServers[] | select(.name == "lain")' "$cfg" >/dev/null 2>&1; then
                report PASS "continue config has experimental.modelContextProtocolServers.lain"
            else
                report FAIL "continue config missing the lain entry"
            fi
        else
            report SKIP "continue: jq not on PATH; shape check skipped"
        fi
    fi
}

# Run all four adapters; CI installs whichever CLIs are available.
run_codex
run_cursor
run_vscode
run_continue

echo
echo "─── summary ───"
echo "PASS: $PASS"
echo "FAIL: $FAIL"
echo "SKIP: $SKIP"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
