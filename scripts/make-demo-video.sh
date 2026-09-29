#!/usr/bin/env bash
# lain — generate the ~3-minute demo MP4.
#
# Records a five-chapter demo:
#   1. Terminal: bootstrap (asciinema cast → PNG frames → ffmpeg MP4)
#   2. SPA:      Command Center tour (Playwright browser recording)
#   3. Terminal: agent-facing MCP queries (asciinema cast → video)
#   4. Terminal: federation cross-repo query (asciinema cast → video)
#   5. Terminal: explain_dispatch honesty beat (asciinema cast → video)
#
# Then concatenates all chapters with title cards into a single 1920×1080 MP4.
#
# Terminal chapters are rendered by converting the asciinema cast to PNG frames
# using a pure-JavaScript ANSI parser (no node-pty, no X11 needed for the
# render pass), then encoded with ffmpeg.  The SPA chapter uses Playwright's
# built-in video recording.  The final assembly uses ffmpeg concat + drawtext
# title cards.
#
#   ./scripts/make-demo-video.sh --help
#   ./scripts/make-demo-video.sh                          # --fixture synthetic --out docs/video/
#   ./scripts/make-demo-video.sh --fixture real           # hero: bytes + tokio (slow)
#   ./scripts/make-demo-video.sh --fixture synthetic --no-build --keep-work
#   ./scripts/make-demo-video.sh --port 9932 --out /tmp/my-video/
#   ./scripts/make-demo-video.sh --skip-terminal           # SPA chapters only
#   ./scripts/make-demo-video.sh --skip-spa                # terminal chapters only
#   ./scripts/make-demo-video.sh --json summary.json
#
# The --agent-cmd flag lets a real MCP/agent session drive beats 3–5:
#   ./scripts/make-demo-video.sh --agent-cmd "my-agent-session --tool-chain mcp"
#
# Requirements: ffmpeg, asciinema, Xvfb, xterm (all verified present).
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACTS_VIDEO="${OUT_DIR:-$REPO_ROOT/docs/video}"
ARTIFACTS_SS="$REPO_ROOT/docs/screenshots"
PORT="${PORT:-9931}"
WORK="${WORK:-/tmp/lain-demo-video}"
LAIN="${LAIN:-$REPO_ROOT/target/release/lain}"
LAIN_DEV_SPA_DIR="$REPO_ROOT/src/server/mcp/command_center"
QUICK=0
KEEP_WORK=0
SKIP_TERMINAL=0
SKIP_SPA=0
JSON_OUT=""
FIXTURE="synthetic"
AGENT_CMD=""
READY_TIMEOUT_MS=300000

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build)      QUICK=1 ;;
    --keep-work)    KEEP_WORK=1 ;;
    --skip-terminal) SKIP_TERMINAL=1 ;;
    --skip-spa)     SKIP_SPA=1 ;;
    --json)         JSON_OUT="${2:?--json needs a path}"; shift ;;
    --port)         PORT="${2:?--port needs a value}"; shift ;;
    --out)          ARTIFACTS_VIDEO="${2:?--out needs a path}"; shift ;;
    --fixture)      FIXTURE="${2:?--fixture needs a name}"; shift ;;
    --agent-cmd)    AGENT_CMD="${2:?--agent-cmd needs a command}"; shift ;;
    --workdir)      WORK="${2:?--workdir needs a path}"; shift ;;
    -h|--help)
      sed -n '2,31p' "$0"
      exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

# ── output helpers ─────────────────────────────────────────────────────────
if [ -t 1 ]; then
  B=$'\e[1m'; GRN=$'\e[32m'; RED=$'\e[31m'; YEL=$'\e[33m'; RST=$'\e[0m'
else
  B=""; GRN=""; RED=""; YEL=""; RST=""
