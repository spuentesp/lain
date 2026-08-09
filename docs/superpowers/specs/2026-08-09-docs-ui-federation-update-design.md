# Federation Docs + UI Update — Design

**Status:** Draft (brainstorming complete, awaiting user review)
**Date:** 2026-08-09
**Scope:** Sub-project 1 follow-up — make the federation work (commit `429d33f` on `main`) discoverable and usable via updated docs and UI.

---

## Context

The federation indexer was implemented across 24 tasks and merged into `main` as commit `429d33f`. Five new MCP tools (`list_repos`, `get_repo_info`, `get_federation_health`, `search_org`, `get_cross_repo_blast_radius`, plus the `_for_repo` variant), a new `lain server` CLI subcommand, a `repos.yaml` config format, and a `FederationIndex` orchestrator are all in the codebase.

**However, none of the user-facing docs or UI surfaces mention the federation work.** A new agent or human reading `README.md`, `docs/TECHNICAL.md`, `docs/QUICKSTART_*.md`, or `docs/query-language.md` would have no idea that federation mode exists. The diagnostic page (`src/mcp/front_end_monitor.html`) and the three tool pages (`src/ui/blast-radius.html`, `call-chain.html`, `coupling.html`) are all single-workspace-only.

**Goal:** close the gap between shipped code and user-facing docs/UI without rearchitecting the federation work.

---

## Approach

**Approach 1 — Additive docs + new federation dashboard (chosen).**

Doc side: two new files (`docs/FEDERATION.md` is the central reference, `docs/REPOS_YAML.md` is the pure config schema) plus additive sections to existing docs. UI side: one new dashboard (`src/mcp/federation_dashboard.html`) plus per-tool repo_id selectors on the existing tool pages. Single source of truth per topic. Add `federation` blob to the `/health` endpoint so UI can render without extra round-trips.

The alternative approaches (rewrite of README/QUICKSTARTs around federation, or minimum-viable docs-only) were considered and rejected:
- Rewrite is bigger churn and harder to review.
- Docs-only leaves the UI federation-blind and agents can't discover the new tools.

---

## Architecture

### Docs

- `docs/FEDERATION.md` (~300-500 lines) — the central reference. Six sections:
  1. **Concept** — what federation is, single-workspace vs federation, when to use which.
  2. **Setup** — `repos.yaml` schema + `lain server` CLI flags.
  3. **Tools** — 5 new tools with examples + JSON shapes.
  4. **Performance** — 100ms p99 target, 200 repos / 30 min cold start, memory estimate.
  5. **Troubleshooting** — federation vs per-repo tools, `AmbiguousSymbol` resolution, `Unavailable` repos.
  6. **Migration** — notes for projects that had been using single-workspace.
- `docs/REPOS_YAML.md` (~150-250 lines) — pure config schema reference. Three sections:
  1. **Schema** — the YAML shape with each field.
  2. **Source kinds** — `local_clone` / `shallow_clone` / `workspace_dir` with when to use each.
  3. **Examples** — 5-10 worked configs of varying complexity.
- `README.md` — add a 2-3 paragraph "Federation mode" section after Quick Start pointing at `docs/FEDERATION.md`. Add a "federation mode" bullet to the feature list. Bump version 0.3.0 → 0.4.0.
- `docs/QUICKSTART_AGENTS.md` — add a "Federation mode" section with: "When to use federation vs single-workspace" decision table, 5 tool examples with realistic input/output, the `repo_id` resolution rule.
- `docs/TECHNICAL.md` — add a "Federation architecture" diagram + a "Cross-repo blast-radius semantics" paragraph.
- `docs/query-language.md` — one-line note pointing at `docs/FEDERATION.md`.

### UI

- `src/mcp/federation_dashboard.html` (new, ~200-300 lines, vanilla JS) — single landing page when running in federation mode. Shows: repo list (id, path, health, last-indexed), health summary (counts per `RepoHealth`), node/edge totals, 5 tool quick-links.
- `src/mcp/front_end_monitor.html` — add a federation header (when `federation` is set) showing N repos + health summary. Fall back to today's single-workspace layout when not.
- `src/ui/blast-radius.html`, `src/ui/call-chain.html`, `src/ui/coupling.html` — add a `repo_id` URL param + selector. Each tool already has a `symbol` param; `repo_id` is added alongside.

### Extension point

- `src/tools.rs:319` `get_agent_strategy` — extend the returned string to mention the 5 federation tools, the decision criteria, and the `repo_id` resolution rule. AI agents already call this first.

### Backend change

- `src/mcp/handler.rs` — the `GET /health` handler returns a `federation: { repos: [...], total_nodes, ... }` blob when the server is in federation mode, omitted (or `null`) when not. The UI pages hit this once on page load to decide which UI to render.

---

## Components

