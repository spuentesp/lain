# Video Demo

Generates `docs/video/lain-demo.mp4` — a ~3-minute, 1920×1080 H.264 MP4
showcasing Lain's key surfaces.

## Quick start

```bash
make demo-video          # synthetic fixture (fast, offline)
make demo-video-real     # bytes + tokio (slow, GitHub required)
```

Or invoke the script directly:

```bash
./scripts/make-demo-video.sh --help
./scripts/make-demo-video.sh --fixture synthetic --no-build --keep-work
```

## Output

| File | Description |
|---|---|
| `docs/video/lain-demo.mp4` | Final assembled MP4 |
| `docs/screenshots/lain-demo-spa-overview.png` | SPA chapter still |
| `docs/screenshots/lain-demo-terminal-bootstrap.png` | Bootstrap chapter still |

These outputs are committed deliverables, not build artifacts: `make
demo-video` regenerates them and the result is re-committed, the same way
`docs/screenshots/spa-demo.gif` already is. `docs/video/*.mp4` and
`docs/screenshots/lain-demo-*.png` are tracked on purpose and therefore stay
out of `.gitignore`.

## Chapters

| # | Title | Source |
|---|---|---|
| 1 | Bootstrap | Terminal (asciinema cast → PNG frames → ffmpeg) |
| 2 | Command Center tour | Playwright browser recording |
| 3 | Agent queries (oneshot, doctor) | Terminal |
| 4 | Cross-repo federation | Terminal |
| 5 | Honesty beat (explain_dispatch) | Terminal |

## Flags

| Flag | Default | Description |
|---|---|---|
| `--fixture real\|synthetic` | `synthetic` | `synthetic` = auth-svc+billing-svc (fast); `real` = bytes+tokio (slow) |
| `--port N` | `9931` | Port for the lain server |
| `--out DIR` | `docs/video/` | Output directory for the MP4 |
| `--workdir DIR` | `/tmp/lain-demo-video` | Working directory (fixture + temp files) |
| `--no-build` | — | Skip `cargo build --release` |
| `--keep-work` | — | Preserve the temp workdir after completion |
| `--skip-terminal` | — | Record only the SPA chapter |
| `--skip-spa` | — | Record only the terminal chapters |
| `--json FILE` | — | Write a machine-readable summary to `FILE` |
| `--agent-cmd CMD` | — | Hook for a real agent session to drive beats 3–5 |

## How it works

**Terminal chapters** — Each chapter records to an asciinema cast file, then
`tests/js/cast-to-png.js` parses the ANSI-encoded output and renders each
frame as an SVG → PNG using ImageMagick 7 (SVG+magick pipeline, no node-pty
required). ffmpeg encodes the PNG sequence to H.264 MP4.

**SPA chapter** — `tests/js/record_spa_demo_video.js` launches Chromium via
Playwright, starts the lain server against the fixture, waits for the federation
to reach `ready`, confirms cross-repo graph nodes exist, and drives five of
the six Command Center tabs (Overview → Repos → Query → Tools → Graph). Playwright's
built-in WebM video recording captures the browser session; ffmpeg encodes it
to H.264 and scales it to 1920×1080.

**Assembly** — ffmpeg concat demuxer combines all chapter MP4s with generated
title-card MP4s (solid-black 3-second clips with drawtext labels) into the
final MP4.

## Regenerating

```bash
# Fast iteration (synthetic fixture, no rebuild)
./scripts/make-demo-video.sh --fixture synthetic --no-build --keep-work

# Inspect intermediate artifacts
ls /tmp/lain-demo-video/

# Hero shot (real fixture, slow)
./scripts/make-demo-video.sh --fixture real

# Re-run after fixing the SPA
./scripts/make-demo-video.sh --no-build   # --no-build skips cargo build; LAIN_DEV_SPA_DIR picks up the live SPA
```

## Requirements

- `ffmpeg` (with libx264, libavformat)
- `asciinema` 2.x
- `Xvfb`, `xterm` (for chapter 1 terminal demo)
- `magick` (ImageMagick 7, for cast-to-png SVG rendering)
- `node` (v24), `playwright` (in `tests/js/node_modules/`)
- Chromium at `~/.cache/ms-playwright/chromium-*/chrome-linux*/chrome`
