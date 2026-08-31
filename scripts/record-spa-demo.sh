#!/usr/bin/env bash
# lain — record the Command Center SPA demo and encode to WebM/MP4/GIF.
#
# Drives the recording pipeline (Playwright + system Chromium + ffmpeg)
# against the federation fixture. Artifacts land in docs/screenshots/,
# alongside the existing per-tab static screenshots.
#
#   ./scripts/record-spa-demo.sh                  # default port 9931
#   ./scripts/record-spa-demo.sh --no-build        # skip cargo build
#   ./scripts/record-spa-demo.sh --allow-stale     # skip binary freshness check
#   ./scripts/record-spa-demo.sh --port 9934       # custom port
#   ./scripts/record-spa-demo.sh --json out.json   # machine-readable summary
#   ./scripts/record-spa-demo.sh --keep-work       # preserve the temp workdir
#   ./scripts/record-spa-demo.sh --fixture real    # clone bytes+tokio (default)
#   ./scripts/record-spa-demo.sh --no-clone        # skip fixture build (debug)
#   ./scripts/record-spa-demo.sh --workdir <dir>   # override the temp workdir
#
# Does NOT mutate the SPA. The recording only uses it.
#
# Exits non-zero on any failure. Hard caps the GIF at 12 MB.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PORT="${PORT:-9931}"
WORK="${WORK:-/tmp/lain-record-spa-demo}"
ARTIFACTS="$REPO_ROOT/docs/screenshots"
LAIN="${LAIN:-$REPO_ROOT/target/release/lain}"
QUICK=0
ALLOW_STALE=0
KEEP_WORK=0
JSON_OUT=""
FIXTURE="real"
NO_CLONE=0

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build)    QUICK=1 ;;
    --allow-stale) ALLOW_STALE=1 ;;
    --keep-work)   KEEP_WORK=1 ;;
    --json)        JSON_OUT="${2:?--json needs a path}"; shift ;;
    --port)        PORT="${2:?--port needs a value}"; shift ;;
    --fixture)     FIXTURE="${2:?--fixture needs a name}"; shift ;;
    --no-clone)    NO_CLONE=1 ;;
    --workdir)     WORK="${2:?--workdir needs a path}"; shift ;;
    -h|--help)
      sed -n '2,19p' "$0"
      exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

# ── output ──────────────────────────────────────────────────────────────
if [ -t 1 ]; then
  B=$'\e[1m'; GRN=$'\e[32m'; RED=$'\e[31m'; YEL=$'\e[33m'; RST=$'\e[0m'
else
  B=""; GRN=""; RED=""; YEL=""; RST=""
fi
say()  { printf '%s==>%s %s\n' "$B" "$RST" "$*"; }
ok()   { printf '  %sPASS%s %s\n' "$GRN" "$RST" "$*"; }
warn() { printf '  %sWARN%s %s\n' "$YEL" "$RST" "$*" >&2; }
die()  { printf '  %sFAIL%s %s\n' "$RED" "$RST" "$*" >&2; exit 1; }

# Remove the partial workdir when we were killed mid-run, unless --keep-work.
# Bash sets $? to 128+signal_number when this fires from a signal trap
# (130 for SIGINT, 143 for SIGTERM), so we propagate that exact exit code.
cleanup_and_die() {
  local rc=$?
  if [ "$KEEP_WORK" = 0 ] && [ -d "$WORK" ]; then
    rm -rf "$WORK" 2>/dev/null
    warn "interrupted; removed partial workdir $WORK"
  elif [ "$KEEP_WORK" = 1 ]; then
    warn "interrupted; preserved workdir $WORK (--keep-work)"
  fi
  if [ "$rc" -ge 128 ]; then
    exit "$rc"
  fi
  return "$rc"
}
trap 'cleanup_and_die' INT TERM

mkdir -p "$WORK" "$ARTIFACTS"

