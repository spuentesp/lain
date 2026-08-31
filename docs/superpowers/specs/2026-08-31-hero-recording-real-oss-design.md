# Hero recording: real open-source federation (replace the synthetic fixture)

Date: 2026-08-31
Status: design
Owner: recording pipeline, no SPA or CLI changes

## Problem

The README hero (and the matching section in `docs/QUICKSTART.md` and the
Tour in `docs/command-center.md`) shows the Command Center running against a
synthetic 2-crate fixture: `auth-svc` + `billing-svc`. The fixture is
deliberately tiny — `scripts/demo-federation-fixture.sh` writes only five
function/method nodes (`verify_token`, `charge_invoice`,
`verify_token_bridge`, `rejects_empty`, `rejects_short`, `accepts_long_enough`)
and **zero real call edges**. The Metadata line in the recorded Graph tab reads
`5 nodes · 0 edges · 0 cross-repo`, and the canvas shows five isolated dots
whose names are test functions. That is the headline screenshot of this
project, and it looks pathetic.

The reasons the fixture is structurally weak:

- `auth-svc` defines `verify_token` but its only other call sites are the
  `tests` module (`rejects_*` / `accepts_long_enough`), which the indexer's
  `node_kind_str` filter (`Function | Method | Class`) keeps but does not
  wire through any call-edge machinery.
- `billing-svc` does not `use auth_svc::verify_token`. Its body has a private
  `verify_token_bridge` helper so the indexer picks the symbol up by name,
  but there is no real `Calls` edge between the two repos. The comment in
  `scripts/demo-federation-fixture.sh` admits this: *"for the fixture the
  indexer only needs the symbol name to appear in the source so cross-repo
  edges resolve. The recording doesn't execute the code, it just queries the
  graph."*

The result: a recording about an MCP server for cross-file structural
reasoning, with a graph that proves nothing.

## Goal

The hero recording must show lain indexing real code that has real call
structure, with a real cross-repo edge visible in the Graph tab. The
federation narrative stays — `get_cross_repo_blast_radius` is still the
Tools-tab headline tool — but it is no longer faked.

Concretely, when the recording reaches the Graph tab, the metadata line
should read roughly `≥ 1500 nodes · ≥ 3000 edges · ≥ 50 cross-repo · truncated`
(the "truncated" flag is fine; the server caps at 5000 nodes / 10000 edges
in `src/server/mcp/federation_tools/workspace.rs:104-105`). The D3 force
layout should lay out a visibly dense graph, with at least one orange
"cross-repo" line bridging the two repos.

## Approach (chosen)

Replace the synthetic 2-crate federation in `scripts/demo-federation-fixture.sh`
with a pair of real, well-known Rust open-source repos loaded via
`shallow_clone` — the smallest of the three `RepoSource` variants
(`docs/REPOS_YAML.md`):

- **Tokio** — `https://github.com/tokio-rs/tokio.git` (current default
  branch, currently `master` at time of writing but the script does not
  pin it). ~150 kLOC, ~700 source files. Yields ~3-5k indexed
  function/method/class nodes once deduplicated.
- **Bytes** — `https://github.com/tokio-rs/bytes.git` (current default
  branch). ~5 kLOC, kept small so the federation index time is bounded
  by tokio. `bytes::Buf` and `bytes::Bytes` are referenced pervasively
  from tokio's I/O / codec code, which gives the cross-repo blast-radius
  query a real answer instead of a synthetic one.

Why these two:

- Both pure Rust — same LSP toolchain, same AST grammar, no need to widen the
  indexer's language matrix.
- Both in the same GitHub org — they are a real production pairing (tokio
  depends on bytes in `Cargo.toml`).
- Tokio is the headline crate in the Rust async ecosystem. Anyone reading
  the README is already familiar with it.
- `shallow_clone` keeps the recording's disk + network footprint bounded
  (~5 MB each) and finishes indexing inside the recording's
  `waitForReady(baseUrl, 120_000)` ceiling.
- The cross-repo edge is *structural*, not just lexical — tokio really calls
  into bytes via the `Buf` / `Bytes` trait surface, so the recorded graph
  renders real orange cross-repo lines.

Both repos are pulled at their current default branch. We let
`git clone --depth 1` discover it (no `--branch` argument) and rely on
`RepoSource` defaulting `ref` to `"main"` per `docs/REPOS_YAML.md:120`.
If either repo renames its trunk later, both `repos.yaml`'s `ref` and the
`git clone` invocation need updating in the same place
(`scripts/demo-federation-fixture.sh`).

## Out of scope

- No edits to `src/server/**`, `src/cli/**`, or any Rust source. The change
  is fixture-only.
