#!/usr/bin/env bash
# Builds the federation fixture the SPA demo recording runs against.
#
# Two well-known Rust open-source repos joined by a real production
# dependency (`tokio` depends on `bytes`):
#   - https://github.com/tokio-rs/bytes  (id: bytes)
#   - https://github.com/tokio-rs/tokio  (id: tokio)
#
# A `--filter=blob:none --depth=1` clone keeps the working tree populated
# for the indexer (tree-sitter walks files on disk) without dragging down
# the full history. A stamp file per repo makes re-runs free.
#
# Writes:
#   $ROOT/repos.yaml        — two `shallow_clone` entries
#   $ROOT/workspaces.yaml   — one workspace `tokio-stack` with both members
#
# Exits non-zero on any failure. NO synthetic fallback — the recording is
# only useful against real data.
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

for entry in "${REPOS[@]}"; do
  set -- $entry     # id url
  id="$1"; url="$2"
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
    set -- $entry      # id url
    id="$1"; url="$2"
    ref="$(git ls-remote --symref "$url" HEAD 2>/dev/null \
        | awk '/^ref:/{sub("refs\/heads\/",""); print $2; exit}')"
    [ -n "$ref" ] || ref="master"
    echo "  - id: $id"
    echo "    source:"
    echo "      type: shallow_clone"
    echo "      url: $url"
    echo "      ref: $ref"
  done
} > "$REPOS_YAML"

cat > "$ROOT/workspaces.yaml" <<'EOF'
workspaces:
  - name: tokio-stack
    members: [bytes, tokio]
EOF

printf '  fixture: %s ready\n' "$ROOT"
