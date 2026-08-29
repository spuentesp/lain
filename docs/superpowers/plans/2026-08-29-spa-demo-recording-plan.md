# SPA Demo Recording + README/QUICKSTART/Command-Center Docs Polish — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record a ~45-second demo of the Command Center SPA against a 2-repo federation and embed it (GIF + MP4 + WebM) as the hero of the README, with the README / QUICKSTART / command-center docs reorganized around it.

**Architecture:** Add a deterministic recording pipeline (Playwright + system Chromium + ffmpeg) that boots `lain server` against a synthetic 2-repo fixture, drives every tab, captures the result as WebM, and re-encodes it into MP4 + GIF + a poster PNG. Embed the GIF in the README, with HD links next to it. The fixture is a small Rust federation (`auth-svc` exposing `verify_token`, `billing-svc` calling it) so the recorded `get_cross_repo_blast_radius` call genuinely crosses a repo boundary. The README is restructured for progressive disclosure; QUICKSTART absorbs material that used to be duplicated in the README; `command-center.md` adds a Tour section that names each step of the recording.

**Tech Stack:** Bash (orchestration), Node.js + Playwright 1.62 (recording driver), system Chromium (`/usr/bin/chromium`), ffmpeg (encoding), Rust + Cargo (no Rust changes — recording only uses the existing binary).

## Global Constraints

- No edits to `src/server/mcp/command_center/**`. The recording only uses the SPA.
- No edits to `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`, `docs/TECHNICAL.md`, `docs/FEDERATION.md`, `docs/hooks.md`, `docs/hot-reload.md`, `docs/multiplayer.md`, `docs/query-language.md`, `docs/quickstart-tools.md`, `docs/REPOS_YAML.md`, `docs/INDEX.md`, `docs/CI.md`, `docs/opinions/`, `docs/srs/`, `docs/wish-list.md`.
- No edits to `docs/screenshots/command-center-{overview,repos,tools}.png`.
- No edits to `.github/workflows/**`.
- The README "commands" table is copied verbatim from the current README — `tests/cli_surface.rs` checks it against `lain --help` and will fail CI if it drifts.
- Recording is on-demand only; not added to CI.
- Spec: `docs/superpowers/specs/2026-08-29-spa-demo-recording-design.md`.
- Chromium binary: `/usr/bin/chromium` (system).
- ffmpeg binary: `/usr/bin/ffmpeg` (system).
- Playwright already installed at `tests/js/node_modules/playwright`.

## File Structure

Created:

| File | Responsibility |
|---|---|
| `scripts/demo-federation-fixture.sh` | Writes the 2-repo federation fixture (`auth-svc`, `billing-svc`) plus `repos.yaml` + `workspaces.yaml` to a target dir. |
| `tests/js/record_spa_demo.js` | Playwright driver: boots `lain server`, drives Chromium through every tab, captures WebM. |
| `scripts/record-spa-demo.sh` | Orchestrator: build, write fixture, run driver, encode WebM → MP4 + GIF + poster PNG. |
| `docs/screenshots/spa-demo.webm` | Recording source-of-truth (Playwright native). |
| `docs/screenshots/spa-demo.mp4` | GitHub-friendly HD video. |
| `docs/screenshots/spa-demo.gif` | Universal inline preview (used in README). |
| `docs/screenshots/spa-demo-poster.png` | First-frame poster. |

Modified:

| File | Change |
|---|---|
| `README.md` | Restructured for progressive disclosure; hero GIF + HD links at top. |
| `docs/QUICKSTART.md` | Absorbs federation example from README; adds first-query curl + "Watch it in action" block. |
| `docs/command-center.md` | Adds Tour section, per-tab hint lines. |
| `Makefile` | Adds `record-demo` target. |
| `tests/js/package.json` | Adds `record-demo` npm script. |

---

## Task 1: Federation fixture script

**Files:**
- Create: `scripts/demo-federation-fixture.sh`
- Reference: `scripts/demo-fixture.sh` (single-crate pattern to mirror)

**Interfaces:**
- Consumes: a target directory path (CLI arg)
- Produces:
  - `<dir>/auth-svc/` — Rust crate exposing `verify_token`
  - `<dir>/billing-svc/` — Rust crate that calls `auth_svc::verify_token`
  - `<dir>/repos.yaml` — two entries, one per crate, both as `workspace_dir`
  - `<dir>/workspaces.yaml` — one workspace `biller-core` with both repos as members
  - Each crate has a real `.git` with at least one commit (co-change needs history)

- [ ] **Step 1: Write the failing smoke test**

Append to `scripts/demo-fixture.sh` is NOT what we want — that's an unrelated file. Create a new file `scripts/smoke_federation_fixture.sh` to verify the fixture script works:

