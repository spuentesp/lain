# LAIN-mcp - Agent Quickstart

If you are an AI agent (Claude, Gemini, etc.) connecting to this MCP server, follow this strategy to understand the codebase efficiently.

## 1. Initialize & Verify
Start by checking the server's health and knowledge freshness.
- Call `get_health`: See which language servers are ready and repository info.
- If a language is missing, call `install_language_server(language: "ext")`.

## 2. Global Orientation (The Telescope)
Don't read files yet. Get the macro-view.
- Call `find_anchors(limit: 5)`: Identify the most foundational building blocks.
- Call `list_entry_points`: Find where the application logic begins.
- Call `explore_architecture(max_depth: 2)`: Get a topological summary.
- Call `describe_schema`: Understand the graph schema.

## 3. Targeted Exploration
Once you have a target subsystem:
- Call `get_layered_map(layer: 1, granularity: "file")`: See files inside modules.
- Use `query_graph` for prebuilt or custom queries.

## 4. Deep Reasoning
When you need to perform a task:

**For query language:** See `docs/quickstart-query.md`
- Prebuilt queries: `get_blast_radius`, `get_call_chain`, etc.
- Custom ops: `find`, `connect`, `filter`, `semantic_filter`, `group`, `sort`, `limit`

**For individual tools:** See `docs/quickstart-tools.md`
- `semantic_search` — Find code by meaning
- `get_blast_radius` — See ripple effects
- `get_call_chain` — Shortest functional path
- `find_dead_code` — Potentially unreachable code
- `explain_symbol` — Symbol summary with metrics
- And 30+ other tools

## 5. Repo Identity
To identify which repository you're working in:
- Call `get_health` — includes `RepoIdentity` parsed from git remote

## 6. Syncing State
If you make changes to the code or switch git branches:
- Call `sync_state`: Refresh the graph using Git deltas.

---

## Federation mode

Use federation mode when a question spans repositories: locating symbol definitions, checking repo health, or following outgoing `Calls` edges across repo boundaries. For work confined to one repo, keep using `lain --workspace ./myrepo`; that path is unchanged.

### When to use federation

| Task | Single-workspace | Federation |
|---|:---:|:---:|
| Bug fix within one repo | ✓ | ✗ |
| Find which repos define a symbol | ✗ | ✓ |
| Follow outgoing `Calls` across repos | ✗ | ✓ |
| Inspect health and graph stats for many repos | ✗ | ✓ |

### Setup

See [`docs/FEDERATION.md`](./FEDERATION.md) for the full guide and [`docs/REPOS_YAML.md`](./REPOS_YAML.md) for the config schema. In summary:

1. The operator writes `repos.yaml` with the repositories to load.
2. The operator runs `lain server --config repos.yaml --transport http --port 9999`.
3. The agent checks `list_repos` or `get_federation_health` before relying on a repo's results.

`max_concurrent_indexers` defaults to `8` and caps concurrent load tasks in `loader.rs`. `ready_threshold` defaults to `0.8`, but the current loader and server do not enforce it as a readiness gate; inspect the reported health states directly.

### Federation tools

The five workflows below cover all six registered federation tools; the sixth is the explicit-repo blast-radius variant. Calls show the MCP tool name and exact argument object. Results shown are the decoded JSON text returned in `CallToolResult::content[0].text`.

#### `list_repos`

Returns every registered repo's id, local path, health, timestamps, and graph counts.

```json
{"name":"list_repos","arguments":{}}
```
```json
[{"id":"auth-svc","path":"/srv/auth-svc","health":"ready","last_refreshed_unix":1730000000,"last_indexed_unix":1730000000,"node_count":1234,"edge_count":987}]
```

#### `get_repo_info`

Returns the same `RepoInfo` shape for one id. Missing `id` produces `Missing required argument: id`; an invalid id produces `Invalid repo id: <id>`; an unknown valid id produces `Not found: repo <id>`.

```json
{"name":"get_repo_info","arguments":{"id":"auth-svc"}}
```
```json
{"id":"auth-svc","path":"/srv/auth-svc","health":"ready","last_refreshed_unix":1730000000,"last_indexed_unix":1730000000,"node_count":1234,"edge_count":987}
```

#### `get_federation_health`

Returns repo counts by health state, total graph counts, and the implementation's memory estimate.

```json
{"name":"get_federation_health","arguments":{}}
```
```json
{"total_repos":2,"ready":1,"indexing":0,"degraded":1,"unavailable":0,"missing":0,"total_nodes":100,"total_edges":50,"memory_estimate_bytes":25000}
```

#### `search_org`

Runs a case-insensitive substring search on symbol `name` and `path`, sorted by `(repo_id, name)` and truncated to `limit`. The advertised input schema declares `limit` as a string, so this example uses `"5"` (the dispatcher also accepts a non-negative JSON integer).

```json
{"name":"search_org","arguments":{"query":"verify_token","limit":"5"}}
```
```json
[{"global_id":"auth-svc:Function:src/auth.rs:verify_token","repo_id":"auth-svc","name":"verify_token","path":"src/auth.rs","kind":"Function"}]
```

#### `get_cross_repo_blast_radius`

Resolves `symbol`, then traverses outgoing `Calls` edges over the half-open depth range `[start, end)`. The result excludes the seed, groups visited global ids by repo, and is capped at 1000 nodes (`truncated` reports whether the cap was hit).

```json
{"name":"get_cross_repo_blast_radius","arguments":{"symbol":"verify_token","depth":"1..3"}}
```
```json
{"by_repo":{"auth-svc":["auth-svc:Function:src/auth.rs:decode_claims"],"billing-svc":["billing-svc:Function:src/checkout.rs:authorize"]},"total_count":2,"truncated":false}
```

If the symbol has multiple owners, surface the candidates from an error such as `Ambiguous symbol: matches repos [RepoId("auth-svc"), RepoId("auth-utils")]` and call the explicit variant, which returns the same shape:

```json
{"name":"get_cross_repo_blast_radius_for_repo","arguments":{"repo_id":"auth-svc","symbol":"verify_token","depth":"1..3"}}
```

### Tool resolution rules

After the federation-specific dispatch arms, other tool calls in federation mode pass through `resolve_repo_for_tool`. It applies this five-step rule:

1. If `repo_id` is present, validate its syntax with `RepoId::new` and use it; this step does not check registration.
2. Otherwise, if `symbol` is present, resolve it to its unique owning repo. No match returns `Not found: symbol <name> not found in any repo`; multiple matches are ambiguous.
3. With no hints and zero registered repos, return `Config error: no repos registered`.
4. With no hints and exactly one registered repo, use that repo.
5. With no hints and multiple registered repos, return `Config error: multiple repos; specify repo_id or symbol`.

For ambiguity from this resolver, the handler returns parseable JSON text:

```json
{"error":"ambiguous_symbol","candidates":["auth-svc","auth-utils"],"message":"Multiple repos match this symbol; specify repo_id or disambiguate."}
```

### Cross-repo queries

When choosing a repo for a resolver-routed call:

- If the user supplies `repo_id`, pass it explicitly.
- If a `symbol` has one owner, let the resolver select that repo.
- If no hint is available and one repo is registered, use the fallback.
- If no hint is available and several repos are registered, ask the user which repo they mean.

When the resolver returns the ambiguity JSON above, present its candidates and retry with an explicit `repo_id`. For `get_cross_repo_blast_radius`, use the explicit `_for_repo` variant after its plain-text ambiguity error. For deeper setup, performance, and troubleshooting details, use [`docs/FEDERATION.md`](./FEDERATION.md).
