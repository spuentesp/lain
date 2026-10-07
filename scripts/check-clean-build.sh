#!/usr/bin/env bash
# Build from a pristine export of a tree-ish (default: HEAD), so a commit
# that stages half of a coupled change cannot land.
#
# Catches what `scripts/check-mod-resolution.sh` structurally cannot:
# that check sees untracked `.rs` files, but not a tracked file whose
# *symbols* live in an uncommitted one. Real incident: `diff.rs` was
# committed referencing `ContractKey::WebSocket` / `Table` that only
# existed in the uncommitted `model.rs`, so HEAD did not compile and the
# branch was bisect-broken.
#
# Usage:
#   scripts/check-clean-build.sh              # check HEAD
#   scripts/check-clean-build.sh <tree-ish>   # check any tree-ish
set -euo pipefail

tree_ish=${1:-HEAD}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

git archive "$tree_ish" | tar -x -C "$tmp"

echo "checking that $tree_ish builds from a clean export ..."
if ! (cd "$tmp" && cargo check --all-targets --quiet); then
    cat >&2 <<EOF

FAIL: $tree_ish does not build from a clean export.

A tracked file is referencing something that is not in the commit —
most often a \`mod\` file or a type that is still untracked in the
working tree. Run \`scripts/check-mod-resolution.sh\` to see untracked
source files, and make sure coupled changes land in the same commit.
EOF
    exit 1
fi

echo "clean $tree_ish builds"
