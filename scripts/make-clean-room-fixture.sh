#!/usr/bin/env bash
# Create a tiny Rust fixture repository for Milestone 9 clean-room checks
# (docs/AGENT_UX_ROADMAP.md). One `orchestrate`/`helper` pair gives
# find_anchors something non-empty to rank, matching the fixture shape
# already used by tests/mcp_cold_start.rs.
#
# Usage: make-clean-room-fixture.sh <target-dir>
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <target-dir>" >&2
  exit 2
fi
DIR="$1"

mkdir -p "$DIR/src"
cd "$DIR"
git init -q
git config user.email "fixture@lain"
git config user.name "fixture"
cat > Cargo.toml <<'EOF'
[package]
name = "clean_room_fixture"
version = "0.1.0"
edition = "2021"
EOF
cat > src/lib.rs <<'EOF'
pub fn orchestrate() -> u32 { helper(1) }
pub fn helper(x: u32) -> u32 { x + 1 }
EOF
git add -A
git commit -q -m init