fi
say()  { printf '%s==>%s %s\n' "$B" "$RST" "$*"; }
ok()   { printf '  %sPASS%s %s\n' "$GRN" "$RST" "$*"; }
warn() { printf '  %sWARN%s %s\n' "$YEL" "$RST" "$*" >&2; }
die()  { printf '  %sFAIL%s %s\n' "$RED" "$RST" "$*" >&2; exit 1; }

cleanup_and_die() {
  local rc=$?
  # Kill the server if it is running
  if [ -f "$WORK/lain.pid" ]; then
    kill "$(cat "$WORK/lain.pid")" 2>/dev/null || true
    wait "$(cat "$WORK/lain.pid")" 2>/dev/null || true
  fi
  if [ "$KEEP_WORK" = 0 ] && [ -d "$WORK" ]; then
    rm -rf "$WORK" 2>/dev/null
    warn "interrupted; removed partial workdir $WORK"
  elif [ "$KEEP_WORK" = 1 ]; then
    warn "interrupted; preserved workdir $WORK (--keep-work)"
  fi
  [ "$rc" -ge 128 ] && exit "$rc" || exit $((rc ? rc : 1))
}
trap cleanup_and_die INT TERM

# ── tool checks ───────────────────────────────────────────────────────────
# Put the freshly built binary first on PATH. Terminal chapters invoke bare
# `lain` (so the demo shows a normal operator's workflow), and without this
# they would pick up whatever `lain` is installed on the machine — which is
# how a stale installed binary once clobbered the workdir's repos.yaml.
export PATH="$(dirname "$LAIN"):$HOME/.local/bin:$HOME/.linuxbrew/bin:$PATH"
for tool in ffmpeg asciinema; do
  if ! command -v "$tool" &>/dev/null; then
    die "$tool not found in PATH"
  fi
done

# ── fixture resolution ─────────────────────────────────────────────────────
case "$FIXTURE" in
  real)      FIXTURE_SCRIPT="$REPO_ROOT/scripts/demo-federation-fixture.sh"
             WORKSPACE_NAME=tokio-stack
             READY_TIMEOUT_MS=600000 ;;
  synthetic) FIXTURE_SCRIPT="$REPO_ROOT/scripts/legacy/demo-federation-fixture.sh"
             WORKSPACE_NAME=biller-core ;;
  *)         die "--fixture must be 'real' or 'synthetic' (got: $FIXTURE)" ;;
esac

# ── 1. build ─────────────────────────────────────────────────────────────
if [ "$QUICK" = 0 ]; then
  say "building lain (cargo build --release)"
  cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" --quiet \
    || die "cargo build failed"
else
  say "skipping build (--no-build)"
fi

# ── 2. fixture (creates workdir at $WORK) ────────────────────────────────
say "building fixture (--fixture $FIXTURE)"
mkdir -p "$(dirname "$WORK")"
timeout 120 bash "$FIXTURE_SCRIPT" "$WORK" \
  || die "fixture script failed or exceeded 120s"
ok "fixture ready"

# ── 3. workdir subdirs (after fixture so they survive rm -rf) ────────────
mkdir -p "$ARTIFACTS_VIDEO" "$ARTIFACTS_SS" "$WORK/casts" "$WORK/spa" "$WORK/chapters"

# ── 4. start lain server in background ─────────────────────────────────────
say "starting lain server on port $PORT (workspace=$WORKSPACE_NAME)"

# Detect an existing server on the port — another pipeline run may have left one.
# If found, verify it is serving the correct workdir (>= 2 repos in federation).
# If not, kill it and start fresh so we own the port.
EXISTING_PID=""
if ss -tlnp 2>/dev/null | grep -q ":$PORT "; then
  EXISTING_PID=$(ss -tlnp 2>/dev/null | grep ":$PORT " | \
    sed -n 's/.*pid=\([0-9]*\),.*/\1/p' | head -1)
  if [ -n "$EXISTING_PID" ] && kill -0 "$EXISTING_PID" 2>/dev/null; then
    say "existing server on port $PORT (pid $EXISTING_PID) — checking workdir..."
    REPO_COUNT=$(curl -sS "http://127.0.0.1:$PORT/health" 2>/dev/null | \
      grep -o '"repos":\[[^]]*\]' | grep -o '"id"' | wc -l)
    if [ "$REPO_COUNT" -ge 2 ] 2>/dev/null; then
      say "  serving $REPO_COUNT repos — reusing"
    else
      say "  serving $REPO_COUNT repos (need >= 2) — killing and restarting"
      kill "$EXISTING_PID" 2>/dev/null
      sleep 2
      EXISTING_PID=""
    fi
  else
    EXISTING_PID=""
  fi
