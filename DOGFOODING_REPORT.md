# Dogfooding Lain on Lain — Findings & Plan (2026-10-04)

## Setup

| Step | What | Result |
|------|------|--------|
| 1 | Branch | `dev` @ `3105a665` (16 commits past v0.8.0) |
| 2 | Source | 258 `.rs` files, 41,358 lines of Rust |
| 3 | Build | `cargo build` (debug) — finished in 1m 48s; `target/debug/lain` = 540 MB |
| 4 | Existing state | `.lain/graph.bin` was 3.3 MB but `bincode` decode failed: `UnexpectedEnd { additional: 1786672818 }` (truncated, torn write) |
| 5 | Recovery | None — only option was a full reindex from source |
| 6 | Cold reindex | ~5 min (41k LOC × 374 Rust files + LSP prewarm) on the dev binary |
| 7 | Persistent server | custom `lain-cli2.py` driving `lain mcp` over stdio (no `oneshot` per-call cost) |

Final graph state (after the cold reindex):

```
Static Nodes:    10,087
Static Edges:    10,781
Last Enriched:   3105a665925234115b1f9602b34a2eb6f8d7eaa1 (current)
NLP Model:       not loaded (semantic search unavailable)
Edge breakdown:
  CallsHttp      36
  CoChangedWith  751
  Contains       9,955
  Produces       8
  ReadsTable     5
  SendsHttp      23
  Uses           1
  WritesTable    2
                ─────
  total         10,781

  Calls              0   ← problem
```

## Bugs / friction found while dogfooding

### B1. Cold reindex is required for every one-shot query
`lain oneshot` spawns a fresh `lain mcp` per call. Each new server enters
`warming_up` (per `get_capabilities`), and the gate returns a partial /
empty answer when the graph is "still indexing". On a 41k-LOC repo with
LSP-prewarm, that reindex is ~5 minutes.

A persistent `lain mcp` (driven over stdio) returns the answer in 4 s.
**The Quickstart and README promise oneshot ergonomics, but the
per-call cost makes the "5-tool pipeline = 25 minutes" reality for
any non-trivial repo.** A user who follows the Quickstart
(`lain oneshot find_anchors`) gets an answer in seconds the first time,
then waits 5 minutes for the second one.

**Two views, both correct, but they disagree on whether the index is ready:**
- `get_health`:  "Status: Operational ✅ · Static Nodes: 10087"
- `get_capabilities`:  `state: "warming_up" · files_completed: 0 · phase: "discovering"`

### B2. `find_git_workspace_root` skips the source tree when the binary lives in it
`/home/spuentesp/.local/bin/lain` is a symlink → `/data/agents/orca/lain/target/debug/lain`.
`std::env::current_exe()` follows the symlink; `binary_lives_inside(found_root)`
returns `true`; the walker skips the only valid candidate and returns `None`.

