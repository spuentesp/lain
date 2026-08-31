# Hero Recording: Real OSS Federation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the synthetic 5-function `auth-svc`/`billing-svc` federation fixture used by the README/QUICKSTART/command-center hero recording with a real, well-known open-source federation — `tokio-rs/bytes` + `tokio-rs/tokio` — so the recorded Graph tab shows a dense, structurally real call graph with real orange cross-repo edges instead of five isolated dots.

**Architecture:** Rewrite `scripts/demo-federation-fixture.sh` to shallow-clone two real GitHub repos (with `--filter=blob:none` and a per-repo stamp file for idempotence), update `scripts/record-spa-demo.sh` with a `--fixture` flag (default `real`, with `synthetic` still available offline) and a per-clone timeout, update `tests/js/record_spa_demo.js` to wait for both repos `ready` and to pick the cross-repo blast-radius target via `find_anchors` at boot (instead of a hardcoded symbol), and swap example repo names in `README.md`, `docs/QUICKSTART.md`, and `docs/command-center.md`'s Tour step 5.

**Tech Stack:** Bash (orchestration), `git clone --depth 1 --filter=blob:none` (fixture data), Node.js + Playwright (recording driver), system Chromium (`/usr/bin/chromium`), ffmpeg (encoding), Rust + Cargo (no Rust code changes — recording only uses the existing binary).

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-31-hero-recording-real-oss-design.md`
- No edits to `src/**` (no Rust code changes).
- No edits to `src/server/mcp/command_center/**` (no SPA changes).
- No edits to `tests/**.rs` (the Rust unit / integration suites do not exercise the fixture).
- No edits to `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`, `docs/TECHNICAL.md`, `docs/hooks.md`, `docs/hot-reload.md`, `docs/multiplayer.md`, `docs/query-language.md`, `docs/quickstart-tools.md`, `docs/REPOS_YAML.md`, `docs/INDEX.md`, `docs/CI.md`, `docs/opinions/**`, `docs/srs/**`, `docs/wish-list.md`.
- No edits to `docs/screenshots/command-center-{overview,repos,tools}.png`.
- No edits to `.github/workflows/**` (no CI change).
- The README "commands" table is copied verbatim from the existing one — `tests/cli_surface.rs` checks it against `lain --help` and would fail CI if it drifts.
- The recording's `tests/js/record_spa_demo.js`'s exposed mode is `node tests/js/record_spa_demo.js --out <webm-path> --port <port> --workdir <dir>`. Driver script honors `--workdir` to skip recreating the fixture when iterating (matches the existing plan in `docs/superpowers/plans/2026-08-29-spa-demo-recording-plan.md`).
- ffmpeg binary: `/usr/bin/ffmpeg`. Chromium binary: `/usr/bin/chromium`. Playwright already installed at `tests/js/node_modules/playwright`.
- The synthetic fixture MUST remain available as `scripts/record-spa-demo.sh --fixture synthetic` so offline runs still work.

---

## File Structure

Modified:

| File | Change |
|---|---|
| `scripts/smoke_federation_fixture.sh` | Assertion rewrite: assert `bytes` + `tokio` not `auth-svc` + `billing-svc`. |
| `scripts/demo-federation-fixture.sh` | Whole script rewrite: shallow-clones `bytes` + `tokio` (or honors existing clone via stamp file) and writes a `shallow_clone`-based `repos.yaml` + `workspaces.yaml`. |
| `scripts/record-spa-demo.sh` | Adds `--fixture`, `--no-clone`; per-clone 90 s timeout; selects fixture script based on `--fixture`. |
| `tests/js/record_spa_demo.js` | Adds fixture-skeleton output to the wait-for-ready path; repos-tab check now waits for `bytes` AND `tokio` rows to read `ready`; Tools-tab picks `find_anchors repo_id=bytes` at boot and uses the top anchor as the cross-repo blast-radius target; per-step `setTimeout` durations grow to the new budget. |
| `Makefile` | Adds `record-demo-small` target beside the existing `record-demo` (which becomes `real` by default). |
| `README.md` | Federation walkthrough swaps `auth-svc` / `billing-svc` → `bytes` / `tokio`; the recorded-query curl becomes the generic `find_anchors` + `get_cross_repo_blast_radius` recipe. |
| `docs/QUICKSTART.md` | Same repo-name swap; recorded-query curl uses the same generic recipe. |
| `docs/command-center.md` | Tour step 5 (Tools) names the `find_anchors`-then-`get_cross_repo_blast_radius` recipe instead of a hardcoded symbol. |

Untouched (explicitly):

- `src/**` (no Rust changes).
- `src/server/mcp/command_center/**` (no SPA changes).
- `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`, `docs/TECHNICAL.md`, `docs/hooks.md`, `docs/hot-reload.md`, `docs/multiplayer.md`, `docs/query-language.md`, `docs/quickstart-tools.md`, `docs/REPOS_YAML.md`, `docs/INDEX.md`, `docs/CI.md`, `docs/opinions/**`, `docs/srs/**`, `docs/wish-list.md`.
- `docs/screenshots/command-center-{overview,repos,tools}.png`.
- `.github/workflows/**`.
- All Rust tests under `tests/**.rs`.

---

## Task 1: Federation fixture smoke test — switch assertions to the new fixture

**Files:**
- Modify: `scripts/smoke_federation_fixture.sh`

**Interfaces:**
- Consumes: the (about-to-be-modified) `scripts/demo-federation-fixture.sh <dir>` and the resulting `<dir>`.
- Produces: exit 0 iff the new fixture shape holds. The script is the unit-test wrapper for the fixture-script in Task 2 — Task 2 will not be considered done until this passes.

- [ ] **Step 1: Run the existing smoke test against the not-yet-rewritten fixture and confirm it passes (baseline)**

Run:
```bash
bash scripts/smoke_federation_fixture.sh
echo "exit=$?"
```
Expected: exit 0, prints `OK: federation fixture smoke test passed`.

- [ ] **Step 2: Rewrite `scripts/smoke_federation_fixture.sh` to expect the new fixture shape**

Replace the file contents with:

```bash
#!/usr/bin/env bash
# Smoke test for scripts/demo-federation-fixture.sh.
# Asserts the new federation fixture shape:
#   - $ROOT/bytes/Cargo.toml, $ROOT/tokio/Cargo.toml exist
#   - $ROOT/repos.yaml declares id: bytes and id: tokio
#   - $ROOT/workspaces.yaml declares workspace `tokio-stack` with both members
# Network-dependent: requires GitHub to be reachable (the fixture does
# `git clone --depth 1 --filter=blob:none https://github.com/tokio-rs/{bytes,tokio}.git`).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

bash "$ROOT/scripts/demo-federation-fixture.sh" "$TMP" >/dev/null

test -d "$TMP/bytes" || { echo "FAIL: bytes dir missing"; exit 1; }
test -d "$TMP/tokio" || { echo "FAIL: tokio dir missing"; exit 1; }
test -f "$TMP/bytes/Cargo.toml"  || { echo "FAIL: bytes/Cargo.toml missing"; exit 1; }
test -f "$TMP/tokio/Cargo.toml"  || { echo "FAIL: tokio/Cargo.toml missing"; exit 1; }
test -f "$TMP/repos.yaml"        || { echo "FAIL: repos.yaml missing"; exit 1; }
test -f "$TMP/workspaces.yaml"   || { echo "FAIL: workspaces.yaml missing"; exit 1; }

grep -Eq '^[[:space:]]*-?[[:space:]]*id:[[:space:]]*bytes\b'  "$TMP/repos.yaml" \
  || { echo "FAIL: repos.yaml missing id: bytes"; exit 1; }
grep -Eq '^[[:space:]]*-?[[:space:]]*id:[[:space:]]*tokio\b'  "$TMP/repos.yaml" \
  || { echo "FAIL: repos.yaml missing id: tokio"; exit 1; }
grep -q "https://github.com/tokio-rs/bytes.git"  "$TMP/repos.yaml" \
  || { echo "FAIL: repos.yaml missing bytes git url"; exit 1; }
grep -q "https://github.com/tokio-rs/tokio.git"  "$TMP/repos.yaml" \
  || { echo "FAIL: repos.yaml missing tokio git url"; exit 1; }

grep -q "tokio-stack"           "$TMP/workspaces.yaml" || { echo "FAIL: workspaces.yaml missing tokio-stack"; exit 1; }
grep -q "  - bytes\|members:.*bytes\|members:.*\[.*bytes\|^- bytes" "$TMP/workspaces.yaml" \
  || { echo "FAIL: workspaces.yaml missing bytes member"; exit 1; }
grep -q "tokio"                 "$TMP/workspaces.yaml" \
  || { echo "FAIL: workspaces.yaml missing tokio member"; exit 1; }

( cd "$TMP/bytes" && test -d .git ) || { echo "FAIL: bytes not a git repo"; exit 1; }
( cd "$TMP/tokio" && test -d .git ) || { echo "FAIL: tokio not a git repo"; exit 1; }

echo "OK: federation fixture smoke test passed"
```

- [ ] **Step 3: Run the smoke test (it must FAIL until Task 2 is in place)**

Run:
```bash
bash scripts/smoke_federation_fixture.sh
echo "exit=$?"
```
Expected: exit 1 with `FAIL: bytes dir missing` (the fixture script has not been rewritten yet — that is the next task).

- [ ] **Step 4: Commit**

```bash
git add scripts/smoke_federation_fixture.sh
git commit -m "test(recording): smoke test asserts new bytes+tokio fixture"
```

---

## Task 2: Rewrite `scripts/demo-federation-fixture.sh` to shallow-clone real OSS repos

**Files:**
- Modify: `scripts/demo-federation-fixture.sh` (replaced entirely)
- Move: `scripts/demo-federation-fixture.sh` → `scripts/legacy/demo-federation-fixture.sh` (stashing the old synthetic fixture so `--fixture synthetic` keeps working in Task 3)

**Interfaces:**
- Consumes: a target directory path (CLI arg `$1`).
- Produces, in that directory:
  - `<dir>/bytes/Cargo.toml` and `<dir>/tokio/Cargo.toml` — shallow clones of `https://github.com/tokio-rs/{bytes,tokio}.git`.
  - `<dir>/repos.yaml` — two `shallow_clone` entries (`id: bytes`, `id: tokio`).
  - `<dir>/workspaces.yaml` — one workspace `tokio-stack` with both repos as members.
  - `<dir>/{bytes,tokio}.stamp` — stamp file touched after a successful clone (used as the idempotence guard).
- Exits non-zero on any failure (no synthetic fallback).

- [ ] **Step 0: Stash the previous synthetic fixture as `scripts/legacy/demo-federation-fixture.sh`**

The previous synthetic fixture must remain reachable at a stable path so `--fixture synthetic` works after Task 3. Move it under `scripts/legacy/` BEFORE replacing the file at `scripts/demo-federation-fixture.sh`:

```bash
mkdir -p scripts/legacy
git mv scripts/demo-federation-fixture.sh scripts/legacy/demo-federation-fixture.sh
```

(This step + Task 2 step 1 = a single atomic commit. Combining in one commit makes the diff reviewable: the old file moves to legacy *and* the new real-OSS fixture takes its place.)

- [ ] **Step 1: Replace the body of `scripts/demo-federation-fixture.sh`**

Overwrite `scripts/demo-federation-fixture.sh` with:

```bash
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
cat > "$ROOT/repos.yaml" <<EOF
data_dir: $ROOT/.lain-data
repos:
  - id: bytes
    source:
      type: shallow_clone
      url: https://github.com/tokio-rs/bytes.git
  - id: tokio
    source:
      type: shallow_clone
      url: https://github.com/tokio-rs/tokio.git
EOF

cat > "$ROOT/workspaces.yaml" <<'EOF'
workspaces:
  - name: tokio-stack
    members: [bytes, tokio]
EOF

printf '  fixture: %s ready\n' "$ROOT"
```

- [ ] **Step 2: `chmod +x` and run the smoke test (from Task 1)**

```bash
chmod +x scripts/demo-federation-fixture.sh
bash scripts/smoke_federation_fixture.sh
echo "exit=$?"
```
Expected: `OK: federation fixture smoke test passed`, exit 0. (Network is required.)

If GitHub is unreachable, the script exits non-zero with `FAIL: git clone … failed — is GitHub reachable?` — that is correct; either re-run on a network-enabled host or stop here and switch to the synthetic fixture (the `scripts/record-spa-demo.sh --fixture synthetic` path from Task 3 is the fallback).

- [ ] **Step 3: Eyeball the emitted files**

```bash
TMP="$(mktemp -d)"
bash scripts/demo-federation-fixture.sh "$TMP"
cat "$TMP/repos.yaml"
echo '---'
cat "$TMP/workspaces.yaml"
echo '---'
ls "$TMP/bytes" "$TMP/tokio" | head -8
ls -la "$TMP/bytes.stamp" "$TMP/tokio.stamp"
rm -rf "$TMP"
```
Expected: `bytes` + `tokio` rows in `repos.yaml`, `tokio-stack` workspace with `[bytes, tokio]` members, both `Cargo.toml` files exist, both stamp files present.

- [ ] **Step 4: Commit**

```bash
git add scripts/demo-federation-fixture.sh
git commit -m "feat(recording): rewrite fixture to clone real OSS (bytes + tokio)"
```

---

## Task 3: Add `--fixture` and `--no-clone` flags to the orchestrator

**Files:**
- Modify: `scripts/record-spa-demo.sh`

**Interfaces:**
- New flags (existing `--no-build`, `--allow-stale`, `--port`, `--json`, `--keep-work`, `--help` keep their semantics):
  - `--fixture <name>` — `real` (default) picks the rewritten fixture from Task 2; `synthetic` picks the previous `scripts/demo-federation-fixture.sh` shape (kept in a `scripts/demo-federation-fixture.sh.synthetic` file under `scripts/legacy/` so the rewrite is reversible). Note: a simpler implementation: we KEEP the original synthetic file's logic by ALSO writing a tiny wrapper script `scripts/demo-federation-fixture.sh.synthetic` that produces the old `auth-svc` + `billing-svc` fixture.
  - `--no-clone` — skip the fixture script entirely; assume `<workdir>` already contains the right files. Used for debug / iteration. (Combined with `--workdir <dir>`.)
- Per-clone ceiling (only when `--fixture real` and not `--no-clone`): 90 s per repo. If a single clone overruns, the orchestrator dies with `FATAL: clone of bytes (or tokio) exceeded 90 s`. Existing 500 ms polling intervals in the recorder are unchanged.
- Per `--fixture`, the orchestrator picks which fixture script to run. Default (`real`) → `scripts/demo-federation-fixture.sh`. `synthetic` → `scripts/legacy/demo-federation-fixture.sh` (see Step 0 below).

- [ ] **Step 0: Stash the previous synthetic fixture as `scripts/legacy/demo-federation-fixture.sh`**

Before changing `scripts/record-spa-demo.sh`, move the existing
`scripts/demo-federation-fixture.sh` to
`scripts/legacy/demo-federation-fixture.sh` so it remains available for
`--fixture synthetic`. Concretely, the sequence is:

```bash
mkdir -p scripts/legacy
git mv scripts/demo-federation-fixture.sh scripts/legacy/demo-federation-fixture.sh
```

This creates a `deleted:` line for the old file in the diff. The new file
(Task 2) replaces it under the same name, so the diff cleanly shows
old-synthetic → legacy-storage and new-real-fixture → scripts/demo-federation-fixture.sh.

This is a single commit *combined with* Task 2's commit so the diff is
reviewable in one place; if you prefer two commits, stop after Step 0 here
and run `git commit -m "refactor(recording): move synthetic fixture to legacy/"` before
proceeding to Task 2 Step 1.

(Continue in this task after Task 2 lands.)

- [ ] **Step 1: Read `scripts/record-spa-demo.sh` to confirm the flag-parsing block**

The orchestrator's argument parser lives at the top of the file. The
relevant block parses `--no-build`, `--allow-stale`, `--keep-work`,
`--json`, `--port`. We will *add* `--fixture <name>` and `--no-clone`
in the same place. Confirm by reading lines 30-80 of
`scripts/record-spa-demo.sh` (the existing plan from
2026-08-29-spa-demo-recording-plan.md has the same shape).

- [ ] **Step 2: Add the new flags + selection logic**

In `scripts/record-spa-demo.sh`, find the `while [ $# -gt 0 ]` block
that currently handles `--no-build`, `--allow-stale`, `--keep-work`,
`--json`, `--port`, `-h|--help`. Extend it like this (the new cases
live alongside the existing ones, alphabetically grouped):

```bash
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
    --help|-h)
      sed -n '2,16p' "$0"
      exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done
```

Just after the variable initialization block, add a fixture-resolution
function and a per-clone timer:

```bash
# Resolve which fixture script to use. The `real` fixture does two
# `git clone`s against GitHub; the `synthetic` fixture is the original
# `auth-svc` + `billing-svc` two-crate pair, kept under scripts/legacy/
# for offline runs.
case "$FIXTURE" in
  real)      FIXTURE_SCRIPT="$REPO_ROOT/scripts/demo-federation-fixture.sh" ;;
  synthetic) FIXTURE_SCRIPT="$REPO_ROOT/scripts/legacy/demo-federation-fixture.sh" ;;
  *) die "--fixture must be 'real' or 'synthetic' (got: $FIXTURE)" ;;
esac
[ -x "$FIXTURE_SCRIPT" ] || die "fixture script $FIXTURE_SCRIPT is missing or not executable"
```

(We assume `REPO_ROOT`, `QUICK`, `ALLOW_STALE`, `KEEP_WORK`, `JSON_OUT`,
`PORT`, and the `die` helper already exist further down the file —
they do, in the existing orchestrator.)

Then in the section labelled `# ── 2. binary freshness check` (around
line 100 in the existing script) find `# ── 3. record WebM` and
replace the lines that call the fixture script with:

```bash
# ── 3. fixture (clone or pre-existing) ────────────────────────────────────
if [ "$NO_CLONE" = 0 ]; then
  say "building fixture (--fixture $FIXTURE)"
  CLONE_T0=$(date +%s%N)
  WORKDIR="$WORK"
  mkdir -p "$WORKDIR"
  bash "$FIXTURE_SCRIPT" "$WORKDIR" \
    || die "fixture script failed; rerun with --keep-work to inspect $WORKDIR"
  CLONE_T1=$(date +%s%N)
  CLONE_MS=$(( (CLONE_T1 - CLONE_T0) / 1000000 ))
  if [ "$CLONE_MS" -gt 90000 ]; then
    die "fixture build took ${CLONE_MS}ms (>90s cap); rerun with --keep-work to inspect"
  fi
  ok "fixture built in ${CLONE_MS}ms → $WORKDIR"
else
  say "skipping fixture (--no-clone); using existing $WORK"
fi

# ── 4. record WebM ────────────────────────────────────────────────────────
RAW_WEBM="$WORK/raw.webm"
say "recording SPA demo (port $PORT)"
LAIN_BIN="$LAIN" \
RECORD_KEEP_DIR="$KEEP_WORK" \
  node "$REPO_ROOT/tests/js/record_spa_demo.js" \
    --out "$RAW_WEBM" --port "$PORT" --workdir "$WORK" \
    || die "recording failed; inspect $WORK/server.log or rerun with --keep-work"
```

Renumber the remaining "── N." headers (4→5, 5→6, …) so the rest of
the script's prose still flows.

Update the script's `--help` block (currently `sed -n '2,16p'`) to
include the new flags:

```bash
#   --fixture <real|synthetic>  pick the recording fixture (default: real)
#   --no-clone                  skip the fixture step (assume pre-populated workdir)
```

- [ ] **Step 3: Run the orchestrator in `--no-clone` mode against the fresh fixture**

After Task 2 has produced `/tmp/lain-oss-check/{bytes,tokio,repos.yaml,workspaces.yaml}`, the orchestrator should accept it:

```bash
TMP="$(mktemp -d)"
bash scripts/demo-federation-fixture.sh "$TMP"
ls "$TMP" | head
# Pick a port that is not in use, then drive just the fixture-validation branch:
./scripts/record-spa-demo.sh --fixture real --no-clone --keep-work --no-build --port 9937 \
  --workdir "$TMP"
echo "exit=$?"
```
Expected: exit 0, prints `PASS fixture built in <ms>ms` (or rather the
equivalent of `ok "fixture built in ${CLONE_MS}ms"` — wait, that line
runs only when `--no-clone` is NOT set). With `--no-clone` set the
fixture step is skipped; the orchestrator prints `skipping fixture …`
and proceeds straight to Playwright. Either path is fine for this check —
we are verifying the parser accepts the new flags without erroring.

Inspect the WebM at the path it reports (`/tmp/lain-record-spa-demo/raw.webm` by default) and confirm it is ≥ 100 KB. Cleanup: `rm -rf /tmp/lain-record-spa-demo "$TMP"`.

- [ ] **Step 4: Verify the synthetic path still works**

```bash
./scripts/record-spa-demo.sh --help
```

Expected: help text includes `--fixture <real|synthetic>` and
`--no-clone`. The synthetic fixture script is at
`scripts/legacy/demo-federation-fixture.sh`, so a smoke run with
`--fixture synthetic --no-build --keep-work --port 9938` should still
complete end-to-end (this exercises the offline path of the recording
pipeline and confirms the move-to-`legacy/` did not break the path
resolution).

- [ ] **Step 5: Commit**

```bash
git add scripts/record-spa-demo.sh scripts/legacy/demo-federation-fixture.sh
git commit -m "feat(recording): --fixture real|synthetic + --no-clone flag"
```

---

## Task 4: Update the Playwright driver for the new fixture

**Files:**
- Modify: `tests/js/record_spa_demo.js`

**Interfaces:**
- Existing CLI contract unchanged: `--out <webm-path>`, `--port <port>`, `--workdir <dir>`, `LAIN_BIN` env.
- New behaviour inside `driveSequence(page)`:
  - Repos-tab step now waits for `tr` rows whose text contains both `bytes` and `tokio` (in any order) and reads `ready` for both.
  - Tools-tab step picks the top anchor of the `bytes` repo with `tools/call find_anchors arguments={"repo_id":"bytes","limit":10}` and extracts the first symbol name from the response, then uses that for `get_cross_repo_blast_radius`. If `find_anchors` returns no anchors (network/indexer failure), the driver falls back to the literal `Bytes` (the bytes crate's type-name symbol) and prints a warning to stderr.
  - Per-step `setTimeout` durations grow per the new budget (Overview 4 s unchanged, Repos 3 s → 4 s, Query 4 s → 6 s, Tools 5 s → 6 s, Graph 5 s → 8 s).

- [ ] **Step 1: Read the current driver**

Read `tests/js/record_spa_demo.js` end-to-end. It is 521 lines and lives at the top of `tests/js/`. The function `driveSequence(page)` (around line 370) is where the per-tab steps live. The fixture-target swap affects two call sites: `page.fill('#query-repo', 'auth-svc')` and the tools-tab symbol 'verify_token'.

- [ ] **Step 2: Update the Repos-tab, Query-tab, and Tools-tab invocations inside `driveSequence`**

In `tests/js/record_spa_demo.js`, find:

```javascript
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
  ...
```

Replace it with:

```javascript
  // 2. Repos — wait for the table to populate with both `bytes` and `tokio`
  //    reading `ready`, then sit.
  await clickTab(page, 'repos');
  await page.waitForFunction(() => {
    const rows = document.querySelectorAll('#tab-repos table.repo-table tbody tr');
    if (rows.length < 2) return false;
    const ids = Array.from(rows).map(r => (r.textContent || '').toLowerCase());
    const readyCount = Array.from(rows).filter(r => /ready/.test(r.textContent || '')).length;
    return ids.some(t => t.includes('bytes'))
        && ids.some(t => t.includes('tokio'))
        && readyCount >= 2;
  }, { timeout: 60_000 });
  await new Promise(r => setTimeout(r, 4000));

  // 3. Query — pick tokio (the larger repo), find Function, limit 50, run.
  await clickTab(page, 'query');
  await page.waitForSelector('#tab-query #query-repo', { timeout: 10_000 });
  await page.fill('#query-repo', 'tokio');
  await page.fill('#query-type', 'Function');
  await page.fill('#query-limit', '50');
  ...
```

- [ ] **Step 3: Update the cross-repo blast radius call site**

Find:

```javascript
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
```

Replace it with:

```javascript
  // 4. Tools — pick get_cross_repo_blast_radius against a real bytes anchor.
  await clickTab(page, 'tools');
  await page.waitForSelector('#tab-tools #tools-list li button', { timeout: 20_000 });
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

  // Pick the top anchor from the bytes repo at boot. The recording
  // should not hardcode a symbol name — bytes renames APIs across
  // releases, and pinning a name in the driver would silently rot the
  // hero demo. Fall back to the literal `Bytes` if find_anchors is
  // empty (e.g. network/indexer hiccup).
  const crossRepoSymbol = await page.evaluate(async () => {
    try {
      const r = await fetch('/mcp', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({
          jsonrpc: '2.0', id: 1, method: 'tools/call',
          params: { name: 'find_anchors', arguments: { repo_id: 'bytes', limit: 10 } },
        }),
      });
      const body = await r.json();
      const text = (body && body.result && body.result.content
        && body.result.content[0] && body.result.content[0].text) || '';
      // find_anchors emits a numbered list, e.g. "1. Bytes\n2. Buf\n...".
      const m = text.match(/^\s*1\.\s+([A-Za-z_][A-Za-z0-9_]*)/m);
      return m ? m[1] : null;
    } catch (_) {
      return null;
    }
  }) || 'Bytes';
  if (crossRepoSymbol === 'Bytes') {
    process.stderr.write('WARN: find_anchors returned no bytes anchor; falling back to literal "Bytes"\n');
  }

  await page.fill('#tab-tools #tool-args input[name="symbol"]', crossRepoSymbol);
  await page.fill('#tab-tools #tool-args input[name="depth"]', '1..3');
  await page.click('#tab-tools #tool-call');
  await page.waitForFunction(() => {
    const el = document.getElementById('tool-result');
    return el && el.textContent && el.textContent.trim().length > 0 &&
           !/…/.test(el.textContent);
  }, { timeout: 30_000 });
  await new Promise(r => setTimeout(r, 6000));
```

(Adjust the `5000` final hold to `6000`, matching the new budget.)

- [ ] **Step 4: Update the Graph-tab hold**

Find the Graph-tab step:

```javascript
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
```

Replace the final `5000` hold with `8000`:

```javascript
  await new Promise(r => setTimeout(r, 8000));
```

- [ ] **Step 5: Smoke-test the driver**

```bash
TMP="$(mktemp -d)"
bash scripts/demo-federation-fixture.sh "$TMP"
cargo build --release --quiet
LAIN_RECORD_KEEP_DIR="$TMP" node tests/js/record_spa_demo.js \
  --out "$TMP/raw.webm" --port 9939 --workdir "$TMP"
echo "exit=$?"
ls -la "$TMP/raw.webm"
ffprobe "$TMP/raw.webm" 2>&1 | grep -E 'Duration|Stream'
rm -rf "$TMP"
```
Expected: `exit=0`, `raw.webm` ≥ 100 KB, `Duration` ≥ 00:00:30 (the new budget), `Stream` is `Video: vp8` or `vp9` (Playwright's default codecs). If any step times out, error message will name the failing wait condition (per `waitForFunction`); common causes are: stale binary (re-run `cargo build --release`), repos not `ready` (the repos-table check), or D3 settling too slow.

- [ ] **Step 6: Commit**

```bash
git add tests/js/record_spa_demo.js
git commit -m "feat(recording): driver waits for bytes+tokio ready, picks anchor dynamically"
```

---

## Task 5: Add `record-demo-small` to the Makefile

**Files:**
- Modify: `Makefile`

**Interfaces:**
- New target `record-demo-small` invokes `record-spa-demo.sh --fixture synthetic` for offline use of the previous (pathetic but offline-friendly) fixture.
- Existing `record-demo` continues to invoke `record-spa-demo.sh`; with Task 3's default, that becomes the new OSS recording.

- [ ] **Step 1: Update the `.PHONY` declaration + add the new target**

In `Makefile`, replace the existing `Makefile` body with:

```make
# Lain — local MCP server for cross-repo and per-repo code analysis.
#
# `make schema` regenerates docs/tool-schema.json from the live
# `tools/list` payload (defect D-L2). CI runs this on every PR and
# fails the build if `git diff --exit-code docs/tool-schema.json`
# reports any change.

.PHONY: schema record-demo record-demo-small

schema:
	cargo run --quiet -- schema dump --out docs/tool-schema.json

record-demo:
	./scripts/record-spa-demo.sh            # default: --fixture real (bytes + tokio)

record-demo-small:
	./scripts/record-spa-demo.sh --fixture synthetic   # offline, original fixture
```

- [ ] **Step 2: Verify the Makefile parses**

```bash
make -n record-demo
make -n record-demo-small
```
Expected: `record-demo` prints `./scripts/record-spa-demo.sh`; `record-demo-small` prints `./scripts/record-spa-demo.sh --fixture synthetic`. Both with no errors.

- [ ] **Step 3: Commit**

```bash
git add Makefile
git commit -m "chore(tooling): record-demo-small — synthetic fixture for offline runs"
```

---

## Task 6: Update README federation example

**Files:**
- Modify: `README.md`

**Interfaces:**
- The Section "Federation mode" and any inline curl examples that mention `auth-svc`, `billing-svc`, `verify_token` get swapped for `bytes`, `tokio`, and the generic `find_anchors` + `get_cross_repo_blast_radius` recipe.
- The "Regenerating the demo video" footnote gets a one-line note about `make record-demo-small`.

- [ ] **Step 1: Find every line in `README.md` that mentions the old fixture names**

```bash
grep -nE 'auth-svc|billing-svc|verify_token|biller' README.md
```
Expected: a small number of hits (each lives inside the federation
example block, the "Regenerating the demo video" footnote, or any
backticks-curl walkthrough).

- [ ] **Step 2: Apply the swap**

For every hit, replace per the table:

| Was | Becomes |
|---|---|
| `auth-svc` | `bytes` |
| `billing-svc` | `tokio` |
| `verify_token` (in a curl example) | the generic recipe below |
| `biller-core` (workspace name) | `tokio-stack` |
| `biller` (any leftover example dir) | `tokio-stack` |

The generic recipe replaces any hardcoded-symbol curl:

```bash
# Pick a real bytes anchor and trace its callers across both repos
curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"find_anchors","arguments":{"repo_id":"bytes","limit":10}},"id":1}'

# Then take the top result and ask for its cross-repo blast radius:
curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_cross_repo_blast_radius","arguments":{"symbol":"<top-anchor>","depth":"1..3"}},"id":1}'
```

In the "Regenerating the demo video" footnote, append a single
sentence: `For the offline (synthetic) fixture, run \`make record-demo-small\`.`

- [ ] **Step 3: Verify links**

```bash
grep -hoE 'docs/[a-zA-Z0-9_./-]+' README.md | sort -u | xargs -I{} test -e {}
echo "exit=$?"
```
Expected: exit 0. Fix any path the grep misses.

- [ ] **Step 4: Verify the commands table is untouched**

```bash
cargo test --test cli_surface
```
Expected: PASS (the `## The commands` table is checked against `lain --help`).

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs(readme): federation example uses bytes+tokio + generic anchor recipe"
```

---

## Task 7: Update QUICKSTART federation example + recorded-query recipe

**Files:**
- Modify: `docs/QUICKSTART.md`

**Interfaces:**
- The federation walkthrough swaps repo + workspace names.
- The recorded `get_workspace_graph` curl example is unchanged (it operates against the whole workspace, not a specific symbol).
- The `get_cross_repo_blast_radius` curl example becomes the generic `find_anchors` + symbol-replaced recipe.

- [ ] **Step 1: Find every line that mentions the old fixture names**

```bash
grep -nE 'auth-svc|billing-svc|verify_token|biller-core|biller' docs/QUICKSTART.md
```

- [ ] **Step 2: Apply the same swap as Task 6**

Use the same Was/Becomes table from Task 6. Replace the `**First
query**` example under "Federation (multi-repo)" with:

```bash
# Pick the top anchor of the bytes repo and trace it across tokio.
curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"find_anchors","arguments":{"repo_id":"bytes","limit":10}},"id":1}'

# Then, with the top result as the symbol:
curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"get_cross_repo_blast_radius","arguments":{"symbol":"<top-anchor>","depth":"1..3"}},"id":1}'
```

- [ ] **Step 3: Verify links**

```bash
grep -hoE 'docs/[a-zA-Z0-9_./-]+|screenshots/[a-zA-Z0-9_./-]+' \
    docs/QUICKSTART.md | sort -u | xargs -I{} test -e {}
echo "exit=$?"
```
Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add docs/QUICKSTART.md
git commit -m "docs(quickstart): federation example uses bytes+tokio + generic anchor recipe"
```

---

## Task 8: Update command-center.md Tour step 5

**Files:**
- Modify: `docs/command-center.md`

**Interfaces:**
- The Tour section's step 5 currently names `verify_token` as the recorded symbol. Replace with the `find_anchors`-then-`get_cross_repo_blast_radius` recipe.

- [ ] **Step 1: Find the Tour section and step 5**

```bash
grep -nE 'Tour|verify_token|get_cross_repo_blast_radius' docs/command-center.md
```

- [ ] **Step 2: Replace the symbol in step 5 with the generic recipe**

Find the bullet for "Tools" inside the Tour:

```
  5. **Tools tab** — `get_cross_repo_blast_radius` against `verify_token`, depth `1..3`. The result pane shows the cross-repo call chain into `billing-svc`.
```

Replace it with:

```
  5. **Tools tab** — `find_anchors` against the `bytes` repo, take the top result, then `get_cross_repo_blast_radius` on that symbol with depth `1..3`. The result pane shows real cross-repo call chains into `tokio` (e.g. `bytes::Buf` callers across tokio's I/O codec and runtime).
```

- [ ] **Step 3: Verify links**

```bash
grep -hoE '\(\.\.?/screenshots/[a-zA-Z0-9_./-]+\)|\(#[a-z-]+\)' \
    docs/command-center.md | sort -u | xargs -I{} \
    sh -c 'case "{}" in
      \(#*) test -n "$(grep -F "{}" docs/command-center.md)" || exit 1 ;;
      *) test -e "docs/{}" || exit 1 ;;
    esac'
echo "exit=$?"
```
Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add docs/command-center.md
git commit -m "docs(command-center): Tour step 5 uses generic anchor recipe"
```

---

## Task 9: Run the recording end-to-end + frame-check the GIF

**Files:**
- This task doesn't modify source code, but it produces the deliverables that the whole plan exists for.

- [ ] **Step 1: Clean build + run the full recording**

```bash
cargo build --release --quiet
./scripts/record-spa-demo.sh --json /tmp/lain-record-summary.json
echo "exit=$?"
cat /tmp/lain-record-summary.json
ls -la docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}
```
Expected: exit 0, summary lists byte counts for all four artifacts,
each within budget (5 MB / 4 MB / 8 MB / 200 KB). If the GIF overflows
its budget, the orchestrator retries at fps=12 per the existing
design; if it still overflows, die with the 12 MB hard cap.

- [ ] **Step 2: Frame-check the Graph tab**

```bash
TMP="$(mktemp -d)"
ffmpeg -y -hide_banner -loglevel error -ss 25 -i docs/screenshots/spa-demo.gif \
  -frames:v 1 "$TMP/graph-frame.png"
ls -la "$TMP/graph-frame.png"
rm -rf "$TMP"
```
Expected: file ≥ 50 KB. Visually inspect the extracted frame: the
Graph tab's metadata line should begin with a node count ≥ 1500 (was
5), and at least one orange `graph-link cross-repo` line should be
visible crossing the canvas.

- [ ] **Step 3: Frame-check the Tools-tab step**

```bash
TMP="$(mktemp -d)"
ffmpeg -y -hide_banner -loglevel error -ss 16 -i docs/screenshots/spa-demo.gif \
  -frames:v 1 "$TMP/tools-frame.png"
```
Expected: the Tools-tab JSON result is visible at t≈16 s and contains
a `by_repo{}` block referencing the `tokio` repo id (proves the
cross-repo blast-radius call returned a real result, not a synthetic
stub). rm -rf "$TMP" after.

- [ ] **Step 4: Re-record the docs artifacts**

```bash
git add docs/screenshots/spa-demo.webm \
        docs/screenshots/spa-demo.mp4 \
        docs/screenshots/spa-demo.gif \
        docs/screenshots/spa-demo-poster.png
git commit -m "docs: capture Command Center hero against real OSS federation (bytes + tokio)"
```

---

## Task 10: Final verification gate

- [ ] **Step 1: Existing tests still pass**

```bash
cargo test --test cli_surface
node tests/js/spa_e2e.test.js
node tests/js/graph_tab.test.js
echo "exit=$?"
```
Expected: all exit 0. `spa_e2e.test.js` is unaffected because it runs
against a single-crate fixture on a different port.

- [ ] **Step 2: Smoke test still passes**

```bash
bash scripts/smoke_federation_fixture.sh
```
Expected: `OK: federation fixture smoke test passed`, exit 0 (network
required; skip if offline and report in the summary).

- [ ] **Step 3: Cross-doc link check**

```bash
grep -hoE 'docs/[a-zA-Z0-9_./-]+|screenshots/[a-zA-Z0-9_./-]+' \
    README.md docs/QUICKSTART.md docs/command-center.md \
  | sort -u \
  | sed 's|^|docs/|' | xargs -I{} test -e {}
echo "exit=$?"
```
Expected: exit 0. Every `docs/…` and `screenshots/…` link resolves
under `docs/`.

- [ ] **Step 4: SPA untouched**

```bash
git diff HEAD~10 HEAD --stat -- src/server/mcp/command_center/
```
Expected: empty diff. The recording only uses the SPA, never modifies it.

- [ ] **Step 5: Rust source untouched**

```bash
git diff HEAD~10 HEAD --stat -- src/
```
Expected: empty diff. No Rust code changed.

- [ ] **Step 6: Final summary**

```bash
git log --oneline HEAD~10..HEAD
echo '---'
ls -la docs/screenshots/spa-demo.*
```
Expected: 9-10 commits in order (Tasks 1-9), all four artifacts at
sizes within budget. Report commit hashes + artifact sizes + the new
Graph-frame node count back to the user.
