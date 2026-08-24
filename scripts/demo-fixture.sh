#!/usr/bin/env bash
# Builds the synthetic repo the demo asserts against.
#
# Every fact the harness checks is a property of THIS tree, worked out
# by reading it, not by asking lain. That is the whole point: a demo
# that asserts "the server returned something" proves only that the
# server returns something.
#
# Ground truth (kept in sync with scripts/demo.sh):
#   entry()        src/lib.rs      calls orchestrate()
#   orchestrate()  src/core.rs     calls helper_a, helper_b, helper_c   <- the hub
#   helper_a/b/c   src/helpers.rs  leaves, called only by orchestrate()
#   never_called() src/dead.rs     called by nothing            <- the only dead symbol
#   parse()        src/dup.rs      name shared with src/other.rs <- ambiguity case
#   parse()        src/other.rs
#   test_entry()   tests/basic.rs  a test: must NOT count as dead
set -eu
ROOT="${1:?usage: demo-fixture.sh <dir>}"
rm -rf "$ROOT"
mkdir -p "$ROOT/src" "$ROOT/tests"

cat > "$ROOT/Cargo.toml" <<'EOF'
[package]
name = "demo_subject"
version = "0.1.0"
edition = "2021"
EOF

cat > "$ROOT/src/lib.rs" <<'EOF'
pub mod core;
pub mod helpers;
pub mod dead;
pub mod dup;
pub mod other;

/// The only caller of `orchestrate`.
pub fn entry() -> u32 {
    core::orchestrate()
}
EOF

cat > "$ROOT/src/core.rs" <<'EOF'
use crate::helpers;

/// Orchestration hub: called by one, coordinates three, real body.
/// `find_anchors` is specified to rank this kind of function above a
/// widely-called one-line helper.
pub fn orchestrate() -> u32 {
    let a = helpers::helper_a(1);
    let b = helpers::helper_b(2);
    let c = helpers::helper_c(3);
    let mut total = a + b + c;
    total += a * 2;
    total += b * 3;
    total += c * 4;
    total
}
EOF

cat > "$ROOT/src/helpers.rs" <<'EOF'
pub fn helper_a(x: u32) -> u32 { x + 1 }
pub fn helper_b(x: u32) -> u32 { x + 2 }
pub fn helper_c(x: u32) -> u32 { x + 3 }
EOF

cat > "$ROOT/src/dead.rs" <<'EOF'
/// Referenced by nothing, anywhere. The one true dead symbol.
pub fn never_called() -> u32 {
    42
}
EOF

cat > "$ROOT/src/dup.rs" <<'EOF'
/// Shares its name with `other::parse`. Neither is called from the
/// other's file, so a bare-name lookup is genuinely ambiguous.
pub fn parse(s: &str) -> usize {
    s.len()
}
EOF

cat > "$ROOT/src/other.rs" <<'EOF'
pub fn parse(s: &str) -> usize {
    s.len() * 2
}
EOF

cat > "$ROOT/tests/basic.rs" <<'EOF'
#[test]
fn test_entry() {
    assert_eq!(demo_subject::entry(), 24);
}
EOF

cd "$ROOT"
git init -q
git add -A
git -c user.email=demo@lain -c user.name=demo commit -qm "demo subject"
# A second commit so git-backed tools (history, co-change) have something.
printf '\n// touch\n' >> src/core.rs
printf '\n// touch\n' >> src/helpers.rs
git add -A
git -c user.email=demo@lain -c user.name=demo commit -qm "touch core and helpers together"