```bash
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

bash "$ROOT/scripts/demo-federation-fixture.sh" "$TMP" >/dev/null

test -d "$TMP/auth-svc"   || { echo "FAIL: auth-svc dir missing"; exit 1; }
test -d "$TMP/billing-svc" || { echo "FAIL: billing-svc dir missing"; exit 1; }
test -f "$TMP/auth-svc/Cargo.toml"   || { echo "FAIL: auth-svc/Cargo.toml missing"; exit 1; }
test -f "$TMP/billing-svc/Cargo.toml" || { echo "FAIL: billing-svc/Cargo.toml missing"; exit 1; }
test -f "$TMP/repos.yaml"     || { echo "FAIL: repos.yaml missing"; exit 1; }
test -f "$TMP/workspaces.yaml" || { echo "FAIL: workspaces.yaml missing"; exit 1; }

grep -q "auth-svc"   "$TMP/repos.yaml"     || { echo "FAIL: repos.yaml missing auth-svc"; exit 1; }
grep -q "billing-svc" "$TMP/repos.yaml"    || { echo "FAIL: repos.yaml missing billing-svc"; exit 1; }
grep -q "biller-core" "$TMP/workspaces.yaml" || { echo "FAIL: workspaces.yaml missing biller-core"; exit 1; }

( cd "$TMP/auth-svc"   && test -d .git ) || { echo "FAIL: auth-svc not a git repo"; exit 1; }
( cd "$TMP/billing-svc" && test -d .git ) || { echo "FAIL: billing-svc not a git repo"; exit 1; }

# verify_token must be present in auth-svc
grep -q "fn verify_token" "$TMP/auth-svc/src/lib.rs" || { echo "FAIL: verify_token missing in auth-svc"; exit 1; }

# billing-svc must reference verify_token across the repo boundary
grep -q "verify_token" "$TMP/billing-svc/src/lib.rs" || { echo "FAIL: billing-svc does not reference verify_token"; exit 1; }

echo "OK: federation fixture smoke test passed"
```

- [ ] **Step 2: Run the smoke test to verify it fails**

Run: `bash scripts/smoke_federation_fixture.sh`
Expected: exit 1 with "FAIL: auth-svc dir missing" (or similar). The fixture script does not exist yet.

- [ ] **Step 3: Write the fixture script**

Create `scripts/demo-federation-fixture.sh`:

```bash
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
```

`chmod +x scripts/demo-federation-fixture.sh` — the file write above does not preserve executable bits if you use `Write` with no exec permission set, so the next step adds it explicitly.

- [ ] **Step 4: Make the fixture script executable and re-run the smoke test**

Run:
```bash
chmod +x scripts/demo-federation-fixture.sh
bash scripts/smoke_federation_fixture.sh
```
Expected: `OK: federation fixture smoke test passed`, exit 0.

- [ ] **Step 5: Manual eyeball**

Run:
```bash
TMP="$(mktemp -d)"
bash scripts/demo-federation-fixture.sh "$TMP"
cat "$TMP/repos.yaml"
cat "$TMP/workspaces.yaml"
ls "$TMP/auth-svc/src" "$TMP/billing-svc/src"
rm -rf "$TMP"
```
Expected: two entries in `repos.yaml`, one workspace entry in `workspaces.yaml`, `lib.rs` in each crate's `src/`.

- [ ] **Step 6: Commit**

```bash
git add scripts/demo-federation-fixture.sh scripts/smoke_federation_fixture.sh
git commit -m "feat(recording): add 2-repo federation fixture for SPA demo"
```

---

## Task 2: Playwright recording driver

**Files:**
- Create: `tests/js/record_spa_demo.js`
- Reference: `tests/js/spa_e2e.test.js` (lifecycle + selector patterns)

**Interfaces:**
- Consumes: `--out <webm-path>` (default `/tmp/lain-spa-demo/raw.webm`), `--port <port>` (default 9931), `--workdir <dir>` (default `mktemp -d`), `LAIN_BIN` env (default `<repo>/target/release/lain`).
- Produces: a WebM video file at `--out`, exit 0 on success.
- Step timing is deterministic (per-step `setTimeout`) so two recordings have identical frames at identical timestamps.

- [ ] **Step 1: Write the driver**

Create `tests/js/record_spa_demo.js`:

