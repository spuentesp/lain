# SPA demo recording + README/QUICKSTART/Command-Center docs polish

Date: 2026-08-29
Status: design (approved verbally)
Owner: docs + recording pipeline, no SPA code changes

## Problem

The README is 557 lines and the first thing a reader sees is a single static
PNG that captures one tab at one moment. Install and federation material is
duplicated between the README and `docs/QUICKSTART.md`. New operators cannot
see what the Command Center actually feels like before installing anything.

We want a ~45-second hero video of the Command Center running against a
real federation (terminal boot + every tab + a real cross-repo query), embedded
at the top of the README, with the README and Command Center docs reorganized
so the video carries the reader into the docs instead of being decoration.

## Scope

In scope:

- New `scripts/record-spa-demo.sh` + `tests/js/record_spa_demo.js` that
  record the SPA via Playwright + system Chromium and emit WebM, MP4, GIF,
  and a poster PNG under `docs/screenshots/`.
- New `scripts/demo-federation-fixture.sh` that builds the 2-repo fixture
  used by the recording.
- README restructure: hero video block, three-step "Quick Start" pointer
  flow, "Where to go next" block at the bottom of Key Features, regenerate
  footnote.
- `docs/QUICKSTART.md` restructure: same 5-minute arc, the federation
  example that USED to live in the README, first-query curl for both
  single-repo and federation modes, a "Watch it in action" video block.
- `docs/command-center.md` restructure: tour section that names what each
  step of the video shows, per-tab hint lines that reference the tour.
- `Makefile` and `tests/js/package.json` wires for `make record-demo`.

Out of scope:

- No code changes to `src/server/mcp/command_center/**`. Recording only
  *uses* the SPA.
- No edits to `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`,
  `docs/TECHNICAL.md`, `docs/FEDERATION.md`, `docs/hooks.md`,
  `docs/hot-reload.md`, `docs/multiplayer.md`, `docs/query-language.md`,
  `docs/quickstart-tools.md`, `docs/REPOS_YAML.md`, `docs/INDEX.md`,
  `docs/CI.md`, `docs/opinions/`, `docs/srs/`, `docs/wish-list.md`.
- No new top-level docs (no `TUTORIAL.md`, no `demos/`).
- No edits to existing static screenshots under `docs/screenshots/`.
- No CI workflow changes; recording is on-demand only.
- No YouTube, no animated README badge, no gifhost.

## Recording pipeline

### What gets recorded

A ~45 second, dark-phosphor-theme clip:

1. Terminal: `lain repos add auth-svc …`, `lain repos add billing-svc …`,
   `lain workspaces create biller-core --members auth-svc,billing-svc`,
   `lain server --config ./repos.yaml --transport http --port 9931`. Server
   log shows the federation reaching `ready`.
2. Browser opens `http://localhost:9931/` → **Overview** tab lights up.
3. Click **Repos** → table shows `auth-svc (ready)` / `billing-svc (ready)`.
4. Click **Query** → pick repo `auth-svc`, `find` op, type `Function`,
   limit 50, click Run. JSON renders below.
5. Click **Tools** → pick `get_cross_repo_blast_radius`, enter
   `verify_token`, depth `1..3`, click Call. Result pane shows the
   cross-repo call chain.
6. Click **Graph** → D3 force layout settles, hover a node to show the
   tooltip.

No narration overlay. The footer status bar tells the viewer what's
happening.

### Fixture

`scripts/demo-federation-fixture.sh` writes two Rust crates to a temp
directory:

- `auth-svc` (re-uses the existing `scripts/demo-fixture.sh` shape):
  contains a `verify_token` function.
- `billing-svc`: contains a caller of `auth_svc::verify_token`, so a
  `get_cross_repo_blast_radius` query for `verify_token` crosses the
  repo boundary.

The fixture writes a `repos.yaml` (two entries) and a `workspaces.yaml`
(biller-core membership) next to it. The recording driver passes both
paths to `lain server`.

### Driver

New file `tests/js/record_spa_demo.js`. Patterns reused from the existing
`tests/js/spa_e2e.test.js`:

