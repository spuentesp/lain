# Federation Mode

Federation mode is the multi-repo mode of `lain`. Run it with:

```bash
lain server --config ./repos.yaml --transport http --port 9999
# Open http://localhost:9999 — the Command Center dashboard.
```

A federation knows about N repositories at once and answers org-wide
questions across them: who defines a given function, who calls it,
what other services depend on it, and which repos are degraded right
now. Federation is the headline of `lain`: a single long-running
process that owns the index, the workspaces, and the MCP tool surface.

This document is the central reference for federation mode. The schema
of the `repos.yaml` config lives in [`docs/REPOS_YAML.md`](REPOS_YAML.md);
this doc covers the operating model, the MCP tools, the resolver rules,
performance, and troubleshooting. The on-disk state format and the
petgraph backend live in [`docs/TECHNICAL.md`](TECHNICAL.md). Hot-reload
of `repos.yaml` / `workspaces.yaml` is documented in
[`docs/hot-reload.md`](hot-reload.md); the Command Center dashboard is
in [`docs/command-center.md`](command-center.md).

---

## When to use federation vs single-workspace

| Mode | Use it when |
|---|---|
| Single-workspace (`lain server --config ./repos.yaml --workspace <name>` with a one-member workspace) | You want to scope answers to one repo's subgraph. |
| Federation (`lain server --config repos.yaml`) | You have many repos and want to ask org-wide questions: "which repos define a function named `verify_token`?", "what does repo A's `shared` library affect in repo B and C?", "is every repo healthy right now?". |

The federation tools are read-only and do not let you mutate the
underlying graphs.

---

## Setup

Federation mode is configured by a `repos.yaml` file and started by
`lain server`. The schema is documented in
[`docs/REPOS_YAML.md`](REPOS_YAML.md); this section is the operational
quickstart.

### 1. Write a `repos.yaml`

At minimum, list the repos you want indexed:

```yaml
data_dir: /var/lib/lain
repos:
  - id: auth-svc
    source: { type: workspace_dir, path: /srv/auth-svc }
  - id: billing-svc
    source: { type: local_clone, url: https://example.com/billing.git }
  - id: web
    source: { type: shallow_clone, url: https://example.com/web.git }
```

The two relevant bounding knobs (defaults shown):

| Field | Default | Purpose |
|---|---|---|
| `max_concurrent_indexers` | `8` | Semaphore that caps how many repos index in parallel at startup. |
| `ready_threshold` | `0.8` | Fraction of repos that must reach `Ready` health before the federation reports healthy. |

Each repo can be a `workspace_dir` (path on disk), `local_clone` (full
clone into `data_dir/<id>`), or `shallow_clone` (shallow clone that
refreshes on an interval). See `docs/REPOS_YAML.md` for the full
schema and `id` rules.

### 2. Start the server

```bash
lain server --config /etc/lain/repos.yaml --transport http --port 9999
```

Flags (all have defaults):

- `--config <path>` — required; path to `repos.yaml`
- `--transport <http|stdio>` — default `http`
- `--port <u16>` — default `9999`, only meaningful for HTTP
- `--log_level <env-filter>` — default `info`; passed to `tracing`'s `EnvFilter`
- `--workspace <name>` — optional; pin to a workspace declared in
  `workspaces.yaml`. Use `auto` to read the operator's
  `~/.config/lain/active_workspace` pointer.

The loader reads the config, builds per-repo sources, and indexes each
repo up to `max_concurrent_indexers` at a time. After loading, the
server explicitly runs `repo.index()` on every registered repo so that
freshly loaded federations have actual nodes (the load-time projection
only sees whatever is already in the per-repo DB). A failed `index()`
demotes that repo to `Degraded` and the federation still comes up so
partial results remain queryable.

While the server runs, the Command Center dashboard is at `GET /` on
the HTTP port. The status bar shows pid, transport, repo/workspace
counts, and the reload state in real time. See
[`docs/command-center.md`](command-center.md).

### 3. Wait for it to be ready