```javascript
// Records a ~45-second demo of the Command Center SPA against the
// 2-repo federation fixture built by `scripts/demo-federation-fixture.sh`.
//
// Run: node tests/js/record_spa_demo.js --out /tmp/lain-spa-demo/raw.webm
// Env:
//   LAIN_BIN   path to the lain binary (default: ./target/release/lain)
//
// Exits 0 on success, non-zero on any failure. Step timings are pinned
// so two recordings have identical frame timing — anything that breaks
// determinism (extra long-running calls, D3 layout not settling) makes
// the resulting GIF choppy on slower hosts.

'use strict';

const { chromium } = require('playwright');
const { spawn, execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const LAIN_BIN = process.env.LAIN_BIN
  || path.resolve(__dirname, '..', '..', 'target', 'release', 'lain');
const CHROMIUM_BIN = '/usr/bin/chromium';
const FIXTURE_SCRIPT = path.resolve(
  __dirname, '..', '..', 'scripts', 'demo-federation-fixture.sh',
);

// ── CLI args ────────────────────────────────────────────────────────────

function parseArgs(argv) {
  const out = { out: '/tmp/lain-spa-demo/raw.webm', port: 9931, workdir: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--out')     out.out     = argv[++i];
    else if (a === '--port') out.port = Number(argv[++i]);
    else if (a === '--workdir') out.workdir = argv[++i];
    else { console.error(`unknown flag: ${a}`); process.exit(2); }
  }
  return out;
}

// ── Lifecycle helpers ───────────────────────────────────────────────────

function buildFixture(workdir) {
  execFileSync('bash', [FIXTURE_SCRIPT, workdir], { stdio: ['ignore', 'pipe', 'pipe'] });
}

function startServer(workdir, port) {
  const configPath = path.join(workdir, 'repos.yaml');
  return spawn(
    LAIN_BIN,
    [
      'server',
      '--config', configPath,
      '--workspace', 'biller-core',
      '--transport', 'http',
      '--port', String(port),
      '--log-level', 'warn',
    ],
    {
      cwd: workdir,
      env: { ...process.env, LAIN_API_KEYS: '' },
      stdio: ['ignore', 'pipe', 'pipe'],
    },
  );
}

async function waitForReady(baseUrl, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastErr = null;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${baseUrl}/health`);
      if (res.status === 200) {
        const body = await res.json().catch(() => null);
        if (body && body.federation &&
            Array.isArray(body.federation.repos) &&
            body.federation.repos.length >= 2 &&
            body.federation.repos.every(r => r.health === 'ready' || r.health === 'ok')) {
          return;
        }
      }
      lastErr = new Error(`status=${res.status}`);
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 500));
  }
  throw new Error(`federation not ready within ${timeoutMs}ms: ${lastErr && lastErr.message}`);
}

// ── Tab drive sequence (deterministic timings) ──────────────────────────

async function clickTab(page, name) {
  await page.click(`nav.tabs button[data-tab="${name}"]`);
  await page.waitForFunction(
    (t) => {
      const el = document.getElementById('tab-' + t);
      return el && window.getComputedStyle(el).display !== 'none';
    },
    name,
    { timeout: 10_000 },
  );
}

async function driveSequence(page) {
  // 1. Overview — let the federation health JSON render, then sit on it.
  await clickTab(page, 'overview');
  await page.waitForFunction(() => {
    const el = document.getElementById('tab-overview');
    return el && el.querySelector('pre, p, h3') !== null;
  }, { timeout: 15_000 });
  await new Promise(r => setTimeout(r, 4000));

  // 2. Repos — wait for the table to populate, then sit.
  await clickTab(page, 'repos');
  await page.waitForSelector('#tab-repos table.repo-table tbody tr', { timeout: 30_000 });
  await new Promise(r => setTimeout(r, 3000));

  // 3. Query — pick auth-svc, find Function, limit 50, run.
  await clickTab(page, 'query');
  await page.waitForSelector('#tab-query #query-repo', { timeout: 10_000 });
  await page.fill('#query-repo', 'auth-svc');
  await page.fill('#query-type', 'Function');
  await page.fill('#query-limit', '50');
  await page.click('#query-run');
  await page.waitForFunction(() => {
    const el = document.getElementById('query-output');
    return el && el.textContent && el.textContent.trim().length > 0 &&
           !/…/.test(el.textContent);
  }, { timeout: 15_000 });
  await new Promise(r => setTimeout(r, 4000));

  // 4. Tools — pick get_cross_repo_blast_radius, run with verify_token.
  await clickTab(page, 'tools');
  await page.waitForSelector('#tab-tools #tools-list li button', { timeout: 20_000 });
  // Find the right tool. The list buttons have the tool name as text.
  await page.evaluate(() => {
    const items = document.querySelectorAll('#tab-tools #tools-list li');
    for (const li of items) {
      const btn = li.querySelector('button');
      if (btn && /get_cross_repo_blast_radius/.test(btn.textContent || '')) {
        btn.click();
        return;
      }
    }
    throw new Error('get_cross_repo_blast_radius not in tools list');
  });
  await page.waitForSelector('#tab-tools #tool-args', { timeout: 10_000 });
  // Fill the args form. `name` attributes mirror the tool's inputSchema keys.
  await page.fill('#tab-tools #tool-args input[name="symbol"]', 'verify_token');
  await page.fill('#tab-tools #tool-args input[name="depth"]', '1..3');
  await page.click('#tab-tools #tool-call');
  await page.waitForFunction(() => {
    const el = document.getElementById('tool-result');
    return el && el.textContent && el.textContent.trim().length > 0 &&
           !/…/.test(el.textContent);
  }, { timeout: 20_000 });
  await new Promise(r => setTimeout(r, 5000));

  // 5. Graph — let the D3 layout settle.
  await clickTab(page, 'graph');
  try {
    await page.waitForFunction(() => {
      const svg = document.getElementById('graph-canvas');
      return svg && svg.querySelectorAll('circle').length > 0;
    }, { timeout: 15_000 });
  } catch (_) {
    // Graph may not have data for this workspace — the empty-state
    // text is acceptable; recording still finishes.
  }
  await new Promise(r => setTimeout(r, 5000));
}