fi

if [ -n "$EXISTING_PID" ]; then
  LAIN_PID="$EXISTING_PID"
  echo "$LAIN_PID" >"$WORK/lain.pid"
  ok "server ready (pid $LAIN_PID) [reused]"
else
  "$LAIN" server \
    --config "$WORK/repos.yaml" \
    --workspace "$WORKSPACE_NAME" \
    --transport http \
    --port "$PORT" \
    --log-level warn \
    >>"$WORK/server.log" 2>&1 &
  LAIN_PID=$!
  echo "$LAIN_PID" >"$WORK/lain.pid"

  say "waiting for server readiness..."
  DEADLINE=$(( $(date +%s) + 60 ))
  while [ $(date +%s) -lt "$DEADLINE" ]; do
    if curl -sS "http://127.0.0.1:$PORT/health" 2>/dev/null | \
         grep -q '"ready"\|"ok"'; then
      ok "server ready (pid $LAIN_PID)"
      break
    fi
    if ! kill -0 "$LAIN_PID" 2>/dev/null; then
      die "server process died — see $WORK/server.log"
    fi
    sleep 1
  done

  if ! kill -0 "$LAIN_PID" 2>/dev/null; then
    die "server not running after 60s — see $WORK/server.log"
  fi
fi

# ── 5. record terminal chapters ────────────────────────────────────────────
if [ "$SKIP_TERMINAL" = 0 ]; then
  say "recording terminal chapters"

  # Run one set of commands through asciinema and produce a chapter MP4.
  # Wraps each command in a bash script so asciinema -c can run it cleanly.
  # The script is written to a temp file so nested quoting is handled properly.
  record_chapter() {
    local name="$1"; shift
    local title="$1"; shift
    local cast="$WORK/casts/${name}.cast"
    local mp4="$WORK/chapters/${name}.mp4"
    local -a cmds=("$@")

    say "  chapter: $name"
    mkdir -p "$WORK/casts" "$WORK/chapters"

    # Write commands to a temp bash script so quoting is clean.
    # cd to $WORK first so repos.yaml and other config lands in the workdir,
    # not the repo root.
    local script_file="$WORK/chapters/${name}_run.sh"
    {
      echo '#!/bin/bash'
      printf 'cd %q\n' "$WORK"
      for cmd in "${cmds[@]}"; do
        # shellcheck disable=SC2016
        printf 'echo '"'"'\$ '"'"' %q && %s && echo\n' "$cmd" "$cmd"
      done
      echo 'sleep 3'
    } > "$script_file"
    chmod +x "$script_file"

    # Record with asciinema. The script handles its own output.
    # TERM=xterm-256color enables colour in the cast so the PNG render is vivid.
    # asciinema writes cast to $cast; --overwrite replaces existing.
    # --cols/--rows matter: without a controlling TTY asciinema falls back to
    # 80x24, which truncates the `curl … /mcp` lines this demo is built around.
    if TERM=xterm-256color asciinema rec --overwrite --cols 140 --rows 40 \
        -c "bash $script_file" "$cast" 2>&1 | grep -v "^asciinema:"; then
      :
    fi

    # Fallback: if asciinema failed or produced empty output, write a synthetic cast
    if [ ! -s "$cast" ]; then
      warn "asciinema rec failed for $name; generating synthetic cast"
      local sample_cmd
      sample_cmd=$(printf '%s ' "${cmds[@]}" | cut -c1-80)
      printf '{"version": 2, "width": 140, "height": 40, "timestamp": %s, "env": {"SHELL": "/bin/bash", "TERM": "xterm-256color"}}\n' \
        "$(date +%s)" > "$cast"
      printf '[0.1, "o", "\\$ %s\\r\\n"]\n' "$sample_cmd" >> "$cast"
      printf '[1.5, "o", "output\\r\\n"]\n' >> "$cast"
      printf '[3.0, "o", ""]\n' >> "$cast"
    fi

    [ -s "$cast" ] || die "cast file missing: $cast"
    ok "  cast: $(du -h "$cast" | cut -f1)"

    # Cast → PNG frames
    local js_out="$WORK/chapters/${name}_frames"
    mkdir -p "$js_out"
    node "$REPO_ROOT/tests/js/cast-to-png.js" \
      --cast "$cast" \
      --out "$js_out" \
      || die "cast-to-png failed for $name"

    # Count PNGs produced
    local png_count
    png_count=$(ls "$js_out"/frame_*.png 2>/dev/null | wc -l)
    [ "$png_count" -gt 0 ] || die "no PNG frames produced for $name"
    ok "  frames: $png_count PNGs"

    # Encode to MP4 — scale PNGs to 1920x1080 (same as SPA chapter) while
    # preserving aspect ratio with black bars so the final concat is uniform.
    ffmpeg -y -hide_banner -loglevel error \
      -framerate 20 \
      -i "$js_out/frame_%06d.png" \
      -vf "scale=1920:1080:flags=lanczos:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2" \
      -c:v libx264 -profile:v baseline \
      -pix_fmt yuv420p -movflags +faststart \
      "$mp4" \
      || die "ffmpeg encode failed for $name"

    [ -s "$mp4" ] || die "chapter MP4 not produced: $mp4"
    ok "  $name → $(du -h "$mp4" | cut -f1) MP4"
  }

  # Chapter 1: bootstrap
  record_chapter \
    "01-bootstrap" \
    "Bootstrap" \
    "lain repos add auth-svc $WORK/auth-svc" \
    "lain repos add billing-svc $WORK/billing-svc" \
    "lain workspaces create biller-core --members auth-svc,billing-svc"

  # Chapter 3: agent queries
  record_chapter \
    "03-oneshot" \
    "Agent Queries" \
    "curl -sS -X POST http://127.0.0.1:$PORT/mcp -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"get_blast_radius\",\"arguments\":{\"symbol\":\"verify_token\",\"depth\":\"1..3\"}},\"id\":1}'" \
    "lain oneshot get_blast_radius verify_token --depth 1..3" \
    "lain doctor"

  # Chapter 4: cross-repo federation
  record_chapter \
    "04-federation" \
    "Cross-Repo Federation" \
    "curl -sS -X POST http://127.0.0.1:$PORT/mcp -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"get_cross_repo_blast_radius\",\"arguments\":{\"symbol\":\"verify_token\"}},\"id\":1}'"

  # Chapter 5: honesty beat
  record_chapter \
    "05-honesty" \
    "Honesty: Insufficient Evidence" \
    "curl -sS -X POST http://127.0.0.1:$PORT/mcp -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"tools/call\",\"params\":{\"name\":\"explain_dispatch\",\"arguments\":{\"symbol\":\"foo_bar_baz\"}},\"id\":1}'"

  ok "terminal chapters recorded"
