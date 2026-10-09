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
# One cargo run, output captured: the grep below must see it, and under
# `set -o pipefail` a `cargo | grep -q` pipeline reports cargo's failure
# status even when grep matches, which would invert the branch.
check_log="$tmp/cargo-check.log"
if ! (cd "$tmp" && cargo check --all-targets --quiet) >"$check_log" 2>&1; then
    cat "$check_log" >&2
    if grep -q "E0583\|E0432\|E0433\|E0425" "$check_log"; then
        cat >&2 <<EOF

FAIL: $tree_ish does not build from a clean export.

A tracked file is referencing something that is not in the commit —
most often a \`mod\` file or a type that is still untracked in the
working tree. Run \`scripts/check-mod-resolution.sh\` to see untracked
source files, and make sure coupled changes land in the same commit.
EOF
    else
        cat >&2 <<EOF

FAIL: $tree_ish could not be checked (toolchain/environment, not necessarily missing source).

The compiler output above carries none of the missing-source error
codes (E0583/E0432/E0433/E0425). A C toolchain failure in a cold
cargo cache (e.g. libgit2-sys/pcre2 under sccache) fails like this
and says nothing about the commit's contents.
EOF
    fi
    exit 1
fi

echo "clean $tree_ish builds"