The federation is considered ready when `ready_threshold` (default 80%)
of the registered repos have `health: "ready"`. Until then, individual
repos will report `indexing`, `degraded`, `unavailable`, or `missing`.
Watch the logs, the Command Center Overview tab, or poll
`get_federation_health` over MCP.

---

## Tools

Federation mode registers six read-only MCP tools. All tool results are
returned as JSON text inside `CallToolResult::content[0].text`. Tool
failures set `is_error: true` and put the error message in the text
block.

The JSON shapes below are the exact serde-derived structs from
`src/mcp/federation_tools.rs`.

### `list_repos`

Returns every registered repo with its current health, path, last
refresh/index timestamps, and node/edge counts.

- **Arguments:** none
- **Returns:** array of `RepoInfo`:

```json
[
  {
    "id": "auth-svc",
    "path": "/srv/auth-svc",
    "health": "ready",
    "last_refreshed_unix": 1730000000,
    "last_indexed_unix": 1730000000,
    "node_count": 1234,
    "edge_count": 987
  }
]
```

Field reference (struct: `RepoInfo`):

| Field | Type | Notes |
|---|---|---|
| `id` | string | The repo id from `repos.yaml` |
| `path` | string | Local on-disk path of the repo (the clone for `local_clone` / `shallow_clone`, the `path` for `workspace_dir`) |
| `health` | string | One of `ready`, `indexing`, `degraded`, `unavailable`, `missing` |
| `last_refreshed_unix` | i64 | Seconds since UNIX epoch when the source was last refreshed (e.g. shallow clone fetch) |
| `last_indexed_unix` | i64 | Seconds since UNIX epoch when `RepoIndex::index()` last completed |
| `node_count` | usize | Nodes in this repo's per-repo `GraphDatabase` |
| `edge_count` | usize | Edges in this repo's per-repo `GraphDatabase` |

**Errors:** none — always returns the current list, even when empty.

### `get_repo_info`

Returns the same `RepoInfo` payload as `list_repos`, but for a single
repo by id.

- **Arguments:**
  - `id` (string, required) — the repo id
- **Returns:** a single `RepoInfo` object (same shape as above)
- **Errors:**
  - `Missing required argument: id` — no `id` key in args
  - `NotFound: repo <id>` — no repo with that id is registered

### `get_federation_health`

Aggregate counts per health bucket, plus total node/edge counts and a
rough memory estimate.

- **Arguments:** none
- **Returns:** `FederationHealth`:

```json
{
  "total_repos": 12,
  "ready": 10,
  "indexing": 1,
  "degraded": 0,
  "unavailable": 1,
  "missing": 0,
  "total_nodes": 123456,
  "total_edges": 78901,
  "memory_estimate_bytes": 32456700
}
```

The `memory_estimate_bytes` field is computed by the implementation as:

```
memory_estimate_bytes = total_nodes * 200 + total_edges * 100
```

It's a rough upper-bound heuristic, not a measured RSS.

**Errors:** none.

### `search_org`

Case-insensitive substring search across every repo's symbols, matched
on `name` or `path`. Results are deduplicated by `global_id`, sorted by
`(repo_id, name)`, and truncated to the caller-supplied limit.

- **Arguments:**
  - `query` (string, required) — substring to match
  - `limit` (integer, required) — max number of results to return
- **Returns:** array of `SymbolMatch`:

```json
[
  {
    "global_id": "auth-svc:Function:src/auth.rs:verify_token",
    "repo_id": "auth-svc",
    "name": "verify_token",
    "path": "src/auth.rs",
    "kind": "function"
  }
]
```

**Errors:**
- `Missing required argument: query`
- `Missing required argument: limit`
- `Invalid argument: limit must be a non-negative integer` — `limit` is the wrong type or unparseable

### `get_cross_repo_blast_radius`

The headline federation tool. Resolves a symbol across the federation,
then traverses outgoing `Calls` edges in the federated graph and groups
the visited nodes by repo.

- **Arguments:**
  - `symbol` (string, required) — the function/symbol name
  - `depth` (string, required) — a half-open `Range<u32>` like `"1..3"` (start inclusive, end exclusive)