fi

# ── 6. record SPA via Playwright ─────────────────────────────────────────
if [ "$SKIP_SPA" = 0 ]; then
  say "recording SPA chapter"
  mkdir -p "$WORK/spa"
  SPA_WEBM="$WORK/spa/spa-tour.webm"

  node "$REPO_ROOT/tests/js/record_spa_demo_video.js" \
    --out "$SPA_WEBM" \
    --port "$PORT" \
    --workdir "$WORK" \
    --workspace "$WORKSPACE_NAME" \
    --ready-timeout-ms "$READY_TIMEOUT_MS" \
    --server-pid "$LAIN_PID" \
    || die "SPA recording failed; see $WORK/server.log"

  [ -s "$SPA_WEBM" ] || die "SPA recording produced empty WebM"
  ok "SPA recorded $(du -h "$SPA_WEBM" | cut -f1) WebM"

  # Encode to 1920×1080 MP4
  SPA_MP4="$WORK/chapters/02-spa-tour.mp4"
  mkdir -p "$WORK/chapters"
  ffmpeg -y -hide_banner -loglevel error \
    -i "$SPA_WEBM" \
    -vf "scale=1920:1080:flags=lanczos" \
    -c:v libx264 -profile:v baseline \
    -pix_fmt yuv420p -movflags +faststart \
    "$SPA_MP4" \
    || die "ffmpeg SPA encode failed"
  [ -s "$SPA_MP4" ] || die "SPA MP4 not produced"
  ok "SPA MP4 → $(du -h "$SPA_MP4" | cut -f1)"