- Launch Chromium from `/usr/bin/chromium` (`executablePath`).
- Launch `lain server` in a child process against the temp fixture.
- Wait for the `/health` endpoint to return 200 with a body containing
  `"status":"ready"` before driving the browser.
- Use `page.click('[data-tab="…"]')` for tab navigation.
- Use `page.fill('input[name=…]', value)` and `page.click('button[type=submit]')`
  for forms.
- Enable `context.recordVideo({ dir: <tmp>, size: { width: 1280, height: 800 } })`
  so Playwright writes a WebM.
- Close the page at the end so the WebM finalises; report its path.

Differences from `spa_e2e.test.js`:

- Driver exits 0 on success; the e2e test tracks pass/fail counts.
- Driver runs against the new 2-repo federation fixture, not the single
  fixture in the e2e.
- Driver waits deterministically at each step; the e2e uses asserts.
- Driver produces a single WebM; the e2e produces one screenshot per tab.

### Post-processing

`scripts/record-spa-demo.sh` orchestrates:

1. `cargo build --release --quiet` (skippable with `--no-build`).
2. `scripts/demo-federation-fixture.sh "$WORK/fixture"` to write the repos.
3. `node tests/js/record_spa_demo.js --out "$WORK/raw.webm" --port 9931`
4. `ffmpeg -y -i "$WORK/raw.webm" -c:v libx264 -profile:v baseline -movflags +faststart -pix_fmt yuv420p docs/screenshots/spa-demo.mp4`
5. `ffmpeg -y -i "$WORK/raw.webm" -vf "fps=20,scale=960:-1:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" docs/screenshots/spa-demo.gif`
6. `ffmpeg -y -i "$WORK/raw.webm" -frames:v 1 -ss 2 -vf "scale=1280:-1" docs/screenshots/spa-demo-poster.png` (the first frame at 2 s in, when the browser is visible).

The script accepts `--no-build`, `--json <results-path>` for parity with
`scripts/demo.sh`, `--allow-stale` to skip the binary-freshness check,
`--port <port>` for a custom port, and `--keep-work` to leave the temp
dir for inspection. Exit code is 0 iff every step succeeds.

The script copies the WebM to `docs/screenshots/spa-demo.webm` so a
re-encoding pass can be re-run without re-recording the SPA.

### Artifact budget

| File | Cap |
|---|---|
| `docs/screenshots/spa-demo.webm` | 5 MB |
| `docs/screenshots/spa-demo.mp4` | 4 MB |
| `docs/screenshots/spa-demo.gif` | 8 MB |
| `docs/screenshots/spa-demo-poster.png` | 200 KB |

If the GIF exceeds the cap the driver retries at fps=12 and writes a
warning. Hard cap is 12 MB.

### Tooling wires

`Makefile` gains:

```make
.PHONY: record-demo
record-demo:
	./scripts/record-spa-demo.sh
```

`tests/js/package.json` gains:

```json
"scripts": {
  "test:e2e":     "node ./spa_e2e.test.js",
  "record-demo":  "node ./record_spa_demo.js",
  "test":         "node --test ./graph_tab.test.js"
}
```

## README restructure

### New top-to-bottom flow (target ~470 lines, down from 557)

```
# LAIN-mcp
> One-paragraph tagline (existing, lightly tightened).

## See it run (NEW — the hero)
![LAIN Command Center demo](docs/screenshots/spa-demo.gif)

**Watch in HD** ([MP4](docs/screenshots/spa-demo.mp4), [WebM](docs/screenshots/spa-demo.webm)) — the GIF above is the universal preview; the MP4 is sharper and ~3× smaller.

Caption (3 bullets):
  • Boots `lain server` against a 2-repo federation.
  • Runs a real `get_cross_repo_blast_radius` query end-to-end.
  • Shows the D3 call graph that the answer comes from.

> Note on GitHub: GitHub sanitises `<video>` tags in `README.md`, so the
> GIF is the inline preview and the MP4 is linked separately.

## How it fits together (existing mermaid, kept)

## What is Lain? (existing, kept; trimmed ~30 %)

## TL;DR — install in 30 seconds
curl … install.sh | bash
source shell, lain --version.

## Quick Start — three steps
1. Install → link to QUICKSTART §Install.
2. Configure → link to QUICKSTART §Federation.
3. Wire your agent → link to QUICKSTART §Single-repo.

## Command Center (existing, kept — link to the narrated tour in command-center.md)

## The commands (existing, kept)

## Key Features (existing sub-headings kept)

## MCP Transport Modes (existing, kept)

## Troubleshooting (existing, kept; cross-link to QUICKSTART §First aid)

## Regenerating the demo video (NEW — footnote)
One paragraph + `make record-demo`.

## Where to go next (NEW)
Operate → USER_MANUAL · Federation → FEDERATION · Tools → quickstart-tools · SPA → command-center.

## License (existing, kept)
```