- **Returns:** `CrossRepoBlastRadius`:

```json
{
  "by_repo": {
    "auth-svc": [
      "auth-svc:Function:src/auth.rs:caller_a",
      "auth-svc:Function:src/auth.rs:caller_b"
    ],
    "billing-svc": [
      "billing-svc:Function:src/checkout.rs:caller_c"
    ]
  },
  "total_count": 3,
  "truncated": false
}
```

Implementation notes:
- The traversal is **outgoing-only** along `Calls` edges from the seed.
- Results are bucketed by the `repo_id` parsed out of each visited node's
  global id (`repo_id:Kind:path:name`).
- A hard cap of **1000 nodes** per call is enforced. When hit,
  `truncated` is `true` and `total_count` equals the cap. More nodes
  exist beyond it; the caller should narrow `depth` or pick a different
  seed.
- The seed node itself is **excluded** by the depth lower bound
  (`min_depth=1`), so a symbol with no outgoing `Calls` edges yields an
  empty result, not an error.

**Errors:**
- `Missing required argument: symbol`
- `Missing required argument: depth`
- `Invalid depth: expected "<start>..<end>", got "<input>"` — depth string is malformed; the trailing `got "<input>"` echoes the offending value (debug-quoted) so you can see what the parser saw
- `NotFound: symbol <name> not found in any repo` — `resolve_symbol` found nothing
- `AmbiguousSymbol: [...]` — `resolve_symbol` found the symbol in multiple repos; the caller should disambiguate via `repo_id` (use `get_cross_repo_blast_radius_for_repo` or pass a disambiguator)
- `NotFound: symbol <name> not found in repo <id>` — only possible via the `_for_repo` variant

### `get_cross_repo_blast_radius_for_repo`

Same shape as `get_cross_repo_blast_radius`, but the caller disambiguates
the repo explicitly, bypassing `resolve_symbol`. Use this when the symbol
exists in multiple repos and the agent already knows which repo owns
the seed.

- **Arguments:**
  - `repo_id` (string, required) — the repo id that owns the seed symbol
  - `symbol` (string, required) — the function/symbol name
  - `depth` (string, required) — `Range<u32>` like `"1..3"`
- **Returns:** `CrossRepoBlastRadius` (same shape as above)
- **Errors:**
  - `Missing required argument: repo_id | symbol | depth`
  - `Invalid depth: expected "<start>..<end>", got "<input>"`
  - `NotFound: symbol <name> not found in repo <id>`
  - `Invalid repo id: <id>` — bad repo id (raised by `RepoId::new`'s validation: empty, contains `:`, or contains `/`)

---

## Tool resolution rules

Federation tools that take a `repo_id` (currently `get_repo_info`,
`get_cross_repo_blast_radius_for_repo`, and the per-repo tools in
single-workspace mode that are resolved against a federation) use
`resolve_repo_for_tool` in `src/mcp/handler.rs`. The rule, in order:

1. **Explicit `repo_id`.** If the call's `args.repo_id` is present and
   non-empty, use it as-is. If it's malformed (fails `RepoId::new`'s
   validation), return `Invalid repo id: <id>`.
2. **Symbol hint.** Otherwise, if `args.symbol` is present, call
   `FederatedIndex::resolve_symbol(symbol)`:
   - exactly one repo owns that name → return that repo
   - zero repos own it → return `NotFound: symbol <name> not found in any repo`
   - multiple repos own it → return `AmbiguousSymbol(candidates)` so the
     caller can present candidates to the user
3. **Zero repos.** If neither arg is present and zero repos are
   registered, return `Config: no repos registered`.
4. **Single-repo fallback.** If neither arg is present and exactly one
   repo is registered, use that repo.
5. **Multi-repo dead end.** If neither arg is present and multiple repos
   are registered, return
   `Config: multiple repos; specify repo_id or symbol`.

Examples (in `args`):

