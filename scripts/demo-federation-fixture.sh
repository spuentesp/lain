#!/usr/bin/env bash
# Builds the federation fixture the SPA demo recording runs against.
#
# Two well-known Rust open-source repos joined by a real production
# dependency (`tokio` depends on `bytes`):
#   - https://github.com/tokio-rs/bytes  (id: bytes)
#   - https://github.com/tokio-rs/tokio  (id: tokio)
#
# Plus a third, tiny synthetic repo (id: probe) that carries exactly one
# axum route (`GET /probe`). The two real libraries expose no HTTP
# routes, topics or RPC services, so without `probe` the contract tools
# would have no endpoint to return (`get_contract` / `trace_impact`
# could only ever answer contract_not_found) and `diff_contracts` could
# never produce a change to verdict. `probe` is registered in
# repos.yaml but is NOT a member of the demo's `tokio-stack`
# workspace, so the demo's live view stays the two real repos; the
# real-federation suite boots its servers against the
# `contract-suite` workspace (bytes + tokio + probe) instead.
#
# A `--filter=blob:none --depth=1` clone keeps the working tree populated
# for the indexer (tree-sitter walks files on disk) without dragging down
# the full history. A stamp file per repo makes re-runs free.
#
# Writes:
#   $ROOT/repos.yaml        — two `shallow_clone` entries + probe + services
#   $ROOT/workspaces.yaml   — one workspace `tokio-stack` with both members
#
# Exits non-zero on any failure. The two recording repos are real data
# only — no synthetic stand-ins for them.
#
# Usage:  scripts/demo-federation-fixture.sh <dir>
set -eu

ROOT="${1:?usage: demo-federation-fixture.sh <dir>}"

REPOS=(
  "bytes https://github.com/tokio-rs/bytes.git"
  "tokio https://github.com/tokio-rs/tokio.git"
)

mkdir -p "$ROOT"

# ── clone step (idempotent: skip when the stamp file is newer than this script) ──
SCRIPT_MTIME="$(stat -c %Y "$0" 2>/dev/null || stat -f %m "$0")"

# ── synthetic probe repo (idempotent, deterministic) ─────────────────────────
# Two commits so the suite can diff them: `master` (c2) adds
# src/probe.rs with one axum route; its parent (c1) is README-only.
# Fixed author/dates keep the shas stable across rebuilds.
PROBE="$ROOT/probe"
PROBE_STAMP="$ROOT/probe.stamp"
if [ ! -d "$PROBE/.git" ] || [ ! -f "$PROBE_STAMP" ] \
   || [ "$(stat -c %Y "$PROBE_STAMP" 2>/dev/null || stat -f %m "$PROBE_STAMP")" -lt "$SCRIPT_MTIME" ]; then
  printf '  fixture: creating synthetic probe repo at %s …\n' "$PROBE"
  rm -rf "$PROBE"
  mkdir -p "$PROBE"
  git -C "$PROBE" init -q -b master
  git -C "$PROBE" config user.email "fixture@lain.local"
  git -C "$PROBE" config user.name "lain-fixture"
  printf '# lain federation probe fixture\n' > "$PROBE/README.md"
  git -C "$PROBE" add README.md
  GIT_AUTHOR_DATE="2026-10-07T00:00:00Z" GIT_COMMITTER_DATE="2026-10-07T00:00:00Z" \
    git -C "$PROBE" commit -q -m "probe: seed repo"
  mkdir -p "$PROBE/src"
  cat > "$PROBE/src/probe.rs" <<'RS'
use axum::{routing::get, Router};

async fn probe_handler() -> &'static str {
    "ok"
}

pub fn probe_app() -> Router {
    let app = Router::new()
        .route("/probe", get(probe_handler));
    app
}
RS
  git -C "$PROBE" add src/probe.rs
  GIT_AUTHOR_DATE="2026-10-07T00:00:01Z" GIT_COMMITTER_DATE="2026-10-07T00:00:01Z" \
    git -C "$PROBE" commit -q -m "probe: add GET /probe route"
  touch "$PROBE_STAMP"
fi

for entry in "${REPOS[@]}"; do
  read -r id url <<<"$entry"     # id url
  target="$ROOT/$id"
  stamp="$ROOT/$id.stamp"

  if [ -d "$target/.git" ] && [ -f "$stamp" ]; then
    stamp_mtime="$(stat -c %Y "$stamp" 2>/dev/null || stat -f %m "$stamp")"
    if [ "$stamp_mtime" -ge "$SCRIPT_MTIME" ]; then
      printf '  fixture: %s already cloned at %s — skipping\n' "$id" "$target"
      continue
    fi
  fi

  printf '  fixture: cloning %s (%s) …\n' "$id" "$url"
  rm -rf "$target"
  if ! git clone --depth 1 --filter=blob:none "$url" "$target"; then
    printf '  FAIL: git clone %s failed — is GitHub reachable?\n' "$url" >&2
    exit 1
  fi

  # Belt-and-braces: a `--filter=blob:none` clone populates enough of the
  # working tree for lain's tree-sitter pass; if the indexer logs
  # "no source files found" we can swap to a non-filtered clone. We do
  # not preemptively `checkout HEAD -- .` because that defeats the
  # filter for every file in the tree.
  touch "$stamp"
done

# ── repos.yaml + workspaces.yaml ────────────────────────────────────────────
# Autodetect each remote's default branch so we don't bake `main` into repos
# that ship on `master` (the historical Rust async ecosystem default). If the
# `git ls-remote` call fails for any reason, fall back to `master` — matches
# the two repos in this fixture and is a safer default than `main` for this
# family of repos.
REPOS_YAML="$ROOT/repos.yaml"
{
  echo "data_dir: $ROOT/.lain-data"
  echo "repos:"
  for entry in "${REPOS[@]}"; do
    read -r id url <<<"$entry"      # id url
    ref="$(git ls-remote --symref "$url" HEAD 2>/dev/null \
        | awk '/^ref:/{sub("refs/heads/",""); print $2; exit}')"
    [ -n "$ref" ] || ref="master"
    echo "  - id: $id"
    echo "    source:"
    echo "      type: shallow_clone"
    echo "      url: $url"
    echo "      ref: $ref"
  done
  # Synthetic probe repo: local path source (no network), pinned to
  # master (the commit that carries the route).
  echo "  - id: probe"
  echo "    source:"
  echo "      type: shallow_clone"
  echo "      url: $PROBE"
  echo "      ref: master"
  # Contract services (§7.1). Without these the `services` index is
  # empty and `get_service` can only answer service_not_found — the
  # names mirror the repo ids so endpoint attribution is unchanged
  # from the implicit (repo-id) naming.
  echo "services:"
  echo "  - name: bytes"
  echo "    repo: bytes"
  echo "  - name: tokio"
  echo "    repo: tokio"
  echo "  - name: probe"
  echo "    repo: probe"
} > "$REPOS_YAML"

cat > "$ROOT/workspaces.yaml" <<'EOF'
workspaces:
  - name: tokio-stack
    members: [bytes, tokio]
  - name: contract-suite
    members: [bytes, tokio, probe]
EOF

printf '  fixture: %s ready\n' "$ROOT"