| File | What it owns | What it does NOT do |
|------|--------------|---------------------|
| `docs/FEDERATION.md` (new) | Federation reference doc | Per-tool recipes (those stay in QUICKSTART_AGENTS.md) |
| `docs/REPOS_YAML.md` (new) | Config schema reference | Tool semantics (those are in FEDERATION.md) |
| `README.md` | "Federation mode" section; feature list bullet; version bump | Rewrite existing sections |
| `docs/QUICKSTART_AGENTS.md` | "Federation mode" section with decision table + 5 tool examples | Setup docs (those are in QUICKSTART.md) |
| `docs/TECHNICAL.md` | "Federation architecture" + cross-repo blast-radius semantics | Full architecture rewrite |
| `docs/query-language.md` | One-line note | Document federation query language fully |
| `src/mcp/federation_dashboard.html` (new) | Federation landing page; routes calls to existing tools; redirects to `front_end_monitor.html` when not in federation mode | Render tool results (each tool page does that) |
| `src/mcp/front_end_monitor.html` | Federation header + single-workspace fallback | Federation tool rendering |
| `src/ui/blast-radius.html` | `repo_id` URL param + selector | Cross-repo walks (already does that) |
| `src/ui/call-chain.html` | Same | Same |
| `src/ui/coupling.html` | Same | Same |
| `src/tools.rs` | Extend `get_agent_strategy` string | Implement new tools (those already exist) |
| `src/mcp/handler.rs` | Add `federation` blob to `/health` response | New federation tools |
| `server.json` + `npm-shim/package.json` + `Formula/lain.rb` | Bump version 0.3.0 → 0.4.0 | Add new fields |

---

## Data flow

### User-facing federation flow (read path)

1. Operator writes `repos.yaml` referencing N repos.
2. Operator runs `lain server --config repos.yaml --transport http --port 9999`.
3. `load_federation` (already exists) clones/indexes each repo in parallel bounded by `max_concurrent_indexers`.
4. Server becomes ready when `ready_threshold` fraction of repos are `Ready`.
5. AI agent (or human) calls `list_repos` over MCP → returns `RepoInfo` list with health badges.
6. Agent calls `get_cross_repo_blast_radius` for a specific symbol → returns `CrossRepoBlastRadius` (results grouped by repo).
7. Agent calls `search_org` for a name → returns `SymbolMatch` list across repos.
8. The new `/health` HTTP endpoint returns `{ federation: { repos: [...], total_nodes, ... } }` plus existing fields. UI fetches this once on page load.

### Doc-discovery flow

1. New agent reads the project → sees `README.md` → "Federation mode" section says "see `docs/FEDERATION.md` and `docs/QUICKSTART_AGENTS.md`".
2. Agent opens `docs/QUICKSTART_AGENTS.md` → "Federation mode" section with decision table + 5 tool examples.
3. Agent calls `get_agent_strategy` first (existing convention) → now returns the federation tool list + decision table inline.
4. For deep-dive questions, agent follows the link to `docs/FEDERATION.md`.

### UI-fetch flow

1. Browser opens `http://localhost:9999/` → `front_end_monitor.html` loads.
2. JS fetches `/health` → checks `federation` field.
3. If federation is set: render the federation header (N repos + health badges + link to `/federation-dashboard.html`). Otherwise render the single-workspace UI as today.
4. User clicks "repo-a" → opens `/federation-dashboard.html` → JS fetches `list_repos` (via HTTP/JSON-RPC `tools/call`) → renders table.
5. User clicks "blast radius" on a row → navigates to `/ui/blast-radius/?repo_id=repo-a&symbol=foo` → JS fetches `get_cross_repo_blast_radius_for_repo` → renders.

### Repo ID resolution (the gotcha)

The `repo_id` resolution rule from `src/mcp/handler.rs:resolve_repo_for_tool`:

1. If `repo_id` is explicit → use it.
2. Else if `symbol` is given → look it up across all repos (single match → use that repo, multiple → `AmbiguousSymbol`, none → `NotFound`).
3. Else if 1 repo registered → use it.
4. Else if 0 repos → `Config("no repos registered")`.
5. Else if multiple repos → `Config("multiple repos; specify repo_id or symbol")`.

Documented in `docs/FEDERATION.md` "Tool resolution rules".

---

## Error handling

### The 5 federation tools' error surfaces (already implemented)

| Tool | Error cases | Surface |
|------|-------------|---------|
| `list_repos` | none (always returns the current set) | — |
| `get_repo_info` | Unknown repo id | `LainError::NotFound` → `isError: true` text payload |
| `get_federation_health` | Backend construction failure | `OtherError` text |
| `search_org` | Empty `query` → `Missing required argument: query`; empty `limit` → `Missing required argument: limit`; non-numeric `limit` → `Invalid argument: limit must be a non-negative integer` | per HTTP/JSON-RPC error format |
| `get_cross_repo_blast_radius` | Symbol not found → `NotFound`; ambiguous symbol → `AmbiguousSymbol` (JSON via `resolve_repo_or_error`); no repos → `Config` | per HTTP/JSON-RPC error format |
| `get_cross_repo_blast_radius_for_repo` | Same as above + bad `repo_id` → `Invalid repo id` | per HTTP/JSON-RPC error format |