| `args` | Resolution |
|---|---|
| `{ "repo_id": "auth-svc" }` | Step 1 → `auth-svc` |
| `{ "symbol": "verify_token" }`, unique owner | Step 2 → owning repo |
| `{ "symbol": "verify_token" }`, multiple owners | Step 2 → `AmbiguousSymbol` |
| `{ "symbol": "nope" }`, no owners | Step 2 → `NotFound` |
| `{}`, zero repos | Step 3 → `Config: no repos registered` |
| `{}`, one repo | Step 4 → that repo |
| `{}`, multiple repos | Step 5 → `Config: multiple repos; specify repo_id or symbol` |

When the resolver returns `AmbiguousSymbol`, the MCP layer surfaces a
JSON payload of shape `{"error":"ambiguous_symbol","candidates":[...],
"message":"..."}` inside the tool result text so the agent can present
the candidates and pick one for the next call.

---

## Performance

The federation has two measured workloads; both are in
`tests/federation_benchmark.rs`.

### Small fixture — `p99 < 100ms`

The small fixture is **10 repos × 5,000 nodes per repo = 50,000 total
nodes**, with each repo holding a chain of `Calls` edges. The test runs
100 `get_cross_repo_blast_radius` calls (depth `1..5`, walking a 5-hop
chain) and asserts the 99th-percentile latency is under 100 ms.

It runs on every PR:

```bash
cargo test --features test-utils --test federation_benchmark \
    small_fixture_blast_radius_under_100ms_p99 -- --nocapture
```

### Large fixture — 200 repos / 10M nodes

The large fixture is **200 repos × 50,000 nodes per repo = 10,000,000
total nodes**. It also asserts `memory_estimate_bytes < 32 GB`. It is
marked `#[ignore]` (nightly-gated) and is not part of the per-PR suite:

```bash
cargo test --features test-utils --test federation_benchmark \
    large_fixture -- --ignored --nocapture
```

### Memory estimate formula

`get_federation_health.memory_estimate_bytes` is computed by the
implementation as:

```
memory_estimate_bytes = total_nodes * 200 + total_edges * 100
```

This is a rough upper bound — actual RSS will depend on the
`data_dir` layout and OS page cache. Use it as a sanity check, not as
a precise measurement.

### Throughput caveats

- The per-PR latency budget (100 ms p99) is measured against the small
  fixture only. Larger federations will be slower; the cost scales with
  the size of the reachable subgraph at the requested depth.
- The blast-radius traversal is bounded by a hard cap of **1000 nodes**
  per call. Deep traversals over a dense subgraph will hit this cap and
  set `truncated: true`.
- Cross-repo symbol matching (`find_cross_repo_matches`, called from
  `project_repo`) is O(repos × nodes) per repo projection and runs once
  at load time. Cold start of a large federation is dominated by
  indexing; subsequent restarts pay only the re-projection cost.

---

## Troubleshooting

The errors below are the ones the implementation actually produces.
Each entry lists the literal error text, what it means, and what to do.

### `NotFound: symbol <name> not found in any repo`

The symbol isn't in the federation's `symbol_to_repos` index nor in the
backend's `find_nodes_by_name` fallback. Common causes:

- The repo that owns the symbol hasn't finished indexing yet. Check
  `list_repos` and confirm the owning repo's `health` is `ready`.
- The repo isn't registered in `repos.yaml`. Check `list_repos` —
  every registered repo should be present even when degraded.
- The symbol has a different name than expected (e.g. fully-qualified
  vs. short name). `resolve_symbol` matches on the short name only.

### `AmbiguousSymbol`

Multiple repos own a function with the same name. The tool result will
include a JSON payload of shape:

```json
{
  "error": "ambiguous_symbol",
  "candidates": ["auth-svc", "billing-svc"],
  "message": "Multiple repos match this symbol; specify repo_id or disambiguate."
}
```

**Action:** present the candidates to the user, then re-call with an
explicit `repo_id` (or use `get_cross_repo_blast_radius_for_repo`).

### `NotFound: repo <id>`