### What gets deleted from the README

- The TL;DR block with the `biller` example (the multi-line `lain repos add`
  + `lain workspaces create` block) — moves to QUICKSTART §Federation.
- The **Installation** section's deep installer flags (`--yes`,
  `--download-model`, heredoc example, Homebrew, build from source) —
  collapses to a pointer at QUICKSTART §Install. Two lines survive in the
  README: the canonical curl command + "see QUICKSTART for the full
  install matrix".
- The **Multi-project** section — folds into QUICKSTART §Federation as a
  tail note.
- The README "After installation" block (reload shell, `lain --version`,
  `lain --help`) — moves verbatim to QUICKSTART §Install.

### What gets added to the README

- Hero video block at the very top (above "How it fits together").
- Footnote-style "Regenerating the demo video" section near the bottom.
- A "Where to go next" pointer block at the bottom of Key Features.

### Constraints honoured

- `tests/cli_surface.rs` keeps passing: the commands table is copied
  verbatim from the current README, not edited.
- Existing screenshot links (`docs/screenshots/command-center-overview.png`
  etc.) stay in place inside the Command Center section.
- The "How it fits together" mermaid diagram stays exactly as-is.
- No new top-level sections beyond the flow above.

### Net diff

~−90 lines (TL;DR biller example, install flag matrix, multi-project),
~+70 lines (hero block, footer, "Where to go next"), −5 lines from the
"What is Lain?" trim. Net: 557 → ~530 lines. The win is progressive
disclosure — the install/federation material stops appearing twice —
not raw line count.

## QUICKSTART.md restructure

### New flow (target ~140 lines, ~105 today but restructured)

```
# Quickstart
> Five minutes from install to first answer.

## Install  (existing block, kept as the only install detail)

## Pick a mode  (existing "Two ways to use it" table — kept, tightened)

## Single-repo (recommended default)
One paragraph + the JSON MCP config snippet.
**First query** (NEW): a single `get_blast_radius` curl you can run the
moment your agent has indexed once.

## Federation (multi-repo)
The `biller` example that USED to live in the README.
**First query** (NEW): the same `get_cross_repo_blast_radius` curl the
video shows, with a "this is what you should see in the response"
comment.

## Watch it in action (NEW)
Same GIF the README hero uses, with the same MP4 / WebM links.

## First aid  (existing — kept)

## Next  (existing — kept)
```

## command-center.md restructure

### New flow (target ~280 lines, up from 229)

```
# Command Center

> The operator's primary surface for inspecting and steering `lain server`.

[![LAIN Command Center demo](../screenshots/spa-demo.gif)](../screenshots/spa-demo.mp4)

**Watch in HD** — click the GIF to open the MP4.

## Tour (NEW — 1:1 with the video)
  1. Server boot (terminal, top of the video)
  2. Overview tab (`get_health` + `get_federation_health`)
  3. Repos tab (per-repo table)
  4. Query tab (the find-Function-with-limit-50 query)
  5. Tools tab (the `get_cross_repo_blast_radius` call against `verify_token`)
  6. Graph tab (D3 force layout, the cross-repo edge in warning colour)

Each step links to the matching section below.

## Launch  (existing — kept)

## Sections  (existing mermaid — kept)

## Tabs (existing — kept, with one small addition: a "Recorded demo"
hint at the top of each tab section that says "see step N of the tour
above for what this looks like")

## Theme  (existing — kept)

## Wire format  (existing — kept)

## Compatibility  (existing — kept)

## Source layout  (existing — kept)
```