- No edits to `src/server/mcp/command_center/**`. The recorder only uses
  the SPA; the SPA's D3 force layout already handles `≥ 1500` nodes
  gracefully (label drawing is suppressed above 150 in
  `app.js:902`).
- No new MCP tools, no schema changes, no CLI changes.
- No changes to `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`,
  `docs/TECHNICAL.md`, `docs/hooks.md`, `docs/hot-reload.md`,
  `docs/multiplayer.md`, `docs/query-language.md`,
  `docs/quickstart-tools.md`, `docs/INDEX.md`, `docs/CI.md`,
  `docs/opinions/`, `docs/srs/`, `docs/wish-list.md`.
- No changes to the per-tab static screenshots under
  `docs/screenshots/command-center-{overview,repos,tools}.png`.
- No CI workflow changes. Recording is on-demand only.

## What changes

### `scripts/demo-federation-fixture.sh` — rewritten

The new script does the work that `lain repos add …` would do in a real
deployment, but offline-friendly:

1. Take a target directory `$ROOT` from `$1` (same interface as today).
2. For each repo in `bytes`, `tokio`:
   - Skip if `$ROOT/<repo>/.git` exists AND the stamp `$ROOT/<repo>.stamp`
     is newer than the script's own mtime (so a re-run does not pay for
     the clone again).
   - `git clone --depth 1 --filter=blob:none https://github.com/tokio-rs/<repo>.git "$ROOT/<repo>"` otherwise.
   - Touch the stamp file on success.
3. Write `$ROOT/repos.yaml` with two `shallow_clone` entries
   (`id: bytes`, `id: tokio`, `ref: main` left implicit).
4. Write `$ROOT/workspaces.yaml` with one workspace `tokio-stack`
   with both repos as members (matches today's single-workspace demo).

`--filter=blob:none` drops blobs until `git checkout` needs them — we
don't, so the working tree is mostly empty but the indexer walks files
already on disk. In practice the working tree of a `--filter=blob:none`
clone is still populated enough for lain's tree-sitter pass; we add a
follow-up `git -C "$ROOT/<repo>" checkout HEAD -- .` if the indexer logs
"no source files found". Clones finish in 5-20 s on a fresh container
(20-60 MB download, not 100+ MB).

If the clones fail (e.g., no network), the script prints a clear error
naming the URL and exits non-zero. There is no synthetic-fallback path —
the recording is no good without real data, and silently degrading to
5-node pathetic mode is what produced this design in the first place.

The script keeps its exit-code contract (0 iff every step succeeded) and
its smoke test (`scripts/smoke_federation_fixture.sh`, which will be
updated to assert the two `repos.yaml` entries and the workspace).

### `tests/js/record_spa_demo.js` — driving sequence updated

The 5-step drive sequence (Overview → Repos → Query → Tools → Graph) is
unchanged in shape. Two differences:

1. **Wait-on-ready grows.** `waitForReady(baseUrl, 120_000)` stays as the
   ceiling. The fixtures are larger, so the inner polling interval and the
   page-side `data-tab="repos"` health check are widened — repos-table
   cells must read `ready` for both `bytes` and `tokio` before the
   Repos-tab step proceeds.
2. **Tool calls switch repos.** The cross-repo blast-radius call runs
   after the driver picks a real, heavily-called `bytes` symbol at boot
   time — the driver calls `tools/call find_anchors arguments={"repo_id":"bytes","limit":10}`
   and picks the top anchor. The reasoning is that pinning the target
   symbol in the spec would break the recording the moment `bytes`
   changes a name; selecting from the anchor list at the moment of
   recording keeps the demo honest. The recording confirms the response
   contains a non-`bytes` repo name (i.e. `tokio`) inside at least one
   `by_repo{}` block — the proof that the call crossed the boundary for
   a real reason. The Query tab's example target becomes `tokio` (was
   `auth-svc`) and the find-type literal stays `Function`.

### `scripts/record-spa-demo.sh` — orchestrator updated

- Adds a fresh `--fixture <name>` flag. Default is `real` (the new OSS
  fixture). The synthetic fixture stays available as
  `--fixture synthetic` for offline / unit-test runs.
- Adds a `--no-clone` flag for debug runs against a pre-populated
  `$WORK`. Recording reruns otherwise re-clone (idempotent if the stamp
  file is present).
- Existing flags (`--no-build`, `--allow-stale`, `--port`, `--json`,
  `--keep-work`) keep their semantics.