The caller passed a well-formed `repo_id` that isn't registered. Only
raised by `get_repo_info` — the cross-repo blast-radius tools don't look
up by registration; they filter by the `repo_id` prefix on the symbol
search. Check `list_repos` for the canonical id spelling.

### `Invalid repo id: <id>`

The caller passed a `repo_id` that fails `RepoId::new`'s validation
(empty string, contains `:`, or contains `/`). Raised by any tool that
validates the `repo_id` directly: `get_repo_info`,
`get_cross_repo_blast_radius_for_repo`, and the per-repo tool resolver
(`resolve_repo_for_tool`). Fix the id to match `RepoId`'s rules and
retry.

### `NotFound: symbol <name> not found in repo <id>`

Only raised by `get_cross_repo_blast_radius_for_repo`. The repo is
registered but doesn't own a symbol with that name. Check
`get_repo_info(<id>)` to confirm the repo is `ready`, then
`search_org` to find what the repo does own.

### `Config: no repos registered`

The federation has zero repos but a tool that needs repo resolution was
called. The config file was either empty or failed to load. Check the
server logs for the load error.

### `Config: multiple repos; specify repo_id or symbol`

A tool that needs repo resolution was called without `repo_id` or
`symbol` and the federation has more than one repo. The agent must
either pass one of those args or present the user with a list.

### `Config: yaml: <serde_yaml error>`

The `repos.yaml` failed to parse. Common causes: bad indentation,
unknown `source.type`, missing `id` or `source`. See
[`docs/REPOS_YAML.md`](REPOS_YAML.md) for the schema.

### `Missing required argument: <name>`

The tool's args object didn't include a required key. The tool's name
appears in the literal error text. Re-call with the missing key.

### `Invalid argument: limit must be a non-negative integer`

`search_org` was called with a `limit` that's not a non-negative
integer (or a string that fails to parse as one). Pass `limit` as a
JSON number (or a string of digits).

### `Invalid depth: expected "<start>..<end>", got "<input>"`

`get_cross_repo_blast_radius*` was called with a `depth` that isn't a
`Range<u32>` literal of the form `"<start>..<end>"`. The trailing
`got "<input>"` echoes the offending value (debug-quoted) so you can
see what the parser saw. The end is **exclusive**. For example,
`depth: "1..3"` traverses depth 1 and 2 only.

### Repo stuck in `indexing` / `degraded` / `unavailable` / `missing`

The federation still serves partial results when some repos are
unhealthy — `get_federation_health` reports counts per bucket, and
`list_repos` reports per-repo health. Watch the server logs:

- `indexing` — initial state; should transition to `ready` once
  `RepoIndex::index()` completes. If it lingers, check the per-repo
  data dir and the `data_dir` write permissions.
- `degraded` — `RepoIndex::index()` returned an error. The server logs
  the underlying error. Common causes: language-server binary not on
  `PATH` (LSP hydration is best-effort), permission errors, disk full.
- `unavailable` — the source's `fetch()` failed (e.g. shallow clone
  couldn't reach the remote). The repo is registered but unindexable
  on this run.
- `missing` — the repo's expected data directory is gone. Check that
  `data_dir/<id>` still exists and is readable.

### `lain server` exits immediately

- `Config: yaml: ...` — bad config. Validate the YAML with
  `cargo test --lib config` (the unit tests in `src/federation/config.rs`
  cover the schema).
- `Io: read config: ...` — config file not found at `--config`. Check
  the path.
- `unknown transport: <x> (expected 'http' or 'stdio')` — `--transport`
  must be `http` or `stdio`.

### `truncated: true` on a blast-radius result

The traversal hit the 1000-node cap. Re-call with a smaller `depth`,
a different seed, or use `search_org` first to confirm the symbol
exists in exactly one repo and isn't a high-fanout hub.

---

## Workspaces