# Resolve which fixture script to use. The `real` fixture does two
# `git clone`s against GitHub; the `synthetic` fixture is the original
# `auth-svc` + `billing-svc` two-crate pair, kept under scripts/legacy/
# for offline runs. Each fixture also writes a `workspaces.yaml` with a
# single workspace whose name the driver needs explicitly — the JS
# driver's own default is the real fixture's name (`tokio-stack`), so
# under `--fixture synthetic` we MUST pass `--workspace biller-core`,
# or the driver errors with `workspace "tokio-stack" not found`.
# `--no-clone` skips the fixture step entirely but still needs the
# workspace name baked in (the pre-populated workdir carries whichever
# fixture's `workspaces.yaml` the caller passed in via `--fixture`).
case "$FIXTURE" in
  real)      FIXTURE_SCRIPT="$REPO_ROOT/scripts/demo-federation-fixture.sh"
             WORKSPACE_NAME=tokio-stack ;;
  synthetic) FIXTURE_SCRIPT="$REPO_ROOT/scripts/legacy/demo-federation-fixture.sh"
             WORKSPACE_NAME=biller-core ;;
  *)         die "--fixture must be 'real' or 'synthetic' (got: $FIXTURE)" ;;
esac
if [ "$NO_CLONE" = 0 ]; then
  [ -x "$FIXTURE_SCRIPT" ] || die "fixture script $FIXTURE_SCRIPT is missing or not executable"
fi

# ── 1. build ────────────────────────────────────────────────────────────
if [ "$QUICK" = 0 ]; then
  say "building lain (cargo build --release)"
  cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" --quiet \
    || die "cargo build failed"
else
  say "skipping build (--no-build)"
fi

# ── 2. binary freshness check (skip on --allow-stale) ──────────────────
if [ "$ALLOW_STALE" = 0 ]; then
  if [ -n "$(find "$REPO_ROOT/src" "$REPO_ROOT/Cargo.toml" "$REPO_ROOT/Cargo.lock" \
              -newer "$LAIN" -print -quit 2>/dev/null)" ]; then
    die "binary $LAIN is older than source files; pass --allow-stale or rebuild"
  fi
fi

# ── 3. fixture (clone or pre-existing) ──────────────────────────────────
if [ "$NO_CLONE" = 0 ]; then
  say "building fixture (--fixture $FIXTURE)"
  # `timeout` returns 124 on kill and the fixture script's own exit code
  # otherwise; capture it directly because `if ! …; then rc=$?` would mask
  # the real status behind the inverted exit.
  timeout 90 bash "$FIXTURE_SCRIPT" "$WORK"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    die "fixture script failed or exceeded 90s (exit=$rc); rerun with --keep-work to inspect $WORK"
  fi
else
  say "skipping fixture (--no-clone); using existing $WORK"
fi

# ── 4. record WebM ──────────────────────────────────────────────────────
RAW_WEBM="$WORK/raw.webm"
say "recording SPA demo (port $PORT, workdir $WORK)"
LAIN_BIN="$LAIN" \
LAIN_RECORD_KEEP_DIR="$KEEP_WORK" \
  node "$REPO_ROOT/tests/js/record_spa_demo.js" \
    --out "$RAW_WEBM" --port "$PORT" --workdir "$WORK" --workspace "$WORKSPACE_NAME" \
    || die "recording failed; inspect $WORK/server.log or rerun with --keep-work"

[ -s "$RAW_WEBM" ] || die "recording produced empty WebM at $RAW_WEBM"
ok "recorded $(du -h "$RAW_WEBM" | cut -f1) WebM"

# ── 5. encode MP4 (H.264 baseline, faststart) ───────────────────────────
MP4="$ARTIFACTS/spa-demo.mp4"
say "encoding MP4"
ffmpeg -y -hide_banner -loglevel error \
  -i "$RAW_WEBM" \
  -c:v libx264 -profile:v baseline -movflags +faststart -pix_fmt yuv420p \
  "$MP4" \
  || die "ffmpeg MP4 encode failed"
[ -s "$MP4" ] || die "MP4 not produced"
mp4_bytes=$(stat -c%s "$MP4")
mp4_mb=$(( mp4_bytes / 1024 / 1024 ))
if [ "$mp4_mb" -gt 4 ]; then
  die "MP4 is ${mp4_mb}MB (>4MB hard cap); README preview will choke — re-record with a smaller viewport"
fi
ok "wrote ${mp4_mb}MB MP4 → $MP4"

