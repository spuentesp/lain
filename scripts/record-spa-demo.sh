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

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build)    QUICK=1 ;;
    --allow-stale) ALLOW_STALE=1 ;;
    --keep-work)   KEEP_WORK=1 ;;
    --json)        JSON_OUT="${2:?--json needs a path}"; shift ;;
    --port)        PORT="${2:?--port needs a value}"; shift ;;
    -h|--help)
      sed -n '2,16p' "$0"
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

# ── 3. record WebM ──────────────────────────────────────────────────────
RAW_WEBM="$WORK/raw.webm"
say "recording SPA demo (port $PORT, workdir $WORK)"
LAIN_BIN="$LAIN" \
RECORD_KEEP_DIR="$KEEP_WORK" \
  node "$REPO_ROOT/tests/js/record_spa_demo.js" \
    --out "$RAW_WEBM" --port "$PORT" --workdir "$WORK" \
    || die "recording failed; inspect $WORK/server.log or rerun with --keep-work"

[ -s "$RAW_WEBM" ] || die "recording produced empty WebM at $RAW_WEBM"
ok "recorded $(du -h "$RAW_WEBM" | cut -f1) WebM"

# ── 4. encode MP4 (H.264 baseline, faststart) ───────────────────────────
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

# ── 5. encode GIF (palettegen + paletteuse) ─────────────────────────────
GIF="$ARTIFACTS/spa-demo.gif"
encode_gif() {
  local fps="$1"
  say "encoding GIF (fps=$fps, palettegen)"
  ffmpeg -y -hide_banner -loglevel error \
    -i "$RAW_WEBM" \
    -vf "fps=$fps,scale=960:-1:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
    "$GIF" \
    || die "ffmpeg GIF encode failed at fps=$fps"
}

encode_gif 20
gif_bytes=$(stat -c%s "$GIF")
gif_mb=$(( gif_bytes / 1024 / 1024 ))
if [ "$gif_mb" -gt 8 ]; then
  warn "GIF is ${gif_mb}MB (>8MB target), retrying at fps=12"
  encode_gif 12
  gif_bytes=$(stat -c%s "$GIF")
  gif_mb=$(( gif_bytes / 1024 / 1024 ))
fi
if [ "$gif_mb" -gt 12 ]; then
  die "GIF is ${gif_mb}MB (>12MB hard cap); reduce content or use a smaller viewport"
fi
ok "wrote ${gif_mb}MB GIF → $GIF"

# ── 6. extract poster PNG (frame at 2s in, when the SPA is visible) ────
POSTER="$ARTIFACTS/spa-demo-poster.png"
say "extracting poster PNG"
ffmpeg -y -hide_banner -loglevel error \
  -ss 2 -i "$RAW_WEBM" -frames:v 1 -vf "scale=1280:-1" \
  "$POSTER" \
  || die "ffmpeg poster extract failed"
[ -s "$POSTER" ] || die "poster not produced"
poster_kb=$(( $(stat -c%s "$POSTER") / 1024 ))
[ "$poster_kb" -le 200 ] || warn "poster is ${poster_kb}KB (>200KB target)"
ok "wrote ${poster_kb}KB poster → $POSTER"

# ── 7. archive the raw WebM for future re-encoding without re-recording
cp "$RAW_WEBM" "$ARTIFACTS/spa-demo.webm"
webm_bytes=$(stat -c%s "$ARTIFACTS/spa-demo.webm")
webm_mb=$(( webm_bytes / 1024 / 1024 ))
[ "$webm_mb" -le 5 ] || warn "WebM is ${webm_mb}MB (>5MB target); consider lowering recording bitrate"
ok "archived ${webm_mb}MB WebM → $ARTIFACTS/spa-demo.webm"

# ── 8. optional JSON summary ────────────────────────────────────────────
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

# ── 9. cleanup ──────────────────────────────────────────────────────────
if [ "$KEEP_WORK" = 0 ]; then
  rm -rf "$WORK"
else
  echo "  workdir preserved: $WORK"
fi

echo
say "done"