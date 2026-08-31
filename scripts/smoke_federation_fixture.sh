#!/usr/bin/env bash
# Smoke test for scripts/demo-federation-fixture.sh.
# Asserts the new federation fixture shape:
#   - $ROOT/bytes/Cargo.toml, $ROOT/tokio/Cargo.toml exist
#   - $ROOT/repos.yaml declares id: bytes and id: tokio
#   - $ROOT/workspaces.yaml declares workspace `tokio-stack` with both members
# Network-dependent: requires GitHub to be reachable (the fixture does
# `git clone --depth 1 --filter=blob:none https://github.com/tokio-rs/{bytes,tokio}.git`).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

bash "$ROOT/scripts/demo-federation-fixture.sh" "$TMP" >/dev/null

test -d "$TMP/bytes" || { echo "FAIL: bytes dir missing"; exit 1; }
test -d "$TMP/tokio" || { echo "FAIL: tokio dir missing"; exit 1; }
test -f "$TMP/bytes/Cargo.toml"  || { echo "FAIL: bytes/Cargo.toml missing"; exit 1; }
test -f "$TMP/tokio/Cargo.toml"  || { echo "FAIL: tokio/Cargo.toml missing"; exit 1; }
test -f "$TMP/repos.yaml"        || { echo "FAIL: repos.yaml missing"; exit 1; }
test -f "$TMP/workspaces.yaml"   || { echo "FAIL: workspaces.yaml missing"; exit 1; }

grep -Eq '^[[:space:]]*-?[[:space:]]*id:[[:space:]]*bytes\b'  "$TMP/repos.yaml" \
  || { echo "FAIL: repos.yaml missing id: bytes"; exit 1; }
grep -Eq '^[[:space:]]*-?[[:space:]]*id:[[:space:]]*tokio\b'  "$TMP/repos.yaml" \
  || { echo "FAIL: repos.yaml missing id: tokio"; exit 1; }
grep -q "https://github.com/tokio-rs/bytes.git"  "$TMP/repos.yaml" \
  || { echo "FAIL: repos.yaml missing bytes git url"; exit 1; }
grep -q "https://github.com/tokio-rs/tokio.git"  "$TMP/repos.yaml" \
  || { echo "FAIL: repos.yaml missing tokio git url"; exit 1; }

grep -q "tokio-stack"           "$TMP/workspaces.yaml" || { echo "FAIL: workspaces.yaml missing tokio-stack"; exit 1; }
grep -q "  - bytes\|members:.*bytes\|members:.*\[.*bytes\|^- bytes" "$TMP/workspaces.yaml" \
  || { echo "FAIL: workspaces.yaml missing bytes member"; exit 1; }
grep -q "tokio"                 "$TMP/workspaces.yaml" \
  || { echo "FAIL: workspaces.yaml missing tokio member"; exit 1; }

( cd "$TMP/bytes" && test -d .git ) || { echo "FAIL: bytes not a git repo"; exit 1; }
( cd "$TMP/tokio" && test -d .git ) || { echo "FAIL: tokio not a git repo"; exit 1; }

echo "OK: federation fixture smoke test passed"