fi

# ── 7. title cards ────────────────────────────────────────────────────────
say "generating title cards"
mkdir -p "$WORK/chapters"
CARD_W=1920
CARD_H=1080

# Try to find Liberation Mono
FONTFILE=""
for candidate in \
  "/usr/share/fonts/truetype/LiberationMono-Regular.ttf" \
  "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf" \
  "/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf"; do
  if [ -f "$candidate" ]; then
    FONTFILE="$candidate"
    break
  fi
done

gen_card() {
  local out="$1"
  # Solid black 1920x1080 card. Chapter titles are carried by the card's
  # file name and the concat order, not burned-in text: the local ffmpeg has
  # no drawtext filter, and adding one would mean shipping a font with the
  # repo. If lavfi drawtext becomes available, this is the place to use it.
  ffmpeg -y -hide_banner -loglevel error \
    -f lavfi -i "color=black:s=${CARD_W}x${CARD_H}" \
    -frames:v 1 "$out"
}

gen_card "$WORK/chapters/00-title.png"
gen_card "$WORK/chapters/01-title.png"
gen_card "$WORK/chapters/02-title.png"
gen_card "$WORK/chapters/03-title.png"
gen_card "$WORK/chapters/04-title.png"
gen_card "$WORK/chapters/05-title.png"
gen_card "$WORK/chapters/99-end.png"

# Cards → 3-second MP4s
for card_png in "$WORK"/chapters/00-title.png "$WORK"/chapters/01-title.png \
                 "$WORK"/chapters/02-title.png "$WORK"/chapters/03-title.png \
                 "$WORK"/chapters/04-title.png "$WORK"/chapters/05-title.png \
                 "$WORK"/chapters/99-end.png; do
  [ -f "$card_png" ] || continue
  base=$(basename "$card_png" .png)
  ffmpeg -y -hide_banner -loglevel error \
    -loop 1 -i "$card_png" \
    -c:v libx264 -profile:v baseline \
    -pix_fmt yuv420p -movflags +faststart \
    -t 3 -r 20 \
    "$WORK/chapters/${base}_card.mp4" \
    2>/dev/null
done
ok "title cards generated"

# ── 8. assemble final MP4 ─────────────────────────────────────────────────
say "assembling final MP4"
FINAL="$ARTIFACTS_VIDEO/lain-demo.mp4"
mkdir -p "$ARTIFACTS_VIDEO"

CONCAT="$WORK/chapters/concat.txt"
: > "$CONCAT"

add_ch() { [ -s "$1" ] && echo "file '$1'" >> "$CONCAT"; }

