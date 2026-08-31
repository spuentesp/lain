#!/usr/bin/env bash
# Builds the 2-repo federation fixture the SPA demo recording uses.
#
# Two Rust crates joined by a single real cross-repo Calls edge:
#   auth-svc::verify_token   — the only definition
#   billing-svc              — the only external caller
#
# The fixture is a real Cargo workspace: a parent `Cargo.toml` declares
# both members and `billing-svc/Cargo.toml` has a path-dep on `auth-svc`,
# so the source of `billing-svc/src/lib.rs` contains a genuine
# `auth_svc::verify_token(...)` call. Tree-sitter / ingest see that call
# as a normal Rust function invocation and emit a cross-repo Calls edge,
# which is what `get_cross_repo_blast_radius("verify_token")` reports in
# the recording. A bare `verify_token_bridge` helper inside billing-svc
# would only produce an intra-repo edge and the blast radius would be
# empty across repos — that was the bug Task 8 caught.
#
# Also writes:
#   <ROOT>/Cargo.toml        — workspace root declaring both members
#   <ROOT>/repos.yaml        — two entries, both workspace_dir
#   <ROOT>/workspaces.yaml   — one workspace `biller-core` with both members
set -eu
ROOT="${1:?usage: demo-federation-fixture.sh <dir>}"
rm -rf "$ROOT"
mkdir -p "$ROOT/auth-svc/src"   "$ROOT/billing-svc/src"

# ── workspace root ──────────────────────────────────────────────────────
# Parent Cargo.toml so rust-analyzer / cargo treat auth-svc and
# billing-svc as one workspace. Without this, the path-dep below
# wouldn't be discoverable as a cross-workspace reference and the
# indexer would never emit a Calls edge between the two repos.
cat > "$ROOT/Cargo.toml" <<'EOF'
[workspace]
members = ["auth-svc", "billing-svc"]
resolver = "2"
EOF

# ── auth-svc ────────────────────────────────────────────────────────────
cat > "$ROOT/auth-svc/Cargo.toml" <<'EOF'
[package]
name = "auth_svc"
version = "0.1.0"
edition = "2021"
EOF

cat > "$ROOT/auth-svc/src/lib.rs" <<'EOF'
/// Validate an incoming bearer token. This is the symbol the recording
/// queries with `get_cross_repo_blast_radius`; its only external caller
/// lives in `billing-svc/src/lib.rs`, so the cross-repo edge is real.
pub fn verify_token(token: &str) -> bool {
    !token.is_empty() && token.len() >= 8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty() {
        assert!(!verify_token(""));
    }

    #[test]
    fn rejects_short() {
        assert!(!verify_token("abc"));
    }

    #[test]
    fn accepts_long_enough() {
        assert!(verify_token("abcdefgh"));
    }
}
EOF

# ── billing-svc ─────────────────────────────────────────────────────────
cat > "$ROOT/billing-svc/Cargo.toml" <<'EOF'
[package]
name = "billing_svc"
version = "0.1.0"
edition = "2021"

[dependencies]
auth_svc = { path = "../auth-svc" }
EOF

cat > "$ROOT/billing-svc/src/lib.rs" <<'EOF'
// Crosses the repo boundary: the only external caller of
// `auth_svc::verify_token`. The recording's blast-radius query uses
// this dependency to produce a multi-repo answer.
pub fn charge_invoice(invoice_id: &str, token: &str) -> Result<u64, String> {
    if !auth_svc::verify_token(token) {
        return Err("unauthorized".into());
    }
    Ok(invoice_id.len() as u64)
}
EOF

# ── git history (indexer + co-change want commits) ──────────────────────
for crate in auth-svc billing-svc; do
  cd "$ROOT/$crate"
  git init -q
  git -c user.email=demo@lain -c user.name=demo add -A
  git -c user.email=demo@lain -c user.name=demo commit -qm "initial $crate"
done

# ── repos.yaml + workspaces.yaml ────────────────────────────────────────
cat > "$ROOT/repos.yaml" <<EOF
data_dir: $ROOT/.lain-data
repos:
  - id: auth-svc
    source: { type: workspace_dir, path: $ROOT/auth-svc }
  - id: billing-svc
    source: { type: workspace_dir, path: $ROOT/billing-svc }
EOF

cat > "$ROOT/workspaces.yaml" <<'EOF'
workspaces:
  - name: biller-core
    members: [auth-svc, billing-svc]
EOF