**Why this matters:** the dev-runner heuristic (`src/cli/workspace.rs:31`)
is *intended* for `cargo test` / `cargo run`. But it also fires for any
dev install that puts the binary inside the source tree via a symlink.
The workaround is `--workspace PATH` every time, or copy the binary
outside the tree. The error message ("no `.git` found in any parent
directory") is misleading.

**Fix shape:** when `current_exe()` is a symlink, also check the
*symlink target*. If the symlink target is outside the source tree
(e.g. `~/.local/bin/lain -> .../target/debug/lain`), the binary is a
*published* dev install, not a `cargo run`. Or, more simply: a
`cargo run` test would have the working tree at a different path than
`current_exe()` (because cargo's test runner is in `target/`); the
heuristic should compare `current_dir()` to the dev tree, not the
binary path.

### B3. `Calls` edges not populated on a 374-file Rust workspace
`describe_schema` declares `Calls` as a real edge type, but the index
has **zero `Calls` edges**. `get_blast_radius get_blast_radius`
returns `(no dependents found — symbol may be a leaf or not yet
indexed)`. `get_call_sites get_blast_radius` says
`No call sites found for 'get_blast_radius' (get_blast_radius is a
leaf)` — even though `get_blast_radius` is one of the most-called
handlers in the codebase.

The top `find_anchors` results include Python test fixtures
(`uc_presence_register_heartbeat_unregister`,
`e_setup_writes_prompt_md`) but the actual `bfs_traverse` (the BFS in
the query executor) ranks *below* the test fixtures. The agent that
trusts rank over path lands on a test, not an architectural anchor.

`rust-analyzer` is listed as installed and ready in `get_health`. The
LSP phase is either not running or failing silently. The `Calls`
edges are not falling back to the tree-sitter heuristics either —
otherwise there would be *some*.

`get_health` reports the 8 edge types it knows about (CallsHttp,
CoChangedWith, etc.) but never `Calls` itself, so the silent
absence is invisible in the health report. There is no "Calls: N"
line anywhere in the diagnostic surface — the *only* way to notice
is to query a tool that depends on it (which is most of them).

### B4. `find_dead_code` reveals the indexing gap
`find_dead_code` returns 1 actual dead code (`poll_frame` in
`src/server/mcp/overlay_sse.rs`) but flags a much bigger issue:

> ⚠ 198 file(s) have definitions but no call edges at all — their
> call graph could not be extracted, so 3987 symbol(s) in them were
> excluded rather than reported as dead. This is an indexing gap,
> not dead code.

The list is dominated by `scripts/*.py`, `scripts/*.sh`, `tests/*`.
For these files, `get_health` says "Python: parser ✅" but the
parser clearly isn't producing `Calls` edges. The 198-file gap is
the *visible* shape of B3.

### B5. Truncated `graph.bin` is full-rebuild-only
The on-disk format is bincode 2.x with a strict length-prefix
decoder (`DECODE_LIMIT: usize = 1 << 32`). A truncated file produces
a hard error: `cannot decode graph: UnexpectedEnd { additional:
1786672818 }`. The recovery path is "move it aside, run `lain mcp`,
wait for full reindex." No incremental recovery; no way to keep
partial state; no incremental write-ahead log. The `.lain/` dir
also holds `events.jsonl`, `locks/`, and `data/repos/lain/` —
deleting `.lain/graph.bin` is fine, but a hand-rolled "rm" recovery
script can accidentally nuke the rest.

### B6. `LAIN_ONESHOT_TIMEOUT` default 60s is too short
`LAIN_ONESHOT_TIMEOUT` defaults to 60s. The Lain repo's cold reindex
takes ~5 minutes. The error (`no tools/call response from lain mcp
within 60s`) doesn't distinguish "server is busy indexing" from
"server is hung" — the agent has no signal. The 5-minute reindex is
**silent** to the user; they have to set `RUST_LOG=lain=debug` to
see anything happen.

### B7. `head -N` in a pipe kills the indexer
Not a Lain bug per se, but a dogfooding footgun: piping `lain
oneshot ... | head -60` closes the upstream when `head` exits, which
sends SIGPIPE to the long-running `lain mcp` process and aborts the
reindex mid-flight. The 540 KB graph.bin I observed on disk after
the first failed run is a partial write from this scenario. This
class of footgun is invisible in the docs.

### B8. Test fixtures inflate anchor / entry-point scores
The top `find_anchors` includes `uc_presence_register_heartbeat_unregister`
in `scripts/use_cases_e2e.py` (a test) and `e_setup_writes_prompt_md`
in `scripts/test_all_promises.py` (a test). `list_entry_points`
returns 31 `main` entries; `main (src/bin/lain-git-sidecar.rs)`
appears 3 times. The rank-by-name heuristic the docs warn about
("Results are deduped by name keeping the best-scoring definition")
treats tests as the best-scoring definition when tests are the most
caller-heavy.

A user who runs `find_anchors` lands on a test fixture. A user who
runs `list_entry_points` lands on a script. The intended consumer of
these tools — a coding agent — silently learns the wrong "entry
point" of the system.

### B9. Tool argument name disagrees with the docs
`get_coupling_radar`'s argument is `symbol` (per the input schema),
but the quickstart-tools.md header reads:

> Files that co-change with this one.
> `{ "name": "get_coupling_radar", "arguments": { "symbol": "my_file.rs" } }`

The example is `symbol` — that part is right. But the section
heading "Files that co-change with this one" suggests `path` or
`file`. The fast path that grep's for "get_coupling_radar" lands on
"this one" (not "this symbol") and tries `path` first. The error is
clear (`Missing required argument: symbol`) but the docs could
disambiguate.

### B10. `events.jsonl` is per-repo and ephemeral
There's a presence/coordination event log (5 entries from a previous
`eval-agent-lain` run) but it's not retained, not queryable, and not
exposed via MCP in a useful way. For "what changed while I was
away?" the agent has to grep the git log, not the presence log.

### B11. `describe_schema` advertises edge types the indexer can't produce
`describe_schema` lists `Calls` as a real edge type with description
"A function calls another function", but on a single-repo Rust
workspace with rust-analyzer installed, the indexer produces zero of
them. The schema says "yes" and the index says "no" — a contract
violation the user has no way to notice until they query a tool
that depends on it.

---

## Plan A — Distillation for a PR comment

The contracts package already gives a `diff_contracts(base, head)`
flow (`list_services` → `get_service` → `prepare_snapshot` × 2 →
`diff_contracts`). For single-repo PRs, the same shape can be filled
with non-federation tools. A single PR-comment block should distill:

### Top-level signal

| Section | Source tool(s) | What the reviewer learns |
|---|---|---|
| **Risk score** (one line) | synthesized from below | "Low / Medium / High" with reason |
| **Direct dependents** (must update) | `get_blast_radius` direct-only | "These N functions are guaranteed affected" |
| **Indirect dependents** (FYI) | `get_blast_radius` indirect | "These M functions *might* be affected, by depth" |
| **Coupling partners** (review those) | `get_coupling_radar` for each changed file | "These files have changed with yours in N% of recent commits" |
| **Test coverage delta** | `get_coverage_summary` on changed modules | "+X% / -Y% lines covered" |
| **New untested functions** | `find_untested_functions` ∩ changed | "PR adds N functions with no callers / no tests" |
| **Test template** (auto-generated) | `get_test_template` for new fns | Inline scaffold, one per new fn |
| **Dead-code introduction** | `find_dead_code` after a fresh index | "PR adds 4 symbols with 0 textual references" (B4 caveat applies) |
| **Architectural observations** | `architectural_observations` | "Module X is now above the fan-out threshold" |
| **Federation impact** | `get_cross_repo_blast_radius` (federation only) | "3 other repos in the org reach this" |
| **Suggested reviewers** | git co-change + `get_commit_history` | "Last 5 PRs touching these files were reviewed by X" |
| **Churn signal** | git history | "This file is touched every 2 days; churn is high" |
| **Diff vs base** (semantic) | `diff_contracts` (federation) | "Public surface changed: 2 endpoints, 1 topic" |
| **Stability of changed area** | existing anchor score | "Anchor score 87 — this is a foundational change" |
| **Layer violation** | `get_layered_map` + diff | "New edge crosses the server/cli layer boundary" |
| **New anchors** | `find_anchors` diff | "After this PR, fn X is a new top-5 anchor" |
| **Indexing gap** | `find_dead_code` ⚠ line | "This PR adds code in N files where call graph is partial" |

### Single-line "why this matters" each

- **Direct dependents** — act on these (update tests, bump version, etc.)
- **Indirect dependents** — informational, group by depth
- **Coupling partners** — review the diff in lockstep
- **Untested functions** — gap to close before merge
- **Risk score** — gates required-reviewers / auto-merge
- **Federation impact** — fan-out beyond the repo is a release-coordination cost

### Collapsing into one comment

For small PRs, the entire block fits in a single collapsible
`<details>` section. For large PRs, the headline is the risk score
+ direct dependents; the rest goes in a table that the reviewer
expands.

### Pre-flight checks the distiller should do

Before any of the above, the distiller needs to *verify* the
prerequisites — call out loud if they fail:

- [ ] `get_capabilities.call_graph.state == "ready"`
- [ ] Edge counts include `Calls > 0` (B3)
- [ ] `find_dead_code` has no `⚠ N file(s) have definitions but no
  call edges` line (B4)
- [ ] Anchor scores not dominated by test fixtures (B8) — filter
  `scripts/**` and `tests/**` from the top-N

If any of these fail, the distiller should NOT report the dependent
synthesis as a confidence-bearing verdict. It should say "the index
is partial; here is what we can answer, with caveats."

---

## Plan B — CI as a warning / code quality tool

Three integration surfaces, in increasing depth of integration:

### B-1. Comment-only (no fail)
- A bot that runs `lain oneshot` on the diff and posts a PR comment.
- Always posts; never blocks. This is the Quickstart ergonomics.
- Good for: surfacing hidden blast radius, missing tests, churn signals.
- Cost: each run is a full reindex (B1). Cache the graph between
  PRs by SHAs the way `prepare_snapshot` already does — that gives
  diff-vs-base without re-scanning the whole repo.
- A persistent `lain mcp` running on the CI runner, kept warm
  across jobs, drops this to a few seconds per query.

### B-2. Required-check (fail on threshold)
- Same bot, but exit code 1 on a "high risk" verdict.
- Threshold knobs (org-default in the Lain repo, per-repo
  override in `.lain/ci.toml`):
  - `max_blast_radius_direct: 10` (raise a flag if more)
  - `max_new_untested: 0` (every new fn needs a test)
  - `max_new_dead_code: 0`
  - `max_coupling_partner_unanalyzed: 5` (if the diff touches a
    file whose co-change partners aren't in the diff, warn)
  - `min_coverage_delta: 0` (don't allow coverage to drop)
- The CI step is `cargo install lain && lain ci-check` or
  `npx @spuentesp/lain-mcp ci-check`.
- The distiller produces a single JSON; the CI step reads it and
  applies the thresholds.

### B-3. Continuous code quality (always on, org-wide)
- A daemon that watches the federation: every push on every repo
  re-runs the analysis, and the result is queryable from
  `search_org` / `get_service` / `get_coverage`.
- This is essentially "the contracts package but for every PR, not
  just the contract view" — same architecture, but a broader
  surface.
- The `RepoSource` trait already supports the right model; the
  orchestrator just needs to schedule per-PR jobs.

### Operational considerations (apply to all three)

| Concern | Today | Plan |
|---|---|---|
| Cold reindex per run | every `oneshot` pays 5 min on 41k LOC | `prepare_snapshot`-style cache: base + head are pinned; only changed files re-scan (B1) |
| `Calls` edges missing | B3 | `run_enrichment` after the LSP-prewarm phase, with a `Calls: N` line in `get_health` so the failure is visible (B11) |
| Truncated graph recovery | full reindex | WAL / append-only checkpoint, with a "recover from checkpoint" tool (B5) |
| Timeout surface | 60s default | 600s default for first call, 60s for warm graph; surfaced via `get_indexing_progress` (B6) |
| `binary_lives_inside` skip | dev runner only | symlink-aware check (B2) |
| Anchor-score inflation by tests | top anchors are test fixtures (B8) | `find_anchors --exclude-glob 'scripts/**'` flag, or weight by inverse-test-call count |
| Indexing-gap visibility | `find_dead_code` ⚠ line, but no `Calls` count in `get_health` (B4) | surface a `coverage.call_graph` field in `get_health` and gate CI on it |
| Pipe footgun | `head -N` kills the indexer (B7) | add a `--tee /tmp/lain.log` flag and a one-liner warning in the error message |
| Doc/tool mismatch | `get_coupling_radar` (B9) | lint the docs against the input schema in CI |
| Events retention | not queryable (B10) | `get_audit_log` already exists in the `social` package; promote it to `core` for solo sessions |

### What a `lain ci-check` invocation would look like

```bash
# CI step (run on every PR):
npx @spuentesp/lain-mcp ci-check \
  --base origin/main --head HEAD \
  --config .lain/ci.toml \
  --output comment.md
# Always writes a markdown PR comment file; exits non-zero on threshold breach.
```

The comment is the Plan-A distillation; the exit code is the
threshold check. Both come from the same JSON.

---

## What I'd build first

If I had to ship one thing in the next week: **B-1 (the comment-only
bot)**. It has the highest ratio of value to implementation cost,
and the deficiencies it surfaces (B3, B4, B6, B8) are the most
embarrassing when they hit a public PR. The B-2 thresholds fall out
naturally once the single-PR distillation works.

The first 5 dogfooding bugs above (B1–B5) are the blockers for a
useful bot:

- **B3** must be fixed first, otherwise the bot's blast-radius
  section is always empty.
- **B1** must be fixed second, otherwise the bot costs 25 minutes
  per PR.
- **B5** is a nice-to-have, but a 5-minute reindex is acceptable
  for a v1 if it's well-logged.
- **B4** and **B8** are visible to the user and need a clear
  "this is a known gap" message.

Everything else (B6, B7, B9, B10, B11) is polish.

---

## Appendix — exact queries that revealed each finding

| # | Tool | Args | Outcome that revealed the bug |
|---|---|---|---|
| B1 | `get_capabilities` (via custom driver) | `{}` | `"state": "warming_up"` despite 10,087 nodes on disk |
| B2 | `lain oneshot` | (none) | "no `.git` found in any parent directory" from inside a git repo |
| B3 | `get_blast_radius` | `get_blast_radius` | "(no dependents found — symbol may be a leaf or not yet indexed)" |
| B3 | `get_call_sites` | `get_blast_radius` | "No call sites found ... (get_blast_radius is a leaf)" |
| B3 | `get_health` | (none) | Edge counts never include `Calls` (silent absence) |
| B4 | `find_dead_code` | `{}` | "⚠ 198 file(s) have definitions but no call edges at all" |
| B5 | `lain doctor` | (none) | "cannot decode graph: UnexpectedEnd { additional: 1786672818 }" |
| B6 | `lain oneshot` | `get_health` | "no tools/call response from `lain mcp` within 60s" (Lain cold reindex ≈ 5 min) |
| B7 | `lain oneshot ... \| head -60` | (first call) | Bash killed the indexer mid-reindex; partial 540 KB graph.bin |
| B8 | `find_anchors` | `limit=10` | Top 2 are Python test fixtures |
| B8 | `list_entry_points` | `{}` | 31 `main` entries, several duplicated (same file listed 3×) |
| B9 | `get_coupling_radar` | `{path: ...}` | "Missing required argument: symbol" |
| B10 | `cat .lain/events.jsonl` | (n/a) | 5 events from a previous run, no MCP query surface |
| B11 | `describe_schema` | (none) | "Calls" listed as real, but `get_health` shows 0 |