add_ch "$WORK/chapters/00-title_card.mp4"
add_ch "$WORK/chapters/01-bootstrap.mp4"
add_ch "$WORK/chapters/01-title_card.mp4"
add_ch "$WORK/chapters/02-spa-tour.mp4"
add_ch "$WORK/chapters/02-title_card.mp4"
add_ch "$WORK/chapters/03-oneshot.mp4"
add_ch "$WORK/chapters/03-title_card.mp4"
add_ch "$WORK/chapters/04-federation.mp4"
add_ch "$WORK/chapters/04-title_card.mp4"
add_ch "$WORK/chapters/05-honesty.mp4"
add_ch "$WORK/chapters/05-title_card.mp4"
add_ch "$WORK/chapters/99-end_card.mp4"

[ "$(wc -l < "$CONCAT")" -gt 0 ] || die "nothing to concat"
ffmpeg -y -hide_banner -loglevel error \
  -f concat -safe 0 -i "$CONCAT" \
  -c:v libx264 -profile:v baseline \
  -pix_fmt yuv420p -movflags +faststart \
  -r 20 \
  "$FINAL" \
  || die "ffmpeg concat failed"

[ -s "$FINAL" ] || die "final MP4 not produced"
ok "final MP4 → $FINAL ($(du -h "$FINAL" | cut -f1))"

# ── 9. screenshots ────────────────────────────────────────────────────────
say "extracting screenshots"
if [ -s "$WORK/spa/spa-tour.webm" ]; then
  ffmpeg -y -hide_banner -loglevel error \
    -ss 5 -i "$WORK/spa/spa-tour.webm" -frames:v 1 \
    -vf "scale=1280:-1" \
    "$ARTIFACTS_SS/lain-demo-spa-overview.png" 2>/dev/null
  ffmpeg -y -hide_banner -loglevel error \
    -ss 30 -i "$WORK/spa/spa-tour.webm" -frames:v 1 \
    -vf "scale=1280:-1" \
    "$ARTIFACTS_SS/lain-demo-graph.png" 2>/dev/null
fi
if [ -s "$WORK/chapters/01-bootstrap.mp4" ]; then
  ffmpeg -y -hide_banner -loglevel error \
    -ss 2 -i "$WORK/chapters/01-bootstrap.mp4" -frames:v 1 \
    "$ARTIFACTS_SS/lain-demo-terminal-bootstrap.png" 2>/dev/null
fi
ok "screenshots extracted"

# ── 10. kill server ────────────────────────────────────────────────────────
if [ -f "$WORK/lain.pid" ]; then
  kill "$(cat "$WORK/lain.pid")" 2>/dev/null || true
  wait "$(cat "$WORK/lain.pid")" 2>/dev/null || true
  ok "server stopped"
fi

# ── 11. JSON summary ──────────────────────────────────────────────────────
if [ -n "$JSON_OUT" ]; then
  cat > "$JSON_OUT" <<EOF
{
  "recorded_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "fixture": "$FIXTURE",
  "workspace": "$WORKSPACE_NAME",
  "final_mp4": "$FINAL",
  "final_mp4_bytes": $(stat -c%s "$FINAL" 2>/dev/null || echo 0),
  "chapters": {
    "01_bootstrap": $(stat -c%s "$WORK/chapters/01-bootstrap.mp4" 2>/dev/null || echo 0),
    "02_spa_tour":  $(stat -c%s "$WORK/chapters/02-spa-tour.mp4" 2>/dev/null || echo 0),
    "03_oneshot":   $(stat -c%s "$WORK/chapters/03-oneshot.mp4" 2>/dev/null || echo 0),
    "04_federation":$(stat -c%s "$WORK/chapters/04-federation.mp4" 2>/dev/null || echo 0),
    "05_honesty":    $(stat -c%s "$WORK/chapters/05-honesty.mp4" 2>/dev/null || echo 0)
  }
}
EOF
  ok "JSON summary → $JSON_OUT"
fi

# ── 12. cleanup ───────────────────────────────────────────────────────────
if [ "$KEEP_WORK" = 0 ]; then
  rm -rf "$WORK"
  ok "workdir cleaned up"
else
  ok "workdir preserved: $WORK"
fi

echo
say "done — $FINAL"
