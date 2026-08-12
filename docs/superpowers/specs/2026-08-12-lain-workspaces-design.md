# Lain Workspaces — Design

**Status:** Draft (brainstorming complete, awaiting user review)
**Date:** 2026-08-12
**Sub-project:** standalone
**Depends on:** [`docs/superpowers/specs/2026-08-11-federation-test-gap-fix-design.md`](file:///home/sebastian/lain/docs/superpowers/specs/2026-08-11-federation-test-gap-fix-design.md) (test-gap fix must land first — see "Prerequisite" below)
**Enables:** future features that assert cross-workspace reasoning, multi-tenant federation use

---

## Context and motivation

Lain's federation mode (`lain server --config repos.yaml`) answers org-wide structural questions across N repos. Today, the only way to scope a federation is to edit `repos.yaml` to point at fewer repos, restart the server, and accept the rest. There's no way to say "these N repos are *my team*" or "this subset is *the payments workspace*" — operators who care about a subset have to maintain a separate config and remember which one is loaded.

The user wants to evolve federation into a **multi-workspace** mode: a named group of repos that the federation engine indexes together as a coherent unit, switchable on server restart. A workspace is the named subset operators already think in ("backend-team", "frontend-monorepo", "payments").

The future Workspaces feature sits on top of federation. Before we build it, the federation substrate must produce and traverse cross-repo `Calls` edges end-to-end. That's the test-gap spec — currently has its first failing test committed on `federation-test-gap-pr1`; the rest of the test-gap work is blocked on a working env with `rust-analyzer`. **This workspace spec should NOT proceed to implementation until the test-gap fix lands.**

---

## Goals

1. **Named workspace groups.** An operator can declare a workspace as a `Vec<RepoId>` referenced from `repos.yaml` and identified by a stable name.
2. **Switchable at server startup.** `lain server --config repos.yaml --workspace <name>` starts the federation holding only that workspace's repos. No `--workspace` → today's behavior (all repos).
3. **CLI for workspace CRUD.** `lain workspaces create / add / remove / import / init / list / show / use / current / forget` covers the full lifecycle without touching YAML by hand.
4. **Federation dashboard surfaces the workspace.** Repos loaded, workspaces that exist, the active one, and a per-workspace graph view that shows real cross-repo `Calls` chains.
5. **No regression.** Single-workspace mode (`lain --workspace <path>`) is unchanged. Existing `repos.yaml` schemas are unchanged. The 6 federation MCP tools are unchanged in their signatures or behavior.

## Non-goals

- **Hot-reload of active workspace.** Switching requires a server restart. A separate spec if needed.
- **Cross-workspace composition.** A workspace can't include another workspace; no `includes: [other-ws]` field. Flat lists only.
- **Single-repo workspaces.** Workspace ≥ 2 repos; a 1-element workspace is a config error. Solo work stays on `--workspace <path>`.
- **Multi-tenant ACLs.** All clients see all repos. Federation is single-tenant; ACLs are a deferred sub-project.
- **Hot-add/remove repos from a running workspace.** Workspace membership is fixed at server startup. Adding/removing repos requires a restart with the updated `workspaces.yaml`.
- **New MCP tools for write operations.** `lain workspaces add/remove` edits `workspaces.yaml` directly via the CLI; not exposed via MCP (no remote mutation through agents).
- **Per-language/per-repo tool tuning.** Workspace just scopes which repos are loaded; per-repo indexing behavior is unchanged.

---

## Architecture

A workspace is a **named subset of repos from `repos.yaml`**. The federation server holds exactly one workspace's repos in its `FederatedIndex` at a time. Switching workspaces tears down the current federation and rebuilds it with the new subset.

```
                 ┌────────────────────────────────────────────────────┐
                 │  lain (one process, one active workspace)         │
                 │                                                    │
   workspaces.yaml│   ┌────────────────────────────────────────────┐  │
   ──────────────▶│   │  WorkspaceIndex (new, thin layer)          │  │
                 │   │  - active_workspace: WorkspaceName          │  │
                 │   │  - members: Vec<RepoId>                      │  │
                 │   │  - status: Active | Switching | Empty       │  │
                 │   └──────────────┬─────────────────────────────┘  │
                 │                  │ filters                          │
                 │                  ▼                                 │
   repos.yaml ──▶│   ┌────────────────────────────────────────────┐  │
                 │   │  FederatedIndex (existing, untouched)       │  │
                 │   │  - holds only repos in active workspace     │  │
                 │   │  - all existing federation tools unchanged  │  │
                 │   └────────────────────────────────────────────┘  │
                 └────────────────────────────────────────────────────┘
                                     ▲
                                     │ MCP (stdio / HTTP)
                                     │
                          ┌──────────────────────────┐
                          │  AI Agent (Claude, etc.) │
                          └──────────────────────────┘
```

`WorkspaceIndex` is a new layer that filters the `FederatedIndex`'s universe. Everything below `FederatedIndex` is reused as-is: `RepoSource`, `GraphBackend`, `find_cross_repo_matches`, `resolve_symbol`, `get_cross_repo_blast_radius`, the resolver rules, the 1000-node cap. No federation engine changes.

---

## Prerequisite

**The federation test-gap fix must land first.**

[`docs/superpowers/specs/2026-08-11-federation-test-gap-fix-design.md`](file:///home/sebastian/lain/docs/superpowers/specs/2026-08-11-federation-test-gap-fix-design.md) — currently has Task 1's test committed on `federation-test-gap-pr1` and Tasks 2–12 unverified (sandbox blocked on rust-analyzer). Whoever runs the plan in a working env completes it first.

Without that fix, `project_repo` doesn't produce cross-repo `Calls` edges, so the workspace graph view shows only intra-repo edges and the headline cross-repo blast-radius-within-workspace semantic is broken.

---

## Components

### Workspace sources (mirrors `RepoSource`)

```yaml
# WorkspaceSourceConfig — same tagged-enum shape as SourceConfig
- name: backend-team
  source:
    type: workspace_dir
    path: /srv/workspaces/backend-team   # directory containing workspaces.yaml
- name: payments-ws
  source:
    type: workspace_clone      # new
    url: https://github.com/acme/payments-ws.git
    ref: main
```

| `type` | What it does |
|---|---|
| `workspace_dir` | Reads `workspaces.yaml` from a local path. No git ops — like `WorkspaceDirSource` for repos. |
| `workspace_clone` | `git clone`s the URL into `$LAIN_HOME/workspaces/<name>/` on first run, then `git fetch && reset --hard origin/<ref>` on subsequent loads. Same `refresh_interval_secs` knob as `shallow_clone`. |

A workspace's own `workspaces.yaml` (inside a `workspace_clone`) contains its member-repo specs — those repo ids must still exist in the user's `repos.yaml` to be resolvable.

### `workspaces.yaml` schema

```yaml
default: backend-team                      # optional; what `lain workspaces use` defaults to
workspaces:
  - name: backend-team
    description: Core backend services    # optional
    source:                                # optional; where the canonical spec lives
      type: workspace_dir
      path: ./workspaces/backend-team.yaml
    members:
      - auth-svc          # must exist as `id` in repos.yaml
      - billing-svc
      - notifications-svc
      - db-client
  - name: frontend-monorepo
    source:
      type: workspace_clone
      url: https://github.com/acme/fe-workspace.git
      ref: main
    members:
      - web
      - mobile-bff
      - shared-ui
```

### CLI surface (full CRUD)

```
lain workspaces create <name> [--description <text>] [--members repo,repo,...]
                                   Create an empty (or pre-populated) workspace
                                   in the current workspaces.yaml.

lain workspaces add <name> --repo <repo-id>
                                   Append a repo to a workspace's members.
                                   (Repo id must already exist in repos.yaml;
                                   errors with a clear message if not.)

lain workspaces remove <name> --repo <repo-id>
                                   Remove a repo from a workspace's members.

lain workspaces import <name> --from <dir>
                                   Read workspaces.yaml from <dir>, merge the
                                   named workspace into the local workspaces.yaml.

lain workspaces init <name> --from <git-url> [--ref <branch>]
                                   Clone a workspace definition repo and
                                   register it (workspace_clone source kind).

lain workspaces list              Show all known workspaces + member count.
lain workspaces show <name>       Show full spec (members, source, description).
lain workspaces use <name>        Set active workspace; writes
                                   ~/.config/lain/active_workspace.
lain workspaces current           Print active workspace.
lain workspaces forget <name>     Remove a workspace from workspaces.yaml.
```

`lain server` picks up the changes: every workspace CRUD operation rewrites `workspaces.yaml` atomically, and the active workspace pointer in `~/.config/lain/active_workspace`. To pick up a structural change (different members), the server has to reload — see "Switching" below.

### Server entry point

```bash
# Today's behavior, unchanged: all repos from repos.yaml
lain server --config repos.yaml --transport http --port 9999

# NEW: federation holding only the named workspace's repos
lain server --config repos.yaml --workspace <name> --transport http --port 9999

# NEW: use whatever's in ~/.config/lain/active_workspace
lain server --config repos.yaml --workspace auto --transport http --port 9999
```

Resolution order for `--workspace`:

1. Explicit name → look up that workspace in `workspaces.yaml`
2. `auto` → read `~/.config/lain/active_workspace`
3. Missing → today's behavior (all repos in `repos.yaml`)

When a workspace is active, the server filters `repos.yaml` to only the workspace's `members` and starts the federation engine on that subset. Any `RepoSource::fetch()` failures or `Degraded` health for non-member repos are irrelevant — they're not in scope.

### Switching semantics

Hot reload is **not** in scope. Switching workspaces requires a server restart:

- `lain workspaces use <name>` writes `~/.config/lain/active_workspace`
- The running server keeps its current workspace until restart
- Operators use their existing supervisor (systemd, docker restart policy, tmux) to bounce the server when the active workspace changes
- **No new moving parts.** Keeps the change to "thin wrapper."

### MCP tool surface

The 6 federation tools (`list_repos`, `get_repo_info`, `get_federation_health`, `search_org`, `get_cross_repo_blast_radius`, `get_cross_repo_blast_radius_for_repo`) are **completely unchanged** — same handlers, same arguments, same return shapes. They just operate over the active workspace's repo subset.

**Three new tools, all read-only:**

| Tool | Args | Returns | Purpose |
|---|---|---|---|
| `list_workspaces` | none | `[{name, description?, source?, member_count, is_active}]` | List all known workspaces from `workspaces.yaml`. `is_active: true` on the active one. |
| `get_active_workspace` | none | `{name, members: [repo_id], source?}` | The workspace the server is currently holding. Errors with `NoActiveWorkspace` if the server was started without `--workspace`. |
| `get_workspace` | `name` | `{name, description?, source?, members: [{repo_id, path, health}]}` | Full detail on one workspace, including resolved repo paths + health. Errors with `NotFound: workspace <name>`. |

**One dashboard-only tool:**

| Tool | Args | Returns | Purpose |
|---|---|---|---|
| `get_workspace_graph` | `filter?: string` | `{nodes: [...], edges: [...]}` | Node + edge data for the workspace graph view in `federation_dashboard.html`. Only registered when a workspace is active. |

All CLI workspace mutations live in the `lain workspaces ...` subcommand — they edit `workspaces.yaml` directly. They're **not** exposed as MCP tools (no remote mutation through the agent).

### Consistency rule

`get_repo_info` in workspace mode only lists repos in the active workspace. If an agent asks about a repo id that exists in `repos.yaml` but isn't in the active workspace, it gets `NotFound: repo <id>` — same error as if the repo didn't exist. This is intentional: from the agent's perspective, the workspace IS the universe.

---

## Error handling

| Scenario | Behavior | User-facing message |
|---|---|---|
| `--workspace foo` but `foo` not in `workspaces.yaml` | Fail fast at server startup | `Config: workspace 'foo' not found in workspaces.yaml` |
| `workspaces.yaml` references a repo id not in `repos.yaml` | Fail fast at server startup, list missing ids | `Config: workspace 'backend-team' references repos not in repos.yaml: [auth-svc-old]. Either add to repos.yaml or remove from workspace definition.` |
| `~/.config/lain/active_workspace` references a workspace not in `workspaces.yaml` | Fail at server startup when `--workspace auto` resolves it | `NoActiveWorkspace: 'foo' not found in workspaces.yaml at <path>; run \`lain workspaces use <name>\` or pass --workspace explicitly` |
| `workspaces.yaml` is malformed YAML | Fail at server startup with parse error | Standard `serde_yaml` error message + path |
| A workspace contains < 2 repos | Fail at config validation | `Config: workspace 'foo' must contain >= 2 repos; got 1` |
| A repo id contains `:` or `/` (fails `RepoId::new` validation) | Fail at config validation | `Config: workspace 'foo' contains invalid repo id 'auth/svc'; ids cannot contain ':' or '/'` |
| Server holds active workspace A; operator changes `workspaces.yaml` to reference workspace B | Server keeps A until restart | No signal — operator's responsibility (we're not adding hot-reload) |
| `lain workspaces use <name>` writes to `~/.config/lain/active_workspace`; subsequent `lain --workspace auto` picks it up | Normal flow | n/a |
| MCP `get_active_workspace` when server started without `--workspace` | Return JSON with `error: "no_active_workspace"` and `message` explaining how to set one | Same shape as today's `AmbiguousSymbol` errors |
| MCP `get_workspace` with unknown name | Standard not-found | `NotFound: workspace <name>` |

**Two design rules** driving these:

1. **Fail fast at startup, never mid-flight.** All workspace membership / config errors are validated before any indexing starts. If the operator's config is broken, they find out in 200 ms, not 5 minutes into a cold start.
2. **Ambiguity surfaces cleanly.** If a repo id referenced from a workspace isn't in `repos.yaml`, the error lists the missing ids — never a vague "workspace load failed" — so the operator knows exactly what to fix.

---

## UI / dashboard changes

**File:** modify `src/mcp/federation_dashboard.html`. Keep light theme (the only federation page; dark `front_end_monitor.html` is for single-workspace mode and stays unchanged).

### Three new sections appended to the existing page

**1. Workspaces panel** — when a workspace is active
```
<h2>Active workspace</h2>
<div id="workspace-banner" class="banner">
  <span class="badge active">backend-team</span>
  <span class="muted">4 repos · described as "Core backend services"</span>
</div>
<table id="workspace-members">
  <thead><tr><th>Repo id</th><th>Path</th><th>Health</th></tr></thead>
  <tbody></tbody>
</table>
```

**2. Config panel** — what's wired up
```
<h2>Config</h2>
<table id="config">
  <tr><td>repos.yaml</td><td><code>{path}</code></td></tr>
  <tr><td>workspaces.yaml</td><td><code>{path}</code> or "not loaded"</td></tr>
  <tr><td>active workspace</td><td><code>{name}</code> or "none"</td></tr>
  <tr><td>workspace repos shown</td><td>{N} of {total_repos} in repos.yaml</td></tr>
</table>
```

**3. Per-workspace graph view** — the headline UI addition
```
<h2>Workspace graph</h2>
<div class="controls">
  <label>Layout: <select id="graph-layout">…</select></label>
  <label>Filter: <input id="graph-filter" placeholder="substring match"></label>
  <span class="muted">{N} nodes · {M} edges</span>
</div>
<svg id="workspace-graph" width="100%" height="500"></svg>
<div class="legend">
  <span class="dot repo-a"></span> repo-a
  <span class="dot repo-b"></span> repo-b
  <span class="line calls"></span> Calls (intra-repo)
  <span class="line calls-cross"></span> Calls (cross-repo, dashed)
  <span class="line imports"></span> Imports
</div>
```

- **Layout:** D3 force-directed (matches `src/ui/blast-radius.html`)
- **Filter:** substring match against node `name` + `path`
- **Color nodes by `repo_id`** (consistent palette across the page)
- **Edges styled by edge type:** solid `Calls` (intra-repo), dashed `Calls` (cross-repo, made possible by the test-gap fix's Pass B), thin gray `Imports`
- **Click a node** → side panel showing `explain_symbol`-style context
- **Hover** → tooltip with `name` + `repo_id` + `path`

### `get_workspace_graph` data flow

Server-side:
- Scope graph query to workspace's repos
- Project per-repo nodes (filtered to `Function` / `Method` / `Class` for clarity)
- Project `Calls` and `Imports` edges
- Mark edges `cross_repo: true` if source's `repo_id` ≠ target's `repo_id`
- Cap at ~5000 nodes / ~10000 edges; truncate with a banner if exceeded

### Scope: what's NOT in the graph

Per the locked decision, the graph filters to **Functions / Methods / Classes + Calls / Imports / cross-repo Calls**. Excluded:
- `File` / `Module` / `Package` (too many, clutters the view)
- `Contains` / `Defines` (structural, redundant with `Imports`)
- `CO_CHANGED_WITH` (historical, separate concept)
- `CrossRepoSameSymbol` (signature-similarity edges, would be misleading to show as "calls")

These exclusions keep the per-workspace graph readable at hundreds-to-low-thousands of nodes.

---

## Data flow

### Per-PR test fixture (`tests/workspace_e2e.rs`)

```
1. tempdir + write 5-repo repos.yaml
2. write workspaces.yaml referencing 3 of them
3. load_federation_with_workspace(cfg, workspace_name)
4. assert list_repos() == 3 (not 5)
5. resolve_symbol / get_active_workspace / get_workspace → assert
```

### Dashboard fetch

```
1. GET /health (existing)
2. call_tool('get_active_workspace', {})
3. call_tool('get_workspace', {name})
4. call_tool('get_workspace_graph', {filter?})
5. render via D3
```

---

## Test plan

### Per-PR (workspace equivalent of D fixture)

New file `tests/workspace_e2e.rs` with 6 tests:

1. `workspace_config_loads_and_validates_members` — load `workspaces.yaml` with a 3-repo workspace; assert `Vec<RepoId>` matches.
2. `workspace_rejects_unknown_repo_id` — workspace references a repo not in `repos.yaml`; assert config-validation error names the missing id.
3. `workspace_rejects_sub_two_repos` — workspace with 1 repo; assert `Config` error.
4. `workspace_filters_repos_to_members` — build a 5-repo `repos.yaml` + a workspace with 3 of them; load via `load_federation_with_workspace`; assert `list_repos()` returns 3, not 5.
5. `workspace_mcp_get_active_workspace_returns_correct_subset` — MCP `get_active_workspace` returns the active workspace's `members` field with the right repo ids and resolved paths.
6. `workspace_mcp_get_workspace_graph_filters_correctly` — MCP `get_workspace_graph` returns nodes whose `repo_id` is always a workspace member, never an excluded repo.

### End-to-end (nightly)

New shell script `tests/e2e/workspace_e2e.sh`:

- Generates a `repos.yaml` with the 3 famous repos (rayon/ripgrep/serde) + a 3-repo workspace `backend-team` referencing 2 of them + the OTel demo subdirs as a separate workspace
- Starts `lain server --workspace backend-team --transport http --port 9999`
- Calls `get_active_workspace`, `get_workspace`, `get_workspace_graph` (no filter), `get_workspace_graph` with a filter
- Asserts node count > 0, all nodes are within workspace members, the per-workspace graph is responsive (renders under 2s for a small workspace)
- Calls `get_cross_repo_blast_radius` for a known symbol and asserts the result buckets within the workspace's repos
- Repeats with `--workspace otel-demo` to verify workspace switching

### Dependency

The workspace's end-to-end nightly test **depends on the test-gap fix landing first**. Without cross-repo `Calls` edges, the per-workspace graph shows only intra-repo edges. The PR 2 test fixtures should be merged AFTER the test-gap fix's PR 1 (Pass A + Pass B) is merged.

---

## Backward compatibility

- **`--workspace` is a new flag.** Default behavior (no flag) is unchanged. Existing federation users see no difference.
- **`repos.yaml` schema unchanged.** The A fixture in `tests/e2e/federation_e2e.sh` (OTel extension from the test-gap spec) is the same `repos.yaml` the workspace feature consumes — workspaces reference repo ids from it.
- **`workspaces.yaml` is a new optional file.** Operators who don't define workspaces don't need to create it. The server falls back to "all repos in `repos.yaml`" mode when no workspace is specified.
- **MCP tool additions are additive.** Existing tools unchanged; three new tools (`list_workspaces`, `get_active_workspace`, `get_workspace`) added when workspaces are configured. `get_workspace_graph` is also new but only registered when a workspace is active.
- **Federation dashboard changes are additive.** New sections render empty / "no active workspace" when no workspace is configured; existing repos table unchanged.
- **Per-repo tool mode unchanged.** Single-workspace mode (`lain --workspace <path>`) doesn't get a workspace concept — that flag still means "one repo for solo work."

---

## Migration / rollout

Two PRs, sequenced:

**PR 1 — Core workspace layer:**
- New `src/federation/workspace.rs` (or similar) for `WorkspaceSpec`, `WorkspaceSource`, `WorkspaceIndex`
- New CLI subcommand `lain workspaces ...` (`src/cmds/workspaces.rs`)
- New MCP tools `list_workspaces`, `get_active_workspace`, `get_workspace`
- `--workspace` flag wiring on `lain server`
- Tests in `tests/workspace_e2e.rs`

**PR 2 — Dashboard + nightly e2e:**
- `src/mcp/federation_dashboard.html` extensions (three new sections)
- New MCP tool `get_workspace_graph`
- `tests/e2e/workspace_e2e.sh`
- Docs (`docs/FEDERATION.md` Workspace section, README pointer)

---

## Definition of done

1. The test-gap fix's PR 1 (Pass A + Pass B) has merged into `main`. Workspace PR 1 cannot land before this.
2. `src/federation/workspace.rs` (or similar) defines `WorkspaceSpec`, `WorkspaceSource`, `WorkspaceIndex` with the spec'd semantics.
3. `lain workspaces create / add / remove / import / init / list / show / use / current / forget` all work, each validated end-to-end against a real `workspaces.yaml`.
4. `lain server --workspace <name>` filters `repos.yaml` to the workspace's members; the 6 federation MCP tools return results scoped to that subset.
5. `--workspace auto` resolves via `~/.config/lain/active_workspace`.
6. All 6 tests in `tests/workspace_e2e.rs` pass on a clean clone.
7. The federation_dashboard.html renders the three new sections; `get_workspace_graph` returns scoped data; `tests/e2e/workspace_e2e.sh` passes in the nightly workflow.
8. `docs/FEDERATION.md` has a new "Workspaces" section; README has a pointer; existing docs unchanged.
9. No regression: existing `tests/federation_integration.rs`, `tests/federation_cross_repo_e2e.rs`, `tests/federation_benchmark.rs`, and `tests/e2e/federation_e2e.sh` all still pass.

## Open questions (for the implementation plan, not blockers)

- The `get_workspace_graph` MCP tool — single dedicated tool returning the graph data in one round-trip, or client-side stitching of multiple existing tools? Single tool is cleaner for latency; multi-tool reuses existing surfaces. (Carry from Section 5.)
- The 5000-node / 10000-edge cap — too generous? Too tight? Depends on workspace size.

---

## Status

Brainstorming complete (sections 1–6 approved). Spec written. Awaiting user review before invoking writing-plans.