- Adds a hard cap on the per-clone step: 90 s. If the clone still has not
  finished, the orchestrator dies with a clear message ("network too slow
  or repo too large; rerun with --keep-work to inspect").
- The MP4/GIF/poster output paths (`docs/screenshots/spa-demo.{mp4,gif,poster.png,webm}`)
  stay the same — overwriting the existing pathetic demo is the point.
- Steps 4 (MP4), 5 (GIF), 6 (poster), 7 (raw WebM archive), 8 (optional
  JSON) keep their behaviour.

### `tests/js/` — only the recorder changes; no new tests

The smoke test for the federation fixture
(`scripts/smoke_federation_fixture.sh`) gets its assertions updated to
match the new fixture: instead of looking for `verify_token` strings in
the synthetic Rust source, it asserts:

- `$ROOT/repos.yaml` contains both `id: bytes` and `id: tokio`.
- `$ROOT/workspaces.yaml` contains `name: tokio-stack` with both
  members.
- `$ROOT/bytes/Cargo.toml` is present.
- `$ROOT/tokio/Cargo.toml` is present.

`tests/js/graph_tab.test.js` and `tests/js/spa_e2e.test.js` are
untouched — they exercise the SPA itself, not the fixture data.

### `docs/` — README + QUICKSTART + command-center updated

The cross-repo walkthrough in `docs/QUICKSTART.md` and the federation
example in `README.md` keep their shape; only the example repo names
swap from `auth-svc` / `billing-svc` to `bytes` / `tokio`. The Tour in
`docs/command-center.md` says *"two repos by the same team that depend
on each other in production"* and remains literally true for the new
pair. Curl examples stop hardcoding a specific `bytes` symbol because
the symbol is now driver-picked from `find_anchors` at boot (see the
recording section above); the QUICKSTART example instead shows the
generic `find_anchors` + then `get_cross_repo_blast_radius` recipe.

No other doc file is touched.

### `Makefile` and `tests/js/package.json`

`make record-demo` / `npm run record-demo` continue to invoke the same
script. A new `make record-demo-small` target optionally invokes
`record-spa-demo.sh --fixture synthetic` for offline use, mirroring the
old behaviour. This keeps the small fixture available to anyone who
wants it without re-cloning GitHub.

## What does NOT change

- `src/server/mcp/command_center/**` (no SPA modifications).
- `src/server/federation/**` (no RepoSource, no config, no federation
  code changes).
- `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`, `docs/TECHNICAL.md`,
  `docs/hooks.md`, `docs/hot-reload.md`, `docs/multiplayer.md`,
  `docs/query-language.md`, `docs/quickstart-tools.md`,
  `docs/REPOS_YAML.md`, `docs/INDEX.md`, `docs/CI.md`,
  `docs/opinions/**`, `docs/srs/**`, `docs/wish-list.md`.
- `docs/screenshots/command-center-{overview,repos,tools}.png`.
- Any test file under `tests/**.rs`. The Rust unit and integration suites
  do not exercise the fixture; they exercise the SPA's pure helpers
  (`graph_tab.test.js`) and the binary's CLI (`cli_surface.rs`).
- `.github/workflows/**` (no CI change).

## Recording budget

| Recording step              | Was | Becomes | Notes |
|-----------------------------|-----|---------|-------|
| Federation ready wait       | ≤30 s | ≤90 s | cap matches `waitForReady`'s ceiling |
| Overview tab hold           | 4 s | 4 s | unchanged |
| Repos tab hold              | 3 s | 4 s | larger payload settles slower |
| Query tab form + hold       | 4 s | 6 s | JSON renders larger with ~5k hits |
| Tools tab form + hold       | 5 s | 6 s | cross-repo blast radius traverses larger graph |
| Graph tab force-layout hold | 5 s | 8 s | D3 alpha decay slows at higher node counts |
| **Total driving time**      | ~21 s | ~28 s | within the 45 s hero budget |

The orchestrator's `waitForReady` step has a 120 s cap; the per-step
`setTimeout` durations above add up to ~28 s, so the recording totals
under 90 s end-to-end on a fresh container, which is the budget we
target.

## Verification

Gates before declaring done, in order:

1. `./scripts/demo-federation-fixture.sh /tmp/lain-fixture-check` succeeds,
   `repos.yaml` lists `bytes` and `tokio`, both `Cargo.toml` files exist.
2. `bash scripts/smoke_federation_fixture.sh` passes with the updated
   assertions.
3. `make record-demo` (or its `--no-build` variant on a warm tree)
   produces `docs/screenshots/spa-demo.{webm,mp4,gif,poster.png}`,
   exits 0, every artifact within its existing budget
   (5 MB / 4 MB / 8 MB / 200 KB).
4. Frame check at t≈25 s into the resulting GIF: the Graph tab's metadata
   line begins with a node count ≥ 1500 (was 5), and at least one
   `graph-link cross-repo` line is visible in the canvas.
5. Frame check at the Tools-tab step (≈12-18 s): the
   `get_cross_repo_blast_radius` response contains the string `tokio`
   inside a `by_repo{}` block (proves the cross-repo edge was a real
   query result, not a synthetic stub).
6. `cargo test --test cli_surface` passes (commands table untouched).
7. `node tests/js/spa_e2e.test.js` and `node tests/js/graph_tab.test.js`
   still pass (SPA untouched).
8. Link check across all touched docs:
   ```bash
   grep -hoE 'docs/[a-zA-Z0-9_./-]+|screenshots/[a-zA-Z0-9_./-]+' \
       README.md docs/QUICKSTART.md docs/command-center.md \
     | sort -u \
     | sed 's|^|docs/|' | xargs -I{} test -e {}
   ```
   exits 0.

## Risk register

| Risk | Mitigation |
|------|------------|
| GitHub is unreachable at recording time | `git clone` fails with a clear message naming the URL. The orchestrator prints "network unreachable; rerun with `--no-clone` against a pre-populated `$WORK`" and exits non-zero. No silent fallback to the synthetic fixture. |
| `tokio-rs/tokio` ships a release that changes the call-graph structure | Fine — the recording only asserts a lower bound on node/edge counts and the presence of a `tokio`-named `by_repo{}` block in the blast-radius response. It does not assert exact values. |
| D3 force layout runs at >1 s/frame on a slow host and chokes the GIF budget | Per-step Graph-tab hold grows to 8 s (was 5). The 8 MB GIF cap and the `fps=12` retry already cover this. |
| `shallow_clone` of `tokio` produces too many symbols and gets server-truncated at the 5000-node / 10000-edge caps | Server marks the response `truncated: true`. The metadata string renders the "truncated" tag (already wired in `app.js:986`). The graph is still dense; truncation is honest, not a fault. |
| Recording bloats past 8 MB GIF cap | Existing retry-at-fps=12 fallback; hard cap at 12 MB remains. |
| A future `tokio-rs/bytes` rename breaks the recording | Both `id` and `repo id` strings are passed through one place (`scripts/demo-federation-fixture.sh`). If bytes renames, update the script and re-record. The Playwright driver uses `id` names, not `path` names. |
| Loss of `make record-demo-small` / `--fixture synthetic` workflow | The synthetic path stays available under `--fixture synthetic` for offline use. README / QUICKSTART only point at the default hero. |
| `tests/cli_surface.rs` or the README commands table drift because we mention new repo names | The commands table is checked against `lain --help`; fixture repo names are example arguments inside the federation walkthrough, not in the commands table. |

## Files touched

Created:

- (none — recording artifacts are re-emitted under existing paths)

Modified:

- `scripts/demo-federation-fixture.sh` — rewrites the fixture to clone
  `tokio-rs/bytes` + `tokio-rs/tokio` and emits their `shallow_clone`
  `repos.yaml`.
- `scripts/record-spa-demo.sh` — adds `--fixture`, `--no-clone`, and a
  per-clone 90 s timeout.
- `scripts/smoke_federation_fixture.sh` — updated assertions.
- `tests/js/record_spa_demo.js` — updated fixture-driven waits and tool
  calls (cross-repo target = top anchor of `bytes` repo picked at boot;
  repos tab = `bytes` + `tokio`).
- `Makefile` — adds `record-demo-small` alongside the existing
  `record-demo` target.
- `README.md` — federation example names swap from `auth-svc` /
  `billing-svc` to `bytes` / `tokio`.
- `docs/QUICKSTART.md` — same swap; the recorded-query example becomes
  the generic `find_anchors` + `get_cross_repo_blast_radius` recipe
  (no hardcoded `bytes` symbol).
- `docs/command-center.md` — Tour step 5 (Tools) names the
  `find_anchors`-then-`get_cross_repo_blast_radius` recipe instead of
  a hardcoded symbol.

Untouched (explicitly):

- `src/**` (no Rust code changes).
- `src/server/mcp/command_center/**` (no SPA changes).
- `docs/ARCHITECTURE.md`, `docs/USER_MANUAL.md`, `docs/TECHNICAL.md`,
  `docs/hooks.md`, `docs/hot-reload.md`, `docs/multiplayer.md`,
  `docs/query-language.md`, `docs/quickstart-tools.md`,
  `docs/INDEX.md`, `docs/CI.md`, `docs/opinions/**`, `docs/srs/**`,
  `docs/wish-list.md`.
- `docs/screenshots/command-center-{overview,repos,tools}.png`.
- `.github/workflows/**`.
- All Rust tests under `tests/**.rs`.