Workspaces are named groups of repos that the federation engine indexes
together as a coherent unit. A workspace = a subset of `repos.yaml`'s
repos, picked at server start via `--workspace <name>` (or `auto` to
read the operator's `~/.config/lain/active_workspace` pointer). The
workspace config lives in `workspaces.yaml` next to `repos.yaml`.

### When to use workspaces

| Mode | Use it when |
|---|---|
| Federation (`lain server --config repos.yaml`) | Org-wide questions across all repos |
| Workspace (`lain server --config repos.yaml --workspace <name>`) | Questions scoped to a named subset ("backend-team", "payments-ws") |

### Setup

1. Declare workspaces in `workspaces.yaml` (same directory as `repos.yaml`):
   ```yaml
   workspaces:
     - name: backend-team
       members: [auth-svc, billing-svc, db-client]
   ```
2. Pick one: `lain workspaces use backend-team` (writes
   `~/.config/lain/active_workspace`).
3. Start the server: `lain server --config repos.yaml --workspace auto
   --transport http --port 9999`. The federation loads only
   `backend-team`'s members. All 6 federation tools operate scoped.
4. Workspace switching is restart-only — `workspaces.yaml` edits are
   hot-reloaded (see [`docs/hot-reload.md`](hot-reload.md)) but the
   *active* workspace is read once at startup.

### Workspace CLI

```
lain workspaces create / list / show / use / current / forget
```

See `lain workspaces --help`.

### MCP tools (workspace mode)

In addition to the 6 federation tools (scoped to the workspace's repos):
- `list_workspaces` — all known workspaces + which is active
- `get_active_workspace` — the active workspace's name + members
- `get_workspace(name)` — full detail on one workspace

### Command Center

When `lain server --transport http` is running, the Command Center at
`GET /` shows:
- Active workspace panel (name + members + their paths/healths)
- Config panel (paths + repo counts)
- Per-workspace D3 force-directed graph view (Functions/Methods/Classes
  + Calls/Imports, color by `repo_id`, dashed lines for cross-repo
  Calls)

See [`docs/command-center.md`](command-center.md).

### Agents

`get_agent_strategy` (a built-in MCP tool) includes a "Workspace mode"
section that documents the workspace tools + the `repo_id` resolution
rule when scoped.

## Migration

Federation mode is additive on top of single-workspace mode.

### Single-workspace users

Run the same directory under a one-member `repos.yaml` and start the
server with `lain server --config ./repos.yaml --transport stdio`.
Federation-only tools (`list_repos`, `get_federation_health`, etc.)
still work; per-repo tools (`query`, `coupling`, `blast_radius`, etc.)
work via the workspace's single repo.

### Federation users coming from single-workspace

Switching modes is opt-in. To turn a single-repo deployment into a
federation, write a `repos.yaml` (see [`docs/REPOS_YAML.md`](REPOS_YAML.md))
and start the server with `lain server --config repos.yaml`. The
existing single-workspace CLI does not need to change.

### `/health` endpoint

The HTTP `/health` endpoint is shared between modes. Federation mode
attaches a `federation` field describing per-repo health, total
counts, and the memory estimate; single-workspace mode reports
`federation: null`. The rest of the `/health` payload is unchanged.

---

## Smoke test

The block below starts the federation server in the background, waits
for it to come up, and exercises two federation tools via the MCP HTTP
JSON-RPC endpoint.

```bash
# Start the federation server:
lain server --config repos.yaml --transport http --port 9999 &

# Wait for it to be ready:
sleep 30

# List repos:
curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_repos","arguments":{}},"id":1}' \
  | jq '.result.content[0].text | fromjson'

# Search org-wide:
curl -s -X POST http://localhost:9999/mcp -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"search_org","arguments":{"query":"verify","limit":5}},"id":1}' \
  | jq '.result.content[0].text | fromjson'
```

The `sleep 30` is a rough budget for the federation to reach
`ready_threshold` of registered repos. Replace it with a poll on
`get_federation_health` when automating. Federation tool results are
JSON text wrapped inside `CallToolResult::content[0].text`; the `jq`
filter unwraps the inner JSON so the output is directly readable.

The full bash block above is shell-syntax-validated with:

```bash
awk '/^```bash$/,/^```$/' docs/FEDERATION.md | bash -n
```