### The `lain server` CLI's error surface

- `repos.yaml` not found → `IO("read config: ...")` with the path.
- Invalid YAML → `Config("yaml: ...")` with line/column.
- A source's URL is empty → `Config("...")`
- `load_federation` errors → propagated to the CLI exit code 1.

### Doc-side error handling

Each error case gets a short note in `docs/FEDERATION.md` "Troubleshooting" with: what it means, how the agent should handle it (e.g., "on `AmbiguousSymbol`, present the candidates to the user and ask them to pick"), what to do if it persists.

### UI-side error handling

- The federation dashboard, when a `tools/call` returns `isError: true`, renders the error text in a dismissable banner. No JS `alert()`.
- `repo_id` selector keeps the previously-selected value when an error happens (don't reset to default).

### Versioning

`server.json`, `npm-shim/package.json`, and `Formula/lain.rb` version bumps will be 0.3.0 → 0.4.0, all in a single commit at the end of the implementation plan. Federation is the headline feature. Old single-workspace mode is unchanged so existing consumers can upgrade without changes.

### Constructor-signature deviation (from the merge)

The merge dropped `repo_id` resolution handling for the per-repo tool path (Task 19 review fix). The new `repo_id` selectors in `src/ui/blast-radius.html` etc. give **users** a way to specify a repo, but until the existing per-repo handlers (e.g. `GetBlastRadiusHandler` in `src/tools/handlers/`) read `repo_id` from args, the resolved id is discarded. This is a known follow-up; documenting it here so it doesn't get lost. The federation path (new tools) does the right thing; the back-compat shim is incomplete on the single-workspace handler side.

---

## Testing

### Doc tests (lightweight, no Rust)

Each new doc has a "Smoke test" section with 2-3 commands an operator can run to verify the doc is accurate. E.g., `docs/FEDERATION.md` includes:

```bash
curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
     -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_repos","arguments":{}},"id":1}' \
     | jq .result.content[0].text
```

CI runs `bash -n` on each smoke-test shell block to catch syntax errors.

### UI tests (e2e shell script)

New `tests/e2e/federation_dashboard_e2e.sh` (mirrors `tests/e2e/federation_e2e.sh`):

- GET `/` → expect HTML with the federation header (`health: ready` for each repo).
- GET `/federation-dashboard.html` → expect HTML with the repo table.
- GET `/ui/blast-radius/?repo_id=hello-rust&symbol=hello` → expect HTML with the blast radius render.

The script uses `python3` for JSON parsing (jq may not be installed).

### `get_agent_strategy` content test

A new unit test in `src/tools.rs` (or wherever the strategy string is built) that asserts the returned string contains the 5 federation tool names. This catches accidental regressions when the federation tools are renamed/refactored.

### Version-consistency test

A small `tests/version_consistency.rs` (or shell test) that asserts `server.json`, `npm-shim/package.json`, and `Formula/lain.rb` all have the same version. Catches the "bumped one but not the others" bug we almost hit during the merge.

### Existing test surface (unchanged)

All 461 lib tests + 7 integration + e2e continue to pass. The version bump is a content change, not a behavior change. The `get_agent_strategy` extension is additive — existing tests don't break.

---

## Out of scope (deferred)

- **Per-repo handler reading of `repo_id`** (the constructor-signature gap from the merge): deferred to a follow-up. Federation path is correct; per-repo handlers will need updates to read `repo_id` from args.
- **Multiple-instance federation (sub-projects 4-7 from the original vision)**: not in scope. This is docs + UI for the shipped federation work.
- **Workspace UI redesign** (e.g., moving from `front_end_monitor.html` to a SPA framework): explicitly out of scope. Vanilla JS only.
- **Internationalization**: not in scope. English docs only.

---

## Effort estimate

Roughly 1-2 weeks for one engineer. Breakdown:
- `docs/FEDERATION.md` + `docs/REPOS_YAML.md`: 1-2 days (writing + smoke tests).
- README + QUICKSTART + TECHNICAL + query-language updates: 1 day.
- `src/mcp/federation_dashboard.html` (new): 1 day.
- `src/mcp/front_end_monitor.html` federation header: 0.5 day.
- 3× `src/ui/*.html` repo_id selectors: 0.5-1 day.
- `get_agent_strategy` extension + unit test: 0.5 day.
- `/health` response extension + federation e2e test: 0.5 day.
- Version bumps + consistency test: 0.5 day.

Total: ~6-7 working days. No new external dependencies. No new test infrastructure. The dashboard uses vanilla JS to match the existing pages. The version bump is a content change with no runtime impact.
