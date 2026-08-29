#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

bash "$ROOT/scripts/demo-federation-fixture.sh" "$TMP" >/dev/null

test -d "$TMP/auth-svc"   || { echo "FAIL: auth-svc dir missing"; exit 1; }
test -d "$TMP/billing-svc" || { echo "FAIL: billing-svc dir missing"; exit 1; }
test -f "$TMP/auth-svc/Cargo.toml"   || { echo "FAIL: auth-svc/Cargo.toml missing"; exit 1; }
test -f "$TMP/billing-svc/Cargo.toml" || { echo "FAIL: billing-svc/Cargo.toml missing"; exit 1; }
test -f "$TMP/repos.yaml"     || { echo "FAIL: repos.yaml missing"; exit 1; }
test -f "$TMP/workspaces.yaml" || { echo "FAIL: workspaces.yaml missing"; exit 1; }

grep -q "auth-svc"   "$TMP/repos.yaml"     || { echo "FAIL: repos.yaml missing auth-svc"; exit 1; }
grep -q "billing-svc" "$TMP/repos.yaml"    || { echo "FAIL: repos.yaml missing billing-svc"; exit 1; }
grep -q "biller-core" "$TMP/workspaces.yaml" || { echo "FAIL: workspaces.yaml missing biller-core"; exit 1; }

( cd "$TMP/auth-svc"   && test -d .git ) || { echo "FAIL: auth-svc not a git repo"; exit 1; }
( cd "$TMP/billing-svc" && test -d .git ) || { echo "FAIL: billing-svc not a git repo"; exit 1; }

# verify_token must be present in auth-svc
grep -q "fn verify_token" "$TMP/auth-svc/src/lib.rs" || { echo "FAIL: verify_token missing in auth-svc"; exit 1; }

# billing-svc must reference verify_token across the repo boundary
grep -q "verify_token" "$TMP/billing-svc/src/lib.rs" || { echo "FAIL: billing-svc does not reference verify_token"; exit 1; }

echo "OK: federation fixture smoke test passed"