# ── 6. encode GIF (palettegen + paletteuse) ─────────────────────────────
# Retry ladder: best-quality fps=20 first, then 12, 8, 6 (last also drops
# width to 800). The 12 MB hard cap is the only thing that aborts the run;
# the 8 MB target just keeps escalating to find the best fit.
GIF="$ARTIFACTS/spa-demo.gif"
encode_gif() {      # encode_gif <fps> [scale]
  local fps="$1" scale="${2:-960}"  # default width=960; "-1" preserves aspect
  say "encoding GIF (fps=$fps, width=${scale}, palettegen)"
  ffmpeg -y -hide_banner -loglevel error \
    -i "$RAW_WEBM" \
    -vf "fps=$fps,scale=${scale}:-1:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
    "$GIF" \
    || die "ffmpeg GIF encode failed at fps=$fps width=$scale"
}

# Try fps=20 (best), then 12, 8, 6 — only escalate when over budget.
encode_gif 20
gif_bytes=$(stat -c%s "$GIF"); gif_mb=$(( gif_bytes / 1024 / 1024 ))
if [ "$gif_mb" -gt 8 ]; then warn "GIF is ${gif_mb}MB (>8MB target)"; encode_gif 12
  gif_bytes=$(stat -c%s "$GIF"); gif_mb=$(( gif_bytes / 1024 / 1024 ))
fi
if [ "$gif_mb" -gt 8 ]; then warn "still ${gif_mb}MB, trying fps=8"; encode_gif 8
  gif_bytes=$(stat -c%s "$GIF"); gif_mb=$(( gif_bytes / 1024 / 1024 ))
fi
if [ "$gif_mb" -gt 8 ]; then warn "still ${gif_mb}MB, dropping to fps=6 + width=800"; encode_gif 6 800
  gif_bytes=$(stat -c%s "$GIF"); gif_mb=$(( gif_bytes / 1024 / 1024 ))
fi
# Hard cap remains 12 MB.
if [ "$gif_mb" -gt 12 ]; then
  die "GIF is ${gif_mb}MB (>12MB hard cap) even at fps=6/width=800 — pick a smaller viewport or shorter recording"
fi
ok "wrote ${gif_mb}MB GIF → $GIF"

# ── 7. extract poster PNG (frame at 2s in, when the SPA is visible) ────
POSTER="$ARTIFACTS/spa-demo-poster.png"
say "extracting poster PNG"
ffmpeg -y -hide_banner -loglevel error \
  -ss 2 -i "$RAW_WEBM" -frames:v 1 -vf "scale=1024:-1" \
  "$POSTER" \
  || die "ffmpeg poster extract failed"
[ -s "$POSTER" ] || die "poster not produced"
poster_kb=$(( $(stat -c%s "$POSTER") / 1024 ))
[ "$poster_kb" -le 200 ] || warn "poster is ${poster_kb}KB (>200KB target)"
ok "wrote ${poster_kb}KB poster → $POSTER"

# ── 8. archive the raw WebM for future re-encoding without re-recording
cp "$RAW_WEBM" "$ARTIFACTS/spa-demo.webm"
webm_bytes=$(stat -c%s "$ARTIFACTS/spa-demo.webm")
webm_mb=$(( webm_bytes / 1024 / 1024 ))
[ "$webm_mb" -le 5 ] || warn "WebM is ${webm_mb}MB (>5MB target); consider lowering recording bitrate"
ok "archived ${webm_mb}MB WebM → $ARTIFACTS/spa-demo.webm"

# ── 9. optional JSON summary ────────────────────────────────────────────
if [ -n "$JSON_OUT" ]; then
  cat > "$JSON_OUT" <<EOF
{
  "recorded_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "webm_bytes": $(stat -c%s "$ARTIFACTS/spa-demo.webm"),
  "mp4_bytes":  $(stat -c%s "$MP4"),
  "gif_bytes":  $(stat -c%s "$GIF"),
  "poster_bytes": $(stat -c%s "$POSTER")
}
EOF
  ok "wrote JSON summary → $JSON_OUT"
fi

# ── 10. cleanup ─────────────────────────────────────────────────────────
if [ "$KEEP_WORK" = 0 ]; then
  rm -rf "$WORK"
else
  echo "  workdir preserved: $WORK"
fi

echo
say "done"