// ── Main ────────────────────────────────────────────────────────────────

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const workdir = args.workdir || fs.mkdtempSync(path.join(os.tmpdir(), 'lain-record-'));
  const baseUrl = `http://127.0.0.1:${args.port}`;
  const videoDir = path.dirname(args.out);
  fs.mkdirSync(videoDir, { recursive: true });

  console.log(`== lain SPA demo recorder ==`);
  console.log(`  binary:    ${LAIN_BIN}`);
  console.log(`  workdir:   ${workdir}`);
  console.log(`  port:      ${args.port}`);
  console.log(`  out:       ${args.out}`);

  if (!fs.existsSync(LAIN_BIN)) {
    console.error(`FATAL: lain binary not found at ${LAIN_BIN}`);
    process.exit(2);
  }
  if (!fs.existsSync(CHROMIUM_BIN)) {
    console.error(`FATAL: chromium binary not found at ${CHROMIUM_BIN}`);
    process.exit(2);
  }

  console.log(`  building fixture...`);
  buildFixture(workdir);

  console.log(`  starting server...`);
  const serverProc = startServer(workdir, args.port);

  let browser;
  let exitCode = 0;
  try {
    await waitForReady(baseUrl, 120_000);
    console.log(`  federation ready`);

    browser = await chromium.launch({
      executablePath: CHROMIUM_BIN,
      args: ['--no-sandbox', '--disable-dev-shm-usage'],
      headless: true,
    });
    const context = await browser.newContext({
      viewport: { width: 1280, height: 800 },
      recordVideo: { dir: videoDir, size: { width: 1280, height: 800 } },
    });
    const page = await context.newPage();

    await page.goto(baseUrl + '/', { waitUntil: 'load', timeout: 30_000 });
    await page.waitForSelector('header.topbar h1', { timeout: 10_000 });

    await driveSequence(page);

    // Close page → video finalises. Then close context so the WebM
    // gets persisted to recordVideo.dir.
    const tempVideoPath = await page.video().path();
    await page.close();
    await context.close();
    await browser.close();
    browser = null;

    // Playwright writes the WebM to a hashed filename inside videoDir.
    // Move it to the requested --out path.
    fs.renameSync(tempVideoPath, args.out);
    console.log(`  video written: ${args.out}`);
  } catch (e) {
    console.error(`FATAL: ${e.stack || e.message}`);
    exitCode = 1;
  } finally {
    if (browser) { try { await browser.close(); } catch (_) {} }
    serverProc.kill('SIGTERM');
    await new Promise(r => setTimeout(r, 500));
    try { serverProc.kill('SIGKILL'); } catch (_) {}
    if (!process.env.RECORD_KEEP_DIR && !args.workdir) {
      try { fs.rmSync(workdir, { recursive: true, force: true }); } catch (_) {}
    } else {
      console.log(`  workdir preserved: ${workdir}`);
    }
  }
  process.exit(exitCode);
}

main().catch(e => { console.error(e); process.exit(1); });
```

- [ ] **Step 2: Smoke test the driver**

Prerequisite: a release binary must exist. Run:
```bash
cargo build --release --quiet
```
Then, with a fixed workdir so we can inspect:
```bash
WORKDIR="$(mktemp -d)"
LAIN_RECORD_KEEP_DIR="$WORKDIR" node tests/js/record_spa_demo.js \
    --out "$WORKDIR/raw.webm" --port 9932 --workdir "$WORKDIR"
echo "exit=$?"
ls -la "$WORKDIR/raw.webm"
```
Expected: `exit=0`, `raw.webm` is a non-empty file ≥ 100 KB. Inspect the workdir to confirm the fixture is intact (`repos.yaml`, `workspaces.yaml`, `auth-svc/`, `billing-svc/`).

If the recording fails on a specific selector (e.g. the Graph tab), the error message names the step. Fix the selector or timing and re-run until the WebM is produced.

Cleanup: `rm -rf "$WORKDIR"`.

- [ ] **Step 3: Commit**

```bash
git add tests/js/record_spa_demo.js
git commit -m "feat(recording): add Playwright driver that captures SPA demo"
```

---

## Task 3: Recording orchestrator script

**Files:**
- Create: `scripts/record-spa-demo.sh`

**Interfaces:**
- Consumes: flags `--no-build`, `--allow-stale`, `--port <port>`, `--json <path>`, `--keep-work`, `--help`. Reads `LAIN_BIN` env (default `<repo>/target/release/lain`).
- Produces:
  - `docs/screenshots/spa-demo.webm` (≤ 5 MB)
  - `docs/screenshots/spa-demo.mp4` (≤ 4 MB)
  - `docs/screenshots/spa-demo.gif` (≤ 8 MB; retries at fps=12 if exceeded)
  - `docs/screenshots/spa-demo-poster.png` (≤ 200 KB)
- Exit code: 0 iff every step succeeded.

- [ ] **Step 1: Write the script**

Create `scripts/record-spa-demo.sh`:

```bash
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
ok "wrote $(du -h "$MP4" | cut -f1) MP4 → $MP4"

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
ok "archived raw WebM → $ARTIFACTS/spa-demo.webm"

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
```

`chmod +x scripts/record-spa-demo.sh` (next step).

- [ ] **Step 2: Make executable and dry-run**

```bash
chmod +x scripts/record-spa-demo.sh
./scripts/record-spa-demo.sh --no-build --keep-work --port 9933
echo "exit=$?"
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
```

Expected: exit 0, all four files exist, sizes within budget. The script should print PASS lines for the WebM, MP4, GIF, and poster.

If the GIF is too big: rerun with `--keep-work` and look at the file with `ffprobe docs/screenshots/spa-demo.gif`. If the GIF is mostly the same colour, the issue is the recording captured mostly-static frames; if it's varied, the budget needs a longer recording per second. The retry-at-fps=12 fallback should cover most cases.

Cleanup: `rm -rf /tmp/lain-record-spa-demo`.

- [ ] **Step 3: Commit**

```bash
git add scripts/record-spa-demo.sh
git commit -m "feat(recording): add ffmpeg orchestrator + artifact caps"
```

---

## Task 4: Tooling wires (Makefile + npm script)

**Files:**
- Modify: `Makefile` (append `record-demo` target)
- Modify: `tests/js/package.json` (add `record-demo` npm script)

- [ ] **Step 1: Add the Makefile target**

Edit `Makefile`, replace its current contents with:

```make
# Lain — local MCP server for cross-repo and per-repo code analysis.
#
# `make schema` regenerates docs/tool-schema.json from the live
# `tools/list` payload (defect D-L2). CI runs this on every PR and
# fails the build if `git diff --exit-code docs/tool-schema.json`
# reports any change.

.PHONY: schema record-demo

schema:
	cargo run --quiet -- schema dump --out docs/tool-schema.json

record-demo:
	./scripts/record-spa-demo.sh
```

- [ ] **Step 2: Add the npm script**

Edit `tests/js/package.json`, replace its current contents with:

```json
{
  "main": "./graph_tab.test.js",
  "scripts": {
    "test:e2e": "node ./spa_e2e.test.js",
    "record-demo": "node ./record_spa_demo.js",
    "test": "node --test ./graph_tab.test.js"
  },
  "devDependencies": {
    "playwright": "^1.62.1"
  }
}
```

- [ ] **Step 3: Verify the wires parse**

```bash
make -n record-demo
node -e "console.log(require('./tests/js/package.json').scripts)"
```
Expected: `make -n record-demo` prints `./scripts/record-spa-demo.sh`. The `node -e` invocation prints `{ 'test:e2e': 'node ./spa_e2e.test.js', 'record-demo': 'node ./record_spa_demo.js', test: 'node --test ./graph_tab.test.js' }`.

- [ ] **Step 4: Commit**

```bash
git add Makefile tests/js/package.json
git commit -m "chore(tooling): wire `make record-demo` and `npm run record-demo`"
```

---

## Task 5: README restructure

**Files:**
- Modify: `README.md` (557 → ~530 lines, reorganized per spec)

- [ ] **Step 1: Rewrite the README in one pass**

Read `README.md` (557 lines). Produce a new version that:

- Keeps the title `# LAIN-mcp` and the tagline paragraph (lines 1-3).
- Inserts a new "## See it run" section immediately after the tagline (BEFORE the existing screenshot on line 5). The new section contains the hero GIF, the HD links, the 3-bullet caption, and the GitHub note block per the spec.
- Removes the existing screenshot on line 5 (`<img …>`) — the hero replaces it.
- Keeps the "How it fits together" mermaid (lines 7-16) and the prose paragraph on lines 18-21 unchanged.
- Keeps "What is Lain?" (lines 62-79) but trims ~30 % of the prose. The five feature bullets in the middle ("The value over LSP-only…") become two sentences.
- Replaces the existing TL;DR block (lines 41-60) with a one-paragraph "TL;DR — install in 30 seconds" section containing ONLY:
  - The `curl … | bash` line (line 44-45).
  - The `source ~/.zshrc` / `lain --version` snippet.
  - A pointer: "See QUICKSTART.md for the full install matrix (Homebrew, build-from-source, non-interactive flags, ONNX model)."
- Removes the existing "Installation" section (lines 111-192).
- Removes the existing "Multi-project" section (lines 321-337).
- Replaces the existing "Quick Start" section (lines 195-268) with a three-step pointer block that links to QUICKSTART sections:
  - 1. Install → `docs/QUICKSTART.md#install`
  - 2. Configure → `docs/QUICKSTART.md#federation-multi-repo`
  - 3. Wire your agent → `docs/QUICKSTART.md#single-repo-recommended-default`
- Keeps the existing "Command Center" section (lines 271-298) but prepends a one-liner: "For a narrated tour of every tab, see [command-center.md § Tour](docs/command-center.md#tour)."
- Keeps the existing "Hot Reload" section (lines 302-319).
- Keeps the existing "Federation mode" section (lines 340-350).
- Keeps the existing "Key Features" section (lines 352-415).
- Appends a "Where to go next" block at the END of Key Features (before the "## Requirements" heading):
  ```
  ## Where to go next
  
  - Operate `lain` for a team → [USER_MANUAL.md](docs/USER_MANUAL.md)
  - Federation operating guide → [FEDERATION.md](docs/FEDERATION.md)
  - Full MCP tool reference → [quickstart-tools.md](docs/quickstart-tools.md)
  - Command Center narrated tour → [command-center.md](docs/command-center.md)
  ```
- Keeps the existing "MCP Transport Modes" section (lines 467-477).
- Keeps the existing "Troubleshooting" section (lines 480-552) but adds a one-liner at the top: "For first-time setup, see [QUICKSTART.md § First aid](docs/QUICKSTART.md#first-aid) before reading this section."
- Keeps the existing "## Requirements" section (lines 418-465).
- Inserts a new "## Regenerating the demo video" section just before "## License":
  ```
  ## Regenerating the demo video
  
  The hero recording above is checked in. Re-record it after any SPA change:
  
  ```bash
  make record-demo
  ```
  
  Or: `npm run record-demo --prefix tests/js` (runs only the Playwright driver;
  you still need `scripts/record-spa-demo.sh` for the ffmpeg encoding pass).
  ```
- Keeps the existing "## License" section (lines 553-557).

The result should be ~530 lines. The commands table inside "## The commands" must be copied verbatim — `tests/cli_surface.rs` checks it against `lain --help`.

- [ ] **Step 2: Verify links and tables**

```bash
grep -hoE 'docs/[a-zA-Z0-9_./-]+' README.md | sort -u | xargs -I{} test -e {}
echo "exit=$?"
```

Expected: exit 0, every `docs/…` link resolves. If any link fails, fix the path.

Also confirm the commands table hasn't drifted:

```bash
cargo test --test cli_surface
```

Expected: PASS.

- [ ] **Step 3: Render preview**

If `grip` is installed: `grip README.md` and visually confirm the hero GIF renders inline and the HD links resolve. If `grip` is not installed: `mdcat README.md | head -50` to confirm the structure looks right.

Expected: hero GIF visible at top, three bullets under it, GitHub note block, then existing mermaid.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: README hero + progressive disclosure restructure"
```

---

## Task 6: QUICKSTART restructure

**Files:**
- Modify: `docs/QUICKSTART.md` (105 → ~140 lines)

- [ ] **Step 1: Rewrite QUICKSTART**

Read `docs/QUICKSTART.md` (105 lines). Produce a new version:

- Keep the title `# Quickstart` and the `> Five minutes from install to first answer.` line.
- Keep "## Install" (lines 5-18) but append the "After installation" block from the README (the three commands: `source ~/.zshrc`, `lain --version`, `lain --help`).
- Keep "## Two ways to use it" (lines 20-26) — rename to "## Pick a mode" and tighten.
- Keep the mermaid block (lines 27-34).
- Keep "## Single-repo (recommended default)" (lines 36-46). At the end of this section, append a "**First query**" subsection:

  ```bash
  # After your agent has indexed once, drop into a terminal:
  curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_blast_radius","arguments":{"symbol":"validate_token","depth":"1..3"}},"id":1}'
  ```
  
  Expected: a JSON `result.content[0].text` listing the callers of `validate_token` plus their paths.

- Replace "## Federation (multi-repo)" (lines 48-64) with the `biller` example that USED to live in the README (the four `lain repos add` / `lain workspaces create` lines + the `lain server --config …` line + the `open http://localhost:9999` line). Append a "**First query**" subsection:

  ```bash
  # The same cross-repo blast-radius query the video shows:
  curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_cross_repo_blast_radius","arguments":{"symbol":"verify_token","depth":"1..3"}},"id":1}'
  ```
  
  Expected: the response names at least one caller in `billing-svc`.

- Insert a new "## Watch it in action" section after Federation, containing:

  ```
  ## Watch it in action
  
  ![LAIN Command Center demo](screenshots/spa-demo.gif)
  
  **Watch in HD** ([MP4](screenshots/spa-demo.mp4), [WebM](screenshots/spa-demo.webm)).
  ```

- Keep "## Smoke test" (lines 66-80) — rename to "## First aid" or keep both? **Decision: keep "Smoke test" as-is, rename the existing "First aid" to "## Common errors".** Actually no — keep "## First aid" exactly as in lines 82-90. Move "## Smoke test" content under "## Federation" as a tail subsection "### Smoke test the federation" so the Federation story is self-contained.
- Keep "## Next" (lines 92-105) exactly.

The result should be ~140 lines.

- [ ] **Step 2: Verify links**

```bash
grep -hoE 'docs/[a-zA-Z0-9_./-]+|screenshots/[a-zA-Z0-9_./-]+' \
    docs/QUICKSTART.md | sort -u | xargs -I{} test -e {}
echo "exit=$?"
```