### Cross-doc consistency

- The 3-line "what this clip shows" caption under the video is identical
  in README hero, QUICKSTART "Watch it in action", and command-center.md
  Tour intro. Three copies on purpose — each doc is read standalone.
- All three docs link to the same GIF + MP4 + WebM trio. No `<video>` tag
  is used in the README (GitHub sanitises it); the QUICKSTART and
  command-center.md docs use a clickable GIF that opens the MP4, which
  works on docs hosts that allow raw HTML.
- The mermaid diagrams in both docs are unchanged.

## Verification

Manual gates run before declaring done, in order:

1. `make record-demo` from a clean tree produces
   `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}` and exits 0.
2. `cargo build --release` succeeds; the recorded video opens the same
   SPA a fresh user gets (the served `index.html` matches HEAD).
3. `cargo test --test cli_surface` passes.
4. `node tests/js/spa_e2e.test.js` still passes.
5. `node tests/js/graph_tab.test.js` still passes.
6. Visual diff on the README in `grip` (or `mdcat`): the hero GIF
   renders inline, the MP4 / WebM / poster links resolve, the existing
   mermaid still renders, no broken in-doc anchors.
7. Link check: every `docs/…` link in README, QUICKSTART, command-center.md
   resolves. Implementation:

   ```bash
   grep -hoE 'docs/[a-zA-Z0-9_./-]+' README.md docs/QUICKSTART.md docs/command-center.md \
     | sort -u \
     | xargs -I{} test -e {}
   ```

## Risk register

| Risk | Mitigation |
|---|---|
| `lain server` boot time varies by host, makes the GIF timing inconsistent | Pin wait-at-each-step timings in `record_spa_demo.js`. Deterministic per step. |
| D3 force layout settles at different speeds on different hosts | `waitForFunction(() => graphCanvas.querySelectorAll('circle').length > N)` after the Graph tab click. Falls back to 5 s timeout. |
| ffmpeg GIF palette choice makes the dark theme banded | Use `palettegen=stats_mode=diff` + `paletteuse=dither=bayer:bayer_scale=5` — script default. |
| ONNX model absent breaks the video path | Recording does not exercise `semantic_search`. The Tools tab tour step uses `get_cross_repo_blast_radius` only. Documented in script header. |
| `tests/cli_surface.rs` table drift if the doc claims a new command | The README commands table is checked against `lain --help` by that test. We copy the table verbatim. |
| MP4 codec mismatch (GitHub preview requires H.264 baseline) | ffmpeg uses `-c:v libx264 -profile:v baseline -movflags +faststart -pix_fmt yuv420p`. |
| GIF size over budget | Hard cap 12 MB; retry at fps=12 with warning. |

## Files touched

Created:

- `scripts/record-spa-demo.sh`
- `scripts/demo-federation-fixture.sh`
- `tests/js/record_spa_demo.js`
- `docs/superpowers/specs/2026-08-29-spa-demo-recording-design.md` (this file)
- `docs/superpowers/plans/2026-08-29-spa-demo-recording-plan.md` (written by writing-plans)
- `docs/screenshots/spa-demo.webm`
- `docs/screenshots/spa-demo.mp4`
- `docs/screenshots/spa-demo.gif`
- `docs/screenshots/spa-demo-poster.png`

Modified:

- `README.md` — restructured per Section 2
- `docs/QUICKSTART.md` — restructured per Section 3
- `docs/command-center.md` — restructured per Section 3
- `Makefile` — `record-demo` target
- `tests/js/package.json` — `record-demo` npm script

Untouched (explicitly):

- `src/server/mcp/command_center/**`
- `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`, `docs/TECHNICAL.md`,
  `docs/FEDERATION.md`, `docs/hooks.md`, `docs/hot-reload.md`,
  `docs/multiplayer.md`, `docs/query-language.md`,
  `docs/quickstart-tools.md`, `docs/REPOS_YAML.md`, `docs/INDEX.md`,
  `docs/CI.md`, `docs/opinions/`, `docs/srs/`, `docs/wish-list.md`
- `docs/screenshots/command-center-{overview,repos,tools}.png`
- `.github/workflows/**` (no CI change)
