#!/usr/bin/env bash
# lain — smoke test the npm-shim package without publishing it.
#
# Mirrors the `publish-npm` job in `.github/workflows/release.yml`
# locally, so a regression in the publish path is caught at PR
# time rather than at release time. Runs four assertions:
#
#   1. `npm pack --dry-run` includes `bin/lain.js`, `scripts/runtime.js`,
#      and `scripts/install.js` (so the published tarball can install).
#   2. The bundled `bin/lain.js` calls `ensureBinary()` from
#      `scripts/runtime.js`. Catches accidental reverts to the
#      pre-`609f8db` stub that prints "Lain binary not found" and
#      exits 1 on a clean machine.
#   3. The bundled `npm-shim/package.json` `version` matches
#      `Cargo.toml`'s `version`. Catches the same drift the v0.7.0
#      / 0.6.1 incident caught at release time, but earlier.
#   4. The launcher in this tree and the launcher in the dry-run
#      tarball list match byte-for-byte (i.e. `npm pack` is reading
#      the same files we are).
#
# Exit non-zero on any failure. Run from the repo root:
#
#   ./scripts/smoke-npm-publish.sh
#
# The script does NOT publish anything. It is local-only.

set -uo pipefail

# Resolve paths from the script's own location by default, but
# allow callers (notably the unit tests in
# scripts/test_smoke_npm_publish.py) to override the working tree
# via the LAIN_REPO_ROOT env var. Tests need this so they can run
# the script against synthetic npm-shim trees without the script
# silently falling back to the real one and green-lighting a
# regression.
REPO_ROOT="${LAIN_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$REPO_ROOT" || exit 1

say()  { printf '==> %s\n' "$*"; }
ok()   { printf '  PASS %s\n' "$*"; }
die()  { printf '  FAIL %s\n' "$*" >&2; exit 1; }

[ -d npm-shim ] || die "npm-shim/ missing — run from the repo root"
[ -f npm-shim/bin/lain.js ] || die "npm-shim/bin/lain.js missing"
[ -f npm-shim/scripts/runtime.js ] || die "npm-shim/scripts/runtime.js missing"
[ -f npm-shim/scripts/install.js ] || die "npm-shim/scripts/install.js missing"
[ -f Cargo.toml ] || die "Cargo.toml missing"

# (1) `npm pack --dry-run` includes the three required files.
say "Asserting npm pack --dry-run includes bin/lain.js, scripts/runtime.js, scripts/install.js"
DRY_RUN_JSON="$(cd npm-shim && npm pack --dry-run --json 2>/dev/null)"
# Some npm versions print the human-readable file list to stderr; try both.
DRY_RUN_HUMAN="$(cd npm-shim && npm pack --dry-run 2>&1 || true)"

has_file() {
    local needle="$1"
    if printf '%s' "$DRY_RUN_JSON" | grep -q "\"$needle\""; then
        return 0
    fi
    if printf '%s' "$DRY_RUN_HUMAN" | grep -qE "(^|/)($needle)$"; then
        return 0
    fi
    return 1
}

for f in bin/lain.js scripts/runtime.js scripts/install.js; do
    if has_file "$f"; then
        ok "tarball includes $f"
    else
        die "tarball does NOT include $f — the postinstall will fail on a clean machine"
    fi
done

# (2) The bundled bin/lain.js calls ensureBinary(). A future revert
# of the post-`609f8db` launcher to the pre-rewrite stub would fail
# this assertion in CI before a release PR lands.
say "Asserting bundled bin/lain.js calls ensureBinary()"
if grep -qE "require\(.*runtime.*\)" npm-shim/bin/lain.js && \
   grep -q "ensureBinary" npm-shim/bin/lain.js; then
    ok "bin/lain.js imports runtime and calls ensureBinary"
else
    die "bin/lain.js does NOT import runtime or call ensureBinary — looks like the pre-609f8db stub"
fi

# (3) npm-shim/package.json version matches Cargo.toml version.
say "Asserting npm-shim/package.json version matches Cargo.toml"
CARGO_VERSION="$(grep -E '^version = ' Cargo.toml | head -1 | sed -E 's/^version = "([^"]+)".*/\1/')"
NPM_VERSION="$(grep -E '"version"' npm-shim/package.json | head -1 | sed -E 's/.*"version":[[:space:]]*"([^"]+)".*/\1/')"
if [ -z "$CARGO_VERSION" ] || [ -z "$NPM_VERSION" ]; then
    die "could not parse versions: cargo='$CARGO_VERSION' npm='$NPM_VERSION'"
fi
if [ "$CARGO_VERSION" = "$NPM_VERSION" ]; then
    ok "versions match: $CARGO_VERSION"
else
    die "version drift: Cargo.toml=$CARGO_VERSION npm-shim/package.json=$NPM_VERSION"
fi

# (4) `npm pack` is reading the same files we are. Re-extract the
# dry-run into a tempdir and compare the launcher bytes against
# the in-tree copy. A different launcher would mean npm is
# publishing files from somewhere else (the registry cache, an
# old clone, an env override).
say "Asserting npm pack reads the same bin/lain.js we are reading"
TMPDIR_OUT="$(mktemp -d)"
mkdir -p "$TMPDIR_OUT"
trap 'rm -rf "$TMPDIR_OUT"' EXIT
# `npm pack` writes the tarball into the current directory and prints
# its absolute path to stdout. Older npm accepted
# `--pack-destination <dir>` but it required the dir to already exist
# and silently no-op'd in some configurations; capturing stdout is
# portable across npm 8/9/10/11.
RELATIVE_PATH="$(cd npm-shim && npm pack --silent 2>/dev/null | tail -1 | tr -d '[:space:]')"
TARBALL_PATH="$REPO_ROOT/npm-shim/$RELATIVE_PATH"
if [ -z "$RELATIVE_PATH" ] || [ ! -f "$TARBALL_PATH" ]; then
    die "npm pack did not produce a tarball (got: '$RELATIVE_PATH') — is npm installed and on PATH?"
fi
mv "$TARBALL_PATH" "$TMPDIR_OUT/"
TARBALL="$TMPDIR_OUT/$RELATIVE_PATH"
LAUNCHER_IN_TARBALL="$(tar -xOf "$TARBALL" package/bin/lain.js 2>/dev/null || true)"
LAUNCHER_IN_TREE="$(cat npm-shim/bin/lain.js)"
if [ -n "$LAUNCHER_IN_TARBALL" ] && [ "$LAUNCHER_IN_TARBALL" = "$LAUNCHER_IN_TREE" ]; then
    ok "bundled bin/lain.js matches the in-tree file byte-for-byte"
else
    die "bundled bin/lain.js differs from the in-tree file — npm pack is reading from a different source"
fi

say "All checks passed."