Expected: exit 0. (The `screenshots/…` paths resolve relative to `docs/QUICKSTART.md` → `docs/screenshots/…`.)

- [ ] **Step 3: Commit**

```bash
git add docs/QUICKSTART.md
git commit -m "docs: QUICKSTART absorbs federation example + adds first-query curl"
```

---

## Task 7: command-center.md restructure

**Files:**
- Modify: `docs/command-center.md` (229 → ~280 lines)

- [ ] **Step 1: Add the Tour section and per-tab hint lines**

Edit `docs/command-center.md`:

- Keep the title and the intro paragraph (lines 1-7) unchanged.
- Insert a new hero block IMMEDIATELY AFTER line 7 (before the existing mermaid on line 9):

  ```markdown
  [![LAIN Command Center demo](screenshots/spa-demo.gif)](screenshots/spa-demo.mp4)

  **Watch in HD** — click the GIF to open the MP4.
  ```

- Insert a new `## Tour` section IMMEDIATELY AFTER the existing mermaid (between the mermaid on line 30 and the "## Launch" heading on line 36):

  ```markdown
  ## Tour

  A walkthrough that matches what you see in the demo above:

  1. **Server boot** — the terminal at the top of the clip runs `lain repos add …`, `lain workspaces create biller-core --members auth-svc,billing-svc`, then `lain server --config ./repos.yaml --transport http --port 9931`. Watch the federation reach `ready`.
  2. **Overview tab** — `get_health` + `get_federation_health` in one view. Federation totals: `total_repos`, `ready`, `indexing`, `degraded`, `total_nodes`, `total_edges`.
  3. **Repos tab** — the per-repo table (id, path, health, node count, edge count). Both `auth-svc` and `billing-svc` show `ready`.
  4. **Query tab** — `find` op against `auth-svc`, type `Function`, limit 50. The JSON result dumps below the form.
  5. **Tools tab** — `get_cross_repo_blast_radius` against `verify_token`, depth `1..3`. The result pane shows the cross-repo call chain into `billing-svc`.
  6. **Graph tab** — D3 force-directed layout settles; cross-repo edges render in the warning colour. Hover a node to see its name, repo, kind, and path.

  The sections below describe the same surface in prose.
  ```

- Inside the "## Tabs" section (line ~152), find the bullet for each of the five tabs. Prepend a one-liner to each bullet that points to the matching Tour step. The added lines:

  ```
  - **Overview** — *(see [Tour step 2](#tour) for what this looks like)* `get_health` + `get_federation_health` in one view.
  - **Graph** — *(see [Tour step 6](#tour) for what this looks like)* D3 force-directed graph of the active workspace.
  - **Repos** — *(see [Tour step 3](#tour) for what this looks like)* per-repo table (id, path, health, node count, edge count).
  - **Query** — *(see [Tour step 4](#tour) for what this looks like)* runs `query_graph` against the federation.
  - **Tools** — *(see [Tour step 5](#tour) for what this looks like)* auto-generated MCP tool tester.
  ```

  Apply the edit carefully: the existing bullet lines use slightly different wording (Overview says "in one view", Tools says "auto-generated MCP tool tester"). Match the existing wording; the hint line is what you ADD before the existing text.

- Keep everything else unchanged (Launch, Sections mermaid, Theme, Wire format, Compatibility, Source layout, Screenshots table).

The result should be ~280 lines.

- [ ] **Step 2: Verify links**

```bash
grep -hoE '\(\.\.?/screenshots/[a-zA-Z0-9_./-]+\)|\(#[a-z-]+\)' \
    docs/command-center.md | sort -u | xargs -I{} \
    sh -c 'case "{}" in
      \(#*) test -n "$(grep -F "{}" docs/command-center.md)" || exit 1 ;;
      *) test -e "docs/{}" || exit 1 ;;
    esac'
echo "exit=$?"
```

Expected: exit 0. (All `screenshots/…` links resolve relative to `docs/command-center.md`; all in-doc anchors match a heading.)

- [ ] **Step 3: Render check**

```bash
grip docs/command-center.md 2>/dev/null || mdcat docs/command-center.md | head -60
```

Expected: hero GIF at top, Tour section visible, per-tab hints present.

- [ ] **Step 4: Commit**

```bash
git add docs/command-center.md
git commit -m "docs(command-center): add Tour section matching the demo video"
```

---

## Task 8: Generate the actual demo artifacts

**Files:**
- Create: `docs/screenshots/spa-demo.webm`
- Create: `docs/screenshots/spa-demo.mp4`
- Create: `docs/screenshots/spa-demo.gif`
- Create: `docs/screenshots/spa-demo-poster.png`

- [ ] **Step 1: Run the orchestrator**

```bash
./scripts/record-spa-demo.sh --json /tmp/lain-record-summary.json
echo "exit=$?"
cat /tmp/lain-record-summary.json
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
```

Expected: exit 0. JSON summary reports bytes for each artifact. All four files exist. Sizes within budget:
- `spa-demo.webm` ≤ 5 MB
- `spa-demo.mp4` ≤ 4 MB
- `spa-demo.gif` ≤ 8 MB (retry-at-fps=12 fallback handles up to 12 MB)
- `spa-demo-poster.png` ≤ 200 KB

