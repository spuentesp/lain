#!/usr/bin/env bash
# Builds the 2-repo federation fixture the SPA demo recording uses.
#
# Two Rust crates joined by a single repo-crossing call:
#   auth-svc::verify_token   — the only definition
#   billing-svc              — the only external caller
# So `get_cross_repo_blast_radius` for `verify_token` will report
# callers in billing-svc, which is the headline of the recording.
#
# Also writes:
#   <ROOT>/repos.yaml       — two entries, both workspace_dir
#   <ROOT>/workspaces.yaml  — one workspace `biller-core` with both members
set -eu
ROOT="${1:?usage: demo-federation-fixture.sh <dir>}"
rm -rf "$ROOT"
mkdir -p "$ROOT/auth-svc/src"   "$ROOT/billing-svc/src"

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
EOF

cat > "$ROOT/billing-svc/src/lib.rs" <<'EOF'
// Crosses the repo boundary: the only external caller of
// `auth_svc::verify_token`. The recording's blast-radius query uses
// this dependency to produce a multi-repo answer.
pub fn charge_invoice(invoice_id: &str, token: &str) -> Result<u64, String> {
    if !verify_token_bridge(token) {
        return Err("unauthorized".into());
    }
    Ok(invoice_id.len() as u64)
}

fn verify_token_bridge(token: &str) -> bool {
    // In a real codebase this would be `auth_svc::verify_token`; for
    // the fixture the indexer only needs the symbol name to appear in
    // the source so cross-repo edges resolve. The recording doesn't
    // execute the code, it just queries the graph.
    token.len() >= 8 && !token.is_empty()
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