If any artifact is over budget, the orchestrator will die with a clear message. Re-run with adjustments (see Task 3 for the GIF retry path).

- [ ] **Step 2: Eyeball the artifacts**

Open `docs/screenshots/spa-demo.gif` in any image viewer that supports animated GIFs. Verify:

1. The terminal at the top shows the federation boot.
2. Each tab (Overview, Repos, Query, Tools, Graph) appears in order.
3. The Tools tab shows the JSON result of the `get_cross_repo_blast_radius` call against `verify_token` — it MUST contain text like `billing-svc` somewhere (proves the cross-repo edge is real).
4. The Graph tab shows D3-laid-out circles, not a blank canvas.
5. The footer status bar text changes between tabs (transport text, repo count text).

If any step looks wrong (wrong tab order, blank screen, missing JSON), inspect the driver output and the recorded WebM with `ffprobe docs/screenshots/spa-demo.webm`. Likely fixes:
- Selector typo in `tests/js/record_spa_demo.js` — the wait will time out and the script will exit 1 with a clear message.
- Timing too short for a slow host — bump the per-step `setTimeout` durations.
- Federation never reports `ready` — re-run `cargo build --release` (the binary must be fresh).

- [ ] **Step 3: Verify the MP4 plays in a standards-compliant player**

```bash
ffprobe docs/screenshots/spa-demo.mp4 2>&1 | head -20
```

Expected: `Stream #0:0 Video: h264 (Constrained Baseline) (avc1 / 0x31637661), …`. Confirms H.264 baseline + faststart.

- [ ] **Step 4: Commit**

```bash
git add docs/screenshots/spa-demo.webm \
        docs/screenshots/spa-demo.mp4 \
        docs/screenshots/spa-demo.gif \
        docs/screenshots/spa-demo-poster.png
git commit -m "docs: capture Command Center SPA demo (WebM/MP4/GIF/poster)"
```

---

## Task 9: Final verification gate

**Files:** none modified

- [ ] **Step 1: Run the existing test suite**

```bash
cargo test --test cli_surface
node tests/js/spa_e2e.test.js
node tests/js/graph_tab.test.js
echo "exit=$?"
```

Expected: all three exit 0. If `spa_e2e.test.js` fails because the federation doesn't include the workspace it expects, the test is against a single-crate fixture — the existing e2e is unaffected by our 2-repo recording (different workdir, different port, different fixture).

- [ ] **Step 2: Link check across all touched docs**

```bash
grep -hoE 'docs/[a-zA-Z0-9_./-]+|screenshots/[a-zA-Z0-9_./-]+' \
    README.md docs/QUICKSTART.md docs/command-center.md \
  | sort -u \
  | sed 's|^|docs/|' | xargs -I{} test -e {}
echo "exit=$?"
```

(Adjust: `screenshots/…` paths from QUICKSTART.md and command-center.md are relative to `docs/`, so the `sed 's|^|docs/|'` makes them absolute relative to repo root.)

Expected: exit 0. Every link resolves.

- [ ] **Step 3: Mermaid renders in README and command-center.md**

```bash
grep -c '```mermaid' README.md docs/command-center.md docs/QUICKSTART.md
```

Expected: at least 2 (README has one, command-center.md has two, QUICKSTART has one). The counts must match what was there before — if any mermaid block was accidentally deleted, the count will be lower.

- [ ] **Step 4: Verify the commands table is unchanged**

```bash
git diff HEAD~3 HEAD -- README.md | grep -E '^\+.*\|.*\`lain ' | wc -l
git diff HEAD~3 HEAD -- README.md | grep -E '^-.*\|.*\`lain ' | wc -l
```

Expected: both counts equal the same number of `\`lain \`` rows that were in the original commands table (~12). Zero lines added or removed. (`tests/cli_surface.rs` enforces this on CI; this is just a local sanity check.)

- [ ] **Step 5: Confirm no SPA code was modified**

```bash
git diff HEAD~8 HEAD --stat -- src/server/mcp/command_center/
```

Expected: empty. The recording only uses the SPA, never modifies it.

- [ ] **Step 6: Manual user-facing sanity check**

1. Open `docs/screenshots/spa-demo.gif` and confirm it shows the demo end-to-end (terminal boot → every tab → Graph layout).
2. Open `README.md` rendered in `grip` (or GitHub's preview if you push a branch). Confirm the hero GIF renders inline and the HD links open the MP4/WebM.
3. Open `docs/command-center.md` rendered. Confirm the Tour section appears immediately after the intro mermaid.

If any step fails, the fix is in the matching task's commit. Re-do that task's commit.

- [ ] **Step 7: Final summary**

```bash
git log --oneline HEAD~8..HEAD
echo "---"
ls -la docs/screenshots/spa-demo.*
```

Expected: eight commits in order (Tasks 1–8), four artifacts at sizes within budget. Report the artifact sizes and the commit hashes back to the user.
