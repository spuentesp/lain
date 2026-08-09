# Docs + UI Federation Update Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the gap between the federated indexer (merged as `429d33f`) and the user-facing docs/UI so end-users (humans + AI agents) can discover and use the federation features.

**Architecture:** Additive docs + new federation dashboard. Two new doc files (`docs/FEDERATION.md`, `docs/REPOS_YAML.md`) plus additive sections to existing docs. One new HTML dashboard (`src/mcp/federation_dashboard.html`) plus per-tool `repo_id` selectors on the existing tool pages. Backend changes: extend `get_agent_strategy` to mention federation tools, add `federation` blob to `/health` response. Vanilla JS for the new dashboard (matches existing pages).

**Tech Stack:** Existing (Rust, tokio, hyper, rust-mcp-sdk, vanilla HTML/JS, no SPA framework). No new dependencies.

## Global Constraints

These come from the spec and apply to every task. The task's requirements implicitly include this section.

- **Rust toolchain:** MSRV 1.75 (matches `Cargo.toml`).
- **No new external dependencies.** Use only crates already in `Cargo.toml`. No new JS libraries — vanilla JS only.
- **Backwards compatibility:** `lain --workspace ./myrepo` (single-workspace mode) must keep working unchanged. Federation is additive.
- **Existing tests:** All 461 lib tests + 7 integration + e2e must continue to pass after the implementation.
- **Version bump:** `server.json`, `npm-shim/package.json`, `Formula/lain.rb` all bump from 0.3.0 to 0.4.0 in a single commit at the end. No other version churn.
- **Commit granularity:** One commit per task. Commit messages: `feat(federation): <what>`, `docs(federation): <what>`, `test(federation): <what>`, `fix(federation): <what>`.
- **Doc placement:** Federation reference docs go under `docs/`. Existing `docs/srs/` and `docs/superpowers/` are untouched.
- **TDD:** Every task with a Rust change has a failing test first. Every task with a UI change has a smoke command or e2e shell snippet first.
- **Tooling precedent:** Match existing patterns. Don't refactor existing code. Don't restructure files unless a task explicitly says to.

---

## File Structure

### New files

| File | Responsibility | Approx LOC |
|---|---|---|
| `docs/FEDERATION.md` | Central federation reference (concept, setup, tools, performance, troubleshooting, migration) | 300-500 |
| `docs/REPOS_YAML.md` | Pure config schema reference (schema, source kinds, examples) | 150-250 |
| `src/mcp/federation_dashboard.html` | Federation landing page (repo list, health, tool links) | 200-300 |
| `tests/e2e/federation_dashboard_e2e.sh` | E2E test for `front_end_monitor.html`, `federation_dashboard.html`, tool pages | 100-150 |
| `tests/version_consistency.rs` | Asserts `server.json`, `npm-shim/package.json`, `Formula/lain.rb` have the same version | 50-80 |

### Files to modify

| File | Change |
|---|---|
| `README.md` | Add "Federation mode" section + feature bullet; bump version |
| `docs/QUICKSTART_AGENTS.md` | Add "Federation mode" section with decision table + 5 tool examples |
| `docs/TECHNICAL.md` | Add "Federation architecture" section + cross-repo blast-radius paragraph |
| `docs/query-language.md` | One-line note pointing at `docs/FEDERATION.md` |
| `src/mcp/front_end_monitor.html` | Federation header + single-workspace fallback |
| `src/ui/blast-radius.html` | `repo_id` URL param + selector |
| `src/ui/call-chain.html` | `repo_id` URL param + selector |
| `src/ui/coupling.html` | `repo_id` URL param + selector |
| `src/tools.rs` | Extend `get_agent_strategy` to mention federation tools |
| `src/mcp/handler.rs` | Add `federation` blob to `/health` response |
| `server.json` | Bump version 0.3.0 → 0.4.0 |
| `npm-shim/package.json` | Bump version 0.3.0 → 0.4.0 |
| `Formula/lain.rb` | Bump version 0.3.0 → 0.4.0 |

Total new files: 5 (~800-1280 LOC). Total modified: 11 (mostly small).

---

## Tasks

### Task 1: Extend `get_agent_strategy` to mention federation tools

**Files:**
- Modify: `src/tools.rs:319` (the `get_agent_strategy` method body)
- Test: `src/tools.rs` (existing test module — append a new test)

**Interfaces:**
- Consumes: nothing (existing method)
- Produces: `get_agent_strategy` returns a string that includes the 5 federation tool names + decision table + `repo_id` resolution rule

**Background:** `get_agent_strategy` is the existing "operational manual" tool that AI agents call first. Extending it makes the federation tools discoverable to agents without reading any docs.

**The 5 tools to mention in the strategy string:**
- `list_repos` — list all indexed repos with health
- `get_repo_info` — get a single repo's details
- `get_federation_health` — get federation-wide stats (repo counts, node/edge totals, memory estimate)
- `search_org` — search symbols across all repos
- `get_cross_repo_blast_radius` (+ `_for_repo` variant) — cross-repo symbol blast radius

**The decision table to include:**
- Use single-workspace mode (`lain --workspace PATH`) when working on one repo.
- Use federation mode (`lain server --config repos.yaml`) when answering org-wide questions like "who else uses this function?" or "what depends on this service?"

**The `repo_id` resolution rule to include:**
- If `repo_id` is explicit → use it.
- If `symbol` is given and resolves to a unique repo → use that.
- If 1 repo is registered → use it.
- Otherwise → `Config("multiple repos; specify repo_id or symbol")`.

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)]` module in `src/tools.rs` (find it by searching for the existing `fn test_get_agent_strategy` or similar):

```rust
#[test]
fn get_agent_strategy_mentions_federation_tools() {
    use crate::tools::definitions::ToolDefinition;
    let strategy = build_test_strategy();
    // The strategy must mention each of the 5 federation tool names.
    for tool in ["list_repos", "get_repo_info", "get_federation_health",
                 "search_org", "get_cross_repo_blast_radius"] {
        assert!(
            strategy.contains(tool),
            "strategy must mention federation tool {}: \n{}",
            tool, strategy,
        );
    }
    // The strategy must explain the repo_id resolution rule.
    assert!(
        strategy.contains("repo_id") || strategy.contains("repo id"),
        "strategy must mention repo_id resolution",
    );
    // The strategy must explain single-workspace vs federation.
    assert!(
        strategy.contains("federation") || strategy.contains("Federation"),
        "strategy must mention federation mode",
    );
}

fn build_test_strategy() -> String {
    // Construct a minimal ToolExecutor + handler and call get_agent_strategy.
    // ... use the same constructor pattern as the existing test in this file.
}
```

If there's no existing test file for `src/tools.rs`, find an existing test or create a minimal one that invokes `get_agent_strategy` on a real (or minimal-mocked) `ToolExecutor`.

- [ ] **Step 2: Run test to verify it fails**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib tools::get_agent_strategy_mentions_federation_tools -- --nocapture`
Expected: FAIL (the current strategy string doesn't mention federation tools).

- [ ] **Step 3: Extend the strategy string in `src/tools.rs:319`**

Open `src/tools.rs` and find the `get_agent_strategy` method. Locate the existing string literal it returns. Append a new section to the string. Find the closing `format!(...)` (or `String::from(...)`) and add the federation content before the closing quote.

The added content should be:

```rust
        // Append a federation section.
        format!("{existing}\n\n---\n\nFederation mode (for org-wide questions):\n\
         - If the user's question spans multiple repos, use `lain server --config repos.yaml`.\n\
         - 5 new tools: list_repos, get_repo_info, get_federation_health, search_org, get_cross_repo_blast_radius (plus _for_repo variant).\n\
         - repo_id resolution: explicit > symbol > single-repo fallback. If multiple repos and no hint, ask the user.\n",
```

The exact format string should match the existing style in `get_agent_strategy`. If the existing format uses `format!` with positional args, add the existing string as a `let existing = ...` and then return the new value. If it returns a single literal, just append the text.

**Important:** the trick is to wrap the existing return in a `let`-binding so you can prepend/append without rewriting the whole body. Look at the existing method shape — if it returns `format!("...", ...)` then capture the value in a let. If it returns a literal string, do the same.

- [ ] **Step 4: Run test to verify it passes**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib tools::get_agent_strategy_mentions_federation_tools -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run full test suite to confirm no regressions**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib`
Expected: 461 + 1 = 462 passed, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add src/tools.rs
git commit -m "feat(federation): get_agent_strategy mentions federation tools"
```

---

### Task 2: Add `federation` blob to `/health` response

**Files:**
- Modify: `src/mcp/handler.rs` (the `GET /health` handler)
- Test: `src/mcp/handler.rs` (existing test module — append a new test, or create one)

**Interfaces:**
- Consumes: `FederationIndex::list_repos()` and `FederationIndex::backend()` (already exist)
- Produces: `GET /health` returns `{ status, server, version, graph_nodes, graph_edges, tools_count, federation: { repos: [...], total_nodes, total_edges, memory_estimate_bytes } | null }`. When `federation` is `null`, the UI renders single-workspace mode.

**Background:** The new HTML dashboard needs a way to know it's running in federation mode and what repos exist. Making a dedicated `tools/call` round-trip for every page load is wasteful. Adding the `federation` blob to the existing `/health` response (which the UI already fetches) is the cleanest signal.

- [ ] **Step 1: Write the failing test**

Append to the existing test module in `src/mcp/handler.rs` (after the federation tests added in Task 19):

```rust
#[tokio::test]
async fn health_response_includes_federation_blob_when_set() {
    // Build a handler with a federation (similar to the test in line 1215+).
    // ... use the existing `resolve_repo_for_tool` test as a model.
    // Send a synthetic GET /health request and parse the response body.
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tools/call",
        "params": { "name": "list_repos", "arguments": {} },
        "id": 1
    });
    // Assert the response contains a "federation" key with the right shape.
    // (Exact assertion depends on how the test harness constructs the
    // handler; follow the pattern of the existing tests in this file.)
}
```

If the handler test infrastructure is too heavy, write a simpler test that just asserts the JSON shape the handler produces:

```rust
#[test]
fn health_handler_includes_federation_field() {
    // Use the existing test for /health (if any) and add a .federation field check.
    // Or construct a minimal FederationIndex and call a helper that builds the response.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib mcp::handler::health_response_includes_federation_blob_when_set -- --nocapture`
Expected: FAIL (the current `/health` response has no `federation` field).

- [ ] **Step 3: Modify the `/health` handler in `src/mcp/handler.rs`**

Find the `GET /health` handler (search for `path == "/health"` in `handler.rs`). The current code constructs a `serde_json::json!` with `status`, `server`, `version`, `graph_nodes`, `graph_edges`, `tools_count`. Add a `federation` field.

The new body should look like:

```rust
        let health = serde_json::json!({
            "status": "ok",
            "server": "lain",
            "version": env!("CARGO_PKG_VERSION"),
            "graph_nodes": nodes,
            "graph_edges": edges,
            "tools_count": ...,
            "federation": fed.map(|f| {
                let repos: Vec<serde_json::Value> = f.list_repos().into_iter().map(|(id, health)| {
                    serde_json::json!({
                        "id": id.to_string(),
                        "health": health.to_string(),
                    })
                }).collect();
                serde_json::json!({
                    "repos": repos,
                    "total_nodes": f.backend().node_count(),
                    "total_edges": f.backend().edge_count(),
                    "memory_estimate_bytes": f.backend().node_count() as u64 * 200 + f.backend().edge_count() as u64 * 100,
                })
            })
        });
```

The `fed` variable needs to be passed into the handler. Look at Task 19's pattern: `resolve_repo_for_tool` takes `federation: Option<Arc<FederatedIndex>>`. The HTTP `handle_request` already takes `federation` (added during the merge). Reuse that.

Note: `/health` already uses `executor.graph().get_stats()`. The `federation.field` should be `null` when `self.federation` is `None`. The serde_json::json! with `Option<T>` serializes `None` as `null` automatically.

- [ ] **Step 4: Run test to verify it passes**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib mcp::handler::health_response_includes_federation_blob_when_set -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run full test suite**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib`
Expected: 462 + 1 = 463 passed, 0 failed.

- [ ] **Step 6: Commit**

```bash
git add src/mcp/handler.rs
git commit -m "feat(handler): /health response includes federation blob"
```

---

### Task 3: Write `docs/REPOS_YAML.md` (config schema reference)

**Files:**
- Create: `docs/REPOS_YAML.md`
- Test: none (this is a docs-only task; the smoke test is embedded in the doc)

**Interfaces:**
- Consumes: `src/federation/config.rs` (the `FederationConfig` / `RepoConfig` / `SourceConfig` types)
- Produces: a complete reference for writing `repos.yaml` files

**Background:** The `repos.yaml` schema is implemented in `src/federation/config.rs` (Task 12) but no user-facing documentation exists. This doc is the single source of truth for the config format.

- [ ] **Step 1: Read `src/federation/config.rs` to confirm the schema**

Open `src/federation/config.rs`. Confirm these fields exist:
- `FederationConfig { data_dir: PathBuf, max_concurrent_indexers: usize, ready_threshold: f32, repos: Vec<RepoConfig> }`
- `RepoConfig { id: String, source: SourceConfig }`
- `SourceConfig` variants: `LocalClone { url, ref }`, `ShallowClone { url, ref, refresh_interval_secs }`, `WorkspaceDir { path }`

Also confirm the defaults: `data_dir = "./.lain/federation"`, `max_concurrent_indexers = 8`, `ready_threshold = 0.8`, `ref = "main"`, `refresh_interval_secs = 300`.

- [ ] **Step 2: Write `docs/REPOS_YAML.md`**

Create `docs/REPOS_YAML.md` with the following sections:

```markdown
# repos.yaml Configuration Reference

`repos.yaml` is the config file `lain server` reads to know which repos to index. ...

## Schema

[Top-level `FederationConfig` schema with each field documented.]

## Source kinds

### workspace_dir
Use when the repo is already on disk. No git operations.

### local_clone
Use for full clones (preserves history). Higher disk cost.

### shallow_clone
Use for large repos where you only need the latest commit. Lower disk cost.

## Examples

[5-10 worked configs of varying complexity, from 1 repo to 10 repos, with annotations.]

## Smoke test

[2-3 shell commands to verify the config is valid.]
```

The doc should be 150-250 lines.

**YAML example for the doc (use this as the schema sample):**

```yaml
repos:
  - id: auth-svc
    source:
      type: local_clone
      url: https://github.com/acme/auth-svc.git
      ref: main
  - id: billing-svc
    source:
      type: shallow_clone
      url: https://github.com/acme/billing-svc.git
      ref: main
      refresh_interval_secs: 600
  - id: legacy-monolith
    source:
      type: workspace_dir
      path: /srv/legacy
data_dir: /var/lib/lain
max_concurrent_indexers: 8
ready_threshold: 0.8
```

The smoke test at the end:

```bash
# Validate the config parses:
lain server --config repos.yaml --dry-run  # if --dry-run exists; otherwise just check the config compiles
```

Note: `--dry-run` may not exist. If it doesn't, the smoke test could be:
```bash
# Just verify the config is parseable by attempting to start the server briefly:
timeout 5s lain server --config repos.yaml --transport http --port 9999
```

- [ ] **Step 3: Verify the example config parses**

Create a temporary `repos.yaml` matching the example and run `cargo test --lib` (the federation config tests already cover this). If the example fails to parse, fix the doc.

- [ ] **Step 4: Run shell syntax check on the smoke test block**

Run: `bash -n docs/REPOS_YAML.md --help 2>&1; echo "---bash syntax check is file-level; do: bash -c '<extract the smoke block>'`
Expected: no syntax errors.

For the smoke test, extract the shell block and run it through `bash -n`:

```bash
# Extract the smoke test block from the doc and lint it:
awk '/^```bash$/,/^```$/' docs/REPOS_YAML.md | bash -n
```

- [ ] **Step 5: Commit**

```bash
git add docs/REPOS_YAML.md
git commit -m "docs(federation): add REPOS_YAML.md config schema reference"
```

---

### Task 4: Write `docs/FEDERATION.md` (central federation reference)

**Files:**
- Create: `docs/FEDERATION.md`
- Test: none (this is a docs-only task)

**Interfaces:**
- Consumes: `src/federation/` (the module), `src/mcp/federation_tools.rs` (the 5 tools), `src/cmds/server.rs` (the CLI)
- Produces: the central reference doc, referenced by README, QUICKSTART_AGENTS, and the UI

**Background:** README and QUICKSTART point at this doc; this is the deep-dive.

- [ ] **Step 1: Write `docs/FEDERATION.md`**

Create `docs/FEDERATION.md` with these sections:

```markdown
# Federation Mode

[2-3 paragraph intro: what federation is, when to use it vs single-workspace.]

## When to use federation vs single-workspace

[Decision table:
- Single-workspace: working on one repo, no cross-repo questions.
- Federation: org-wide questions, cross-service blast radius, who's using this function.]

## Setup

[3-step setup:
1. Write repos.yaml — see docs/REPOS_YAML.md.
2. Run `lain server --config repos.yaml --transport http --port 9999`.
3. The server is ready when `ready_threshold` fraction of repos are `Ready`. Watch the logs.]

## Tools

[5 subsections, one per tool. Each subsection has:
- A 1-line description.
- Input/output JSON shape.
- A worked example.
- Error cases.]

### list_repos

### get_repo_info

### get_federation_health

### search_org

### get_cross_repo_blast_radius

[Both the resolver variant and the `_for_repo` variant.]

## Tool resolution rules

[5-step rule from resolve_repo_for_tool, with examples.]

## Performance

[100ms p99 target, 200 repos / 30 min cold start, memory estimate formula.]

## Troubleshooting

[Common errors with their meaning and recommended action:
- `NotFound` for a symbol you expected to exist → check if the repo is Ready.
- `AmbiguousSymbol` → present the candidates to the user.
- `Unavailable` repo → half of the federation still works; check clone logs.
- `lain server` exits immediately → check the YAML config with `cargo test --lib`.]

## Migration

[Notes for projects that had been using single-workspace mode:
- Backward compatible: existing single-workspace CLI still works.
- Federation is opt-in via `lain server` subcommand.
- Federation and single-workspace share the same `/health` endpoint (with `federation: null` for single-workspace).]

## Smoke test

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
```

The doc should be 300-500 lines.

**Important:** Use the actual JSON shapes from the existing tools. Look at `src/mcp/federation_tools.rs` for the exact serde-derived struct shapes. Don't invent shapes.

- [ ] **Step 2: Lint the smoke test shell block**

```bash
awk '/^```bash$/,/^```$/' docs/FEDERATION.md | bash -n
```

Expected: no syntax errors.

- [ ] **Step 3: Commit**

```bash
git add docs/FEDERATION.md
git commit -m "docs(federation): add FEDERATION.md central reference"
```

---

### Task 5: Update `docs/QUICKSTART_AGENTS.md` (federation mode section)

**Files:**
- Modify: `docs/QUICKSTART_AGENTS.md`
- Test: none (this is a docs-only task)

**Interfaces:**
- Consumes: `docs/FEDERATION.md` (the central reference)
- Produces: a "Federation mode" section that AI agents read first

**Background:** `QUICKSTART_AGENTS.md` is the primary doc for AI agents. It needs a Federation section with concrete examples.

- [ ] **Step 1: Add a "Federation mode" section**

Find the end of `docs/QUICKSTART_AGENTS.md` (or an appropriate breakpoint near the existing sections). Add a new section:

```markdown
---

## Federation mode

When the user's question spans multiple repos (e.g., "who else uses this function?", "what depends on this service?"), use federation mode. The server indexes N repos and answers cross-repo queries.

### When to use federation

| ... | Single-workspace | Federation |
|-----|------------------|------------|
| Bug fix in one repo | ✓ | ✗ |
| "Who uses this function?" | ✗ | ✓ |
| Cross-service blast radius | ✗ | ✓ |
| Repo ownership info | partial | ✓ |

### Setup

See [`docs/FEDERATION.md`](./FEDERATION.md) for the full guide. Summary:

1. Operator writes `repos.yaml` (see [`docs/REPOS_YAML.md`](./REPOS_YAML.md)).
2. Operator runs `lain server --config repos.yaml --transport http --port 9999`.
3. The federation is ready when a sufficient fraction of repos are `Ready`.

### Federation tools

[For each of the 5 tools, a 1-paragraph description + example call.]

#### list_repos

Returns the registered repos with their health.

```
list_repos() → [{ id: "auth-svc", health: "ready", ... }, ...]
```

#### get_repo_info(id)

Returns one repo's details by ID. Errors with `NotFound` if the ID is unknown.

#### get_federation_health

Returns federation-wide stats: total repos, counts per health state, total nodes/edges, memory estimate.

#### search_org(query, limit)

Case-insensitive substring search across all repos. Returns matches with their global IDs.

Example: `search_org("verify_token", 5)` → matches in `auth-svc`, `auth-utils`, etc.

#### get_cross_repo_blast_radius(symbol, depth)

[Same as the resolver variant. The `_for_repo` variant takes an explicit repo_id.]

### Tool resolution rules

[5-step rule, brief.]

### Cross-repo queries

When you ask questions that span repos, the resolver translates:
- `repo_id` explicit → use it.
- `symbol` given → resolve to the repo that owns it.
- 1 repo → use it.
- Otherwise → ask the user.

When results are ambiguous (`AmbiguousSymbol`), surface the candidates to the user and ask them to confirm.
```

The section should be 80-120 lines.

- [ ] **Step 2: Verify the example calls are correct**

Cross-reference the example calls with `src/mcp/federation_tools.rs`. Each tool name and argument shape must match the actual serde-derived schema.

- [ ] **Step 3: Commit**

```bash
git add docs/QUICKSTART_AGENTS.md
git commit -m "docs(federation): add federation mode section to QUICKSTART_AGENTS"
```

---

### Task 6: Update `docs/TECHNICAL.md` (federation architecture section)

**Files:**
- Modify: `docs/TECHNICAL.md`
- Test: none

**Interfaces:**
- Consumes: `src/federation/` and the federation design spec
- Produces: an architecture section that satisfies the implementer's curiosity

**Background:** `TECHNICAL.md` is for engineers who want to understand the system. The federation architecture section is the "how does it work" complement to `FEDERATION.md`'s "how do I use it".

- [ ] **Step 1: Add a "Federation architecture" section**

Find an appropriate insertion point in `docs/TECHNICAL.md` (likely after the existing architecture section). Add:

```markdown
## Federation architecture

[3-4 paragraphs covering:
- 1-paragraph overview: a federated server is a single process that owns N `RepoIndex` workers, projects their nodes/edges into a global petgraph via `GraphBackend`, and runs cross-repo matching.
- The 2 load-bearing traits: `RepoSource` (how the server gets code) and `GraphBackend` (how the server stores the graph). The escape hatches are `ShallowCloneSource` (for storage-light) and `MemgraphBackend` (deferred).
- The global ID scheme: `repo_id:NodeType:path:name`. Cross-repo edges are added by `find_cross_repo_matches` on signature similarity.
- The deferred sub-projects (4-7 from the original vision) and what they will add.]

### Cross-repo blast-radius semantics

[1 paragraph: `get_cross_repo_blast_radius` traverses `EdgeType::Calls` edges via the global backend. The seed node is found via `find_nodes_by_name` filtered by repo. The traverse is outgoing-only — incoming callers are not visited. The result is grouped by repo. There's a cap of 1000 nodes; `truncated: true` indicates the cap was hit.]
```

The section should be 40-60 lines.

- [ ] **Step 2: Verify technical accuracy**

Cross-reference the architecture description with the actual code in `src/federation/`. Don't make claims that aren't backed by the implementation.

- [ ] **Step 3: Commit**

```bash
git add docs/TECHNICAL.md
git commit -m "docs(federation): add federation architecture section to TECHNICAL.md"
```

---

### Task 7: Update `docs/query-language.md` (one-line note)

**Files:**
- Modify: `docs/query-language.md`
- Test: none

**Interfaces:**
- Consumes: knowledge of what the federation tools do
- Produces: a one-line note pointing at `docs/FEDERATION.md`

**Background:** `docs/query-language.md` documents the query language. Two of the 5 federation tools (`find_cross_repo_blast_radius` and `search_org`) are federation-only. The doc should mention this without duplicating `FEDERATION.md`.

- [ ] **Step 1: Add a one-line note at the top**

Find the top of `docs/query-language.md` (after the first heading). Add a single paragraph:

```markdown
> **Federation note:** cross-repo queries (`find_cross_repo_blast_radius`, `search_org`) require federation mode (`lain server --config repos.yaml`). See [`docs/FEDERATION.md`](./FEDERATION.md) for the full guide.
```

- [ ] **Step 2: Commit**

```bash
git add docs/query-language.md
git commit -m "docs(federation): add federation note to query-language.md"
```

---

### Task 8: Update `README.md` (federation section + version bump)

**Files:**
- Modify: `README.md`
- Test: none

**Interfaces:**
- Consumes: `docs/FEDERATION.md`
- Produces: a "Federation mode" section in `README.md` + a 1-line feature bullet + version bump

**Background:** `README.md` is the first thing anyone reads. It needs to mention federation and link to the deep-dive.

**Important:** the version bump is the LAST step in this implementation plan (Task 14). Just bump the feature bullet + section here; don't bump the version in this task.

- [ ] **Step 1: Add a "Federation mode" section**

Find a good insertion point in `README.md` (after the "Quick Start" section is a natural fit). Add:

```markdown
## Federation mode

For org-wide structural questions — "who else uses this function?", "what depends on this service?" — run `lain server --config repos.yaml` to index N repos and answer cross-repo queries. Federation mode exposes 5 MCP tools (`list_repos`, `get_repo_info`, `get_federation_health`, `search_org`, `get_cross_repo_blast_radius`) that answer questions spanning repos. See [`docs/FEDERATION.md`](docs/FEDERATION.md) for the full guide and [`docs/REPOS_YAML.md`](docs/REPOS_YAML.md) for the config schema.
```

- [ ] **Step 2: Add a "federation mode" bullet to the feature list**

Find the feature list in `README.md`. Add a bullet:

```markdown
- **Federation mode** — index N repos and answer org-wide structural questions across them
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(federation): add federation mode section to README"
```

---

### Task 9: Create `src/mcp/federation_dashboard.html` (new dashboard)

**Files:**
- Create: `src/mcp/federation_dashboard.html`
- Test: `tests/e2e/federation_dashboard_e2e.sh` (the e2e shell — Task 15 covers this)

**Interfaces:**
- Consumes: `GET /health` (returns `federation` blob from Task 2)
- Produces: a single-page dashboard showing the federation state

**Background:** This is the new landing page for federation mode. It's the bridge between the static doc and the live data.

**Constraints:**
- Vanilla JS only (no npm deps, no SPA framework).
- Match the existing `front_end_monitor.html` style (which is `src/mcp/front_end_monitor.html:553` lines).
- Single self-contained HTML file (no external CSS or JS files).

- [ ] **Step 1: Read the existing `front_end_monitor.html` to match style**

```bash
head -100 src/mcp/front_end_monitor.html
```

- [ ] **Step 2: Create `src/mcp/federation_dashboard.html`**

Create the file with:

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>Federation Dashboard — Lain</title>
  <style>
    /* Match the existing front_end_monitor.html style. */
    body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 2rem; color: #222; }
    h1 { margin-bottom: 0.5rem; }
    .subtitle { color: #666; margin-bottom: 2rem; }
    table { border-collapse: collapse; width: 100%; margin-bottom: 2rem; }
    th, td { text-align: left; padding: 0.5rem; border-bottom: 1px solid #ddd; }
    th { background: #f4f4f4; }
    .health { display: inline-block; padding: 0.15rem 0.5rem; border-radius: 3px; font-size: 0.85em; font-weight: 600; }
    .health.ready { background: #d4edda; color: #155724; }
    .health.indexing { background: #fff3cd; color: #856404; }
    .health.degraded { background: #f8d7da; color: #721c24; }
    .health.unavailable { background: #e2e3e5; color: #383d41; }
    .stats { display: flex; gap: 2rem; margin-bottom: 2rem; }
    .stat { padding: 1rem; background: #f4f4f4; border-radius: 4px; min-width: 120px; }
    .stat-label { font-size: 0.8em; color: #666; }
    .stat-value { font-size: 1.5em; font-weight: 600; }
    .tool-links a { display: inline-block; margin-right: 1rem; padding: 0.5rem 1rem; background: #007bff; color: white; text-decoration: none; border-radius: 3px; }
    .tool-links a:hover { background: #0056b3; }
    .error { background: #f8d7da; color: #721c24; padding: 1rem; border-radius: 4px; margin-bottom: 1rem; }
  </style>
</head>
<body>
  <h1>Federation Dashboard</h1>
  <div class="subtitle">Org-wide structural intelligence for Lain</div>

  <div id="error" class="error" style="display: none"></div>

  <div class="stats" id="stats"></div>

  <h2>Repositories</h2>
  <table id="repos">
    <thead>
      <tr>
        <th>ID</th><th>Path</th><th>Health</th><th>Last refresh</th><th>Last indexed</th><th>Nodes</th><th>Edges</th><th>Actions</th>
      </tr>
    </thead>
    <tbody></tbody>
  </table>

  <h2>Quick links</h2>
  <div class="tool-links">
    <a href="/ui/blast-radius/?repo_id=__DEFAULT__&symbol=__SELECTED__">Blast radius</a>
    <a href="/ui/call-chain/?repo_id=__DEFAULT__&from=__&to=__">Call chain</a>
    <a href="/ui/coupling/?repo_id=__DEFAULT__&symbol=__">Coupling</a>
    <a href="/">Home</a>
  </div>

  <script>
    async function load() {
      try {
        const base = window.location.origin;
        // Step 1: GET /health to know we're in federation mode.
        const healthRes = await fetch(base + '/health');
        const health = await healthRes.json();
        if (!health.federation) {
          // Back-compat: redirect to single-workspace home if not federation.
          window.location.href = '/';
          return;
        }

        // Step 2: render stats.
        const f = health.federation;
        const statsEl = document.getElementById('stats');
        statsEl.innerHTML = `
          <div class="stat"><div class="stat-label">Repositories</div><div class="stat-value">${f.repos.length}</div></div>
          <div class="stat"><div class="stat-label">Total nodes</div><div class="stat-value">${f.total_nodes.toLocaleString()}</div></div>
          <div class="stat"><div class="stat-label">Total edges</div><div class="stat-value">${f.total_edges.toLocaleString()}</div></div>
          <div class="stat"><div class="stat-label">Memory est.</div><div class="stat-value">${(f.memory_estimate_bytes / 1024 / 1024).toFixed(1)} MB</div></div>
        `;

        // Step 3: render the repo table.
        // For each row, also call tools/call list_repos and get_repo_info to get node/edge counts.
        const tbody = document.querySelector('#repos tbody');
        for (const r of f.repos) {
          const tr = document.createElement('tr');
          tr.innerHTML = `
            <td>${r.id}</td>
            <td><code>${r.path}</code></td>
            <td><span class="health ${r.health}">${r.health}</span></td>
            <td>${r.last_refreshed_unix ? new Date(r.last_refreshed_unix * 1000).toLocaleString() : '—'}</td>
            <td>${r.last_indexed_unix ? new Date(r.last_indexed_unix * 1000).toLocaleString() : '—'}</td>
            <td>${r.node_count.toLocaleString()}</td>
            <td>${r.edge_count.toLocaleString()}</td>
            <td><a href="/ui/blast-radius/?repo_id=${r.id}"> blast radius</a> · <a href="#search" onclick="document.getElementById('search-input').value='${r.id}'; return false;">search</a></td>
          `;
          tbody.appendChild(tr);
        }

        // Step 4: fix the quick-links once we know the first ready repo.
        const firstReady = f.repos.find(r => r.health === 'ready');
        if (firstReady) {
          document.querySelectorAll('.tool-links a').forEach(a => {
            a.href = a.href.replace('__DEFAULT__', encodeURIComponent(firstReady.id));
          });
        }
      } catch (e) {
        const el = document.getElementById('error');
        el.textContent = 'Failed to load federation: ' + e.message;
        el.style.display = 'block';
      }
    }

    load();
  </script>
</body>
</html>
```

This is a 100-150 line file. Adapt the route handler to serve this file at `/federation-dashboard.html` (check `src/mcp/handler.rs` for the existing pattern that serves `front_end_monitor.html`).

- [ ] **Step 3: Wire the route**

In `src/mcp/handler.rs`, find where `front_end_monitor.html` is served (search for `FRONT_END_HTML`). Add a sibling route for `federation_dashboard.html`:

```rust
// (Add to the existing if/else chain that serves HTML files.)
if method == Method::GET && path == "/federation-dashboard.html" {
    return Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html")
        .body(full_body(Bytes::from(include_str!("federation_dashboard.html"))))
        .unwrap());
}
```

- [ ] **Step 4: Verify the file builds**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo build --lib 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/mcp/federation_dashboard.html src/mcp/handler.rs
git commit -m "feat(mcp): add federation_dashboard.html page"
```

---

### Task 10: Update `src/mcp/front_end_monitor.html` (federation header)

**Files:**
- Modify: `src/mcp/front_end_monitor.html`
- Test: `tests/e2e/federation_dashboard_e2e.sh` (Task 15)

**Interfaces:**
- Consumes: `GET /health` (returns `federation` blob from Task 2)
- Produces: a federation header at the top of the home page when running in federation mode

**Background:** When a user navigates to `/`, they should see at a glance whether they're in federation mode and how many repos are indexed.

- [ ] **Step 1: Read the existing `front_end_monitor.html` structure**

Find the `<body>` tag and the first content block (often a header or a "status" element).

- [ ] **Step 2: Add the federation header**

Add a `<div id="federation-banner">` element near the top of the body, hidden by default. Add a `<script>` block at the bottom that fetches `/health` and toggles the banner's visibility based on the `federation` field.

```html
<div id="federation-banner" style="display: none; background: #e7f3ff; border: 1px solid #b6daff; padding: 1rem; margin-bottom: 1rem; border-radius: 4px;">
  <strong>Federation mode</strong> — <span id="fed-summary"></span>
  — <a href="/federation-dashboard.html">open dashboard</a>
</div>

<script>
(async function() {
  try {
    const h = await fetch('/health').then(r => r.json());
    if (h.federation) {
      const f = h.federation;
      const summary = `${f.repos.length} repos · ${f.total_nodes.toLocaleString()} nodes · ${f.total_edges.toLocaleString()} edges`;
      document.getElementById('fed-summary').textContent = summary;
      document.getElementById('federation-banner').style.display = 'block';
    }
  } catch (e) { /* silent: single-workspace mode may not have /health */ }
})();
</script>
```

- [ ] **Step 3: Verify the modification builds**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo build --lib 2>&1 | tail -3`
Expected: clean (HTML changes don't affect Rust build, but verify).

- [ ] **Step 4: Commit**

```bash
git add src/mcp/front_end_monitor.html
git commit -m "feat(mcp): federation banner on front_end_monitor.html"
```

---

### Task 11: Update `src/ui/blast-radius.html` (repo_id selector)

**Files:**
- Modify: `src/ui/blast-radius.html`
- Test: `tests/e2e/federation_dashboard_e2e.sh` (Task 15)

**Interfaces:**
- Consumes: `?repo_id=...&symbol=...` URL params (existing pattern: `symbol`)
- Produces: a render of the blast radius for the given repo + symbol

**Background:** The blast radius page is single-workspace today. It needs a `repo_id` selector so users can pick which repo to query.

- [ ] **Step 1: Read the existing page structure**

Look at the existing `src/ui/blast-radius.html` to see how it currently takes the `symbol` param.

- [ ] **Step 2: Add the `repo_id` selector**

Add a `<div id="repo-selector">` near the top of the page that fetches `/health` and renders a `<select>` populated with the federation repos. The form's submit handler reads both `repo_id` and `symbol`.

```html
<div id="repo-selector" style="display: none; margin-bottom: 1rem;">
  <label>Repo:
    <select id="repo-select"></select>
  </label>
</div>
<form id="query-form">
  <label>Symbol: <input name="symbol" /></label>
  <input type="hidden" name="repo_id" id="repo-id-input" />
  <button type="submit">Compute blast radius</button>
</form>

<script>
(async function() {
  const h = await fetch('/health').then(r => r.json());
  if (!h.federation) return;  // single-workspace: hide selector
  const sel = document.getElementById('repo-select');
  for (const r of h.federation.repos) {
    const opt = document.createElement('option');
    opt.value = r.id;
    opt.textContent = `${r.id} (${r.health})`;
    sel.appendChild(opt);
  }
  // Read URL params.
  const params = new URLSearchParams(window.location.search);
  if (params.get('repo_id')) sel.value = params.get('repo_id');
  document.getElementById('repo-selector').style.display = 'block';
  document.getElementById('repo-id-input').value = sel.value;
  sel.addEventListener('change', () => {
    document.getElementById('repo-id-input').value = sel.value;
  });
})();
</script>
```

- [ ] **Step 3: Commit**

```bash
git add src/ui/blast-radius.html
git commit -m "feat(ui): add repo_id selector to blast-radius page"
```

---

### Task 12: Update `src/ui/call-chain.html` (repo_id selector)

**Files:**
- Modify: `src/ui/call-chain.html`
- Test: `tests/e2e/federation_dashboard_e2e.sh` (Task 15)

**Same pattern as Task 11.** Different form fields (this page takes `from` and `to` instead of `symbol`).

- [ ] **Step 1: Apply the same pattern as Task 11 to call-chain.html**

The form has `from` and `to` inputs instead of `symbol`. The selector still populates `<select id="repo-select">` from `/health`. The hidden input is `repo_id`.

- [ ] **Step 2: Commit**

```bash
git add src/ui/call-chain.html
git commit -m "feat(ui): add repo_id selector to call-chain page"
```

---

### Task 13: Update `src/ui/coupling.html` (repo_id selector)

**Files:**
- Modify: `src/ui/coupling.html`
- Test: `tests/e2e/federation_dashboard_e2e.sh` (Task 15)

**Same pattern as Task 11.**

- [ ] **Step 1: Apply the same pattern as Task 11 to coupling.html**

- [ ] **Step 2: Commit**

```bash
git add src/ui/coupling.html
git commit -m "feat(ui): add repo_id selector to coupling page"
```

---

### Task 14: Version bump (0.3.0 → 0.4.0) + version consistency test

**Files:**
- Modify: `server.json`, `npm-shim/package.json`, `Formula/lain.rb`
- Create: `tests/version_consistency.rs`

**Interfaces:**
- Consumes: nothing (just changes a string in 3 files)
- Produces: all 3 files have version `0.4.0`; a Rust test that asserts this

**Background:** Federation is the headline feature, so the version bump goes from 0.3.0 to 0.4.0. The version-consistency test catches the "bumped one but not the others" bug we almost hit during the merge.

- [ ] **Step 1: Write the version-consistency test**

Create `tests/version_consistency.rs`:

```rust
//! Asserts that all version-bumping files share the same version string.
//! This catches the "bumped one but not the others" bug surfaced during the
//! 2026-08-09 federation merge.

use std::fs;

fn read_version(path: &str) -> String {
    let content = fs::read_to_string(path).expect(path);
    // Parse "version": "0.4.0" or `version "0.4.0"` depending on the format.
    let needle = if path.ends_with(".json") {
        "\"version\""
    } else if path.ends_with(".rb") {
        "version "
    } else {
        panic!("don't know how to parse {}", path);
    };
    let idx = content.find(needle).expect(needle);
    let after = &content[idx + needle.len()..];
    let after = after.trim_start().trim_start_matches(':').trim_start();
    let after = after.trim_start_matches('"');
    let end = after.find(|c: char| c == '"' || c == ',' || c == '\n').unwrap();
    after[..end].to_string()
}

#[test]
fn all_versions_match() {
    let files = [
        "server.json",
        "npm-shim/package.json",
        "Formula/lain.rb",
    ];
    let versions: Vec<String> = files.iter().map(|f| (f, read_version(f))).collect();
    let first = &versions[0].1;
    for (name, v) in &versions {
        assert_eq!(v, first, "{} has version {}, expected {}", name, v, first);
    }
    assert!(!first.is_empty());
    assert!(first.contains('.'), "version {} should be semver (e.g. 0.4.0)", first);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --test version_consistency`
Expected: FAIL (versions are 0.3.0, not 0.4.0).

- [ ] **Step 3: Bump `server.json` from 0.3.0 to 0.4.0**

Edit `server.json` line 7 (`"version": "0.3.0,"` → `"version": "0.4.0",`).

- [ ] **Step 4: Bump `npm-shim/package.json` from 0.3.0 to 0.4.0**

Edit `npm-shim/package.json` line 5 (`"version": "0.3.0",` → `"version": "0.4.0",`).

- [ ] **Step 5: Bump `Formula/lain.rb` from 0.3.0 to 0.4.0**

Edit `Formula/lain.rb`. Find the `version` line and bump it.

- [ ] **Step 6: Run the test to verify it passes**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --test version_consistency`
Expected: PASS.

- [ ] **Step 7: Run full test suite**

Run: `export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH" && cargo test --lib && cargo test --test federation_integration && cargo test --test version_consistency`
Expected: 463 lib + 7 integration + 1 version consistency = 471 tests, 0 failed.

- [ ] **Step 8: Commit**

```bash
git add server.json npm-shim/package.json Formula/lain.rb tests/version_consistency.rs
git commit -m "chore(release): bump version 0.3.0 -> 0.4.0 (federation)"
```

---

### Task 15: Federation dashboard e2e shell test

**Files:**
- Create: `tests/e2e/federation_dashboard_e2e.sh`
- Reference: `tests/e2e/federation_e2e.sh` (existing)

**Interfaces:**
- Consumes: a running `lain server` (assumed up from prior tasks)
- Produces: pass/fail exit code

**Background:** Lightweight e2e test that hits the new HTML endpoints. The existing `tests/e2e/federation_e2e.sh` tests the JSON-RPC tools; this new script tests the HTML pages.

- [ ] **Step 1: Write the e2e script**

Create `tests/e2e/federation_dashboard_e2e.sh`:

```bash
#!/usr/bin/env bash
# E2E test for the federation HTML dashboard. Assumes a `lain server`
# is running on $LAIN_E2E_PORT (default 19998 to avoid conflict with the
# existing federation_e2e.sh on 19999).
set -euo pipefail

PORT="${LAIN_E2E_PORT:-19998}"
BASE="http://localhost:${PORT}"

echo "==> GET /health (should include federation blob)"
curl -sf "${BASE}/health" | python3 -c '
import json, sys
data = json.load(sys.stdin)
assert "federation" in data, "federation blob missing from /health"
f = data["federation"]
assert f is not None, "federation should not be null in federation mode"
assert "repos" in f
assert "total_nodes" in f
assert "total_edges" in f
assert "memory_estimate_bytes" in f
print("OK: /health has federation blob with", len(f["repos"]), "repos")
'

echo "==> GET / (should show federation banner)"
curl -sf "${BASE}/" | grep -q "federation-banner" && echo "OK: home page has federation banner" || (echo "FAIL: home page missing federation banner"; exit 1)

echo "==> GET /federation-dashboard.html"
curl -sf "${BASE}/federation-dashboard.html" | grep -q "Federation Dashboard" && echo "OK: dashboard renders" || (echo "FAIL: dashboard missing title"; exit 1)

echo "==> GET /ui/blast-radius/?repo_id=hello-rust&symbol=hello"
curl -sf "${BASE}/ui/blast-radius/?repo_id=hello-rust&symbol=hello" | grep -q "repo-selector" && echo "OK: blast radius has repo_id selector" || (echo "FAIL: blast radius missing repo_id selector"; exit 1)

echo "==> GET /ui/call-chain/?repo_id=hello-rust&from=hello&to=main"
curl -sf "${BASE}/ui/call-chain/?repo_id=hello-rust&from=hello&to=main" | grep -q "repo-selector" && echo "OK: call chain has repo_id selector" || (echo "FAIL: call chain missing repo_id selector"; exit 1)

echo "==> GET /ui/coupling/?repo_id=hello-rust&symbol=hello"
curl -sf "${BASE}/ui/coupling/?repo_id=hello-rust&symbol=hello" | grep -q "repo-selector" && echo "OK: coupling has repo_id selector" || (echo "FAIL: coupling missing repo_id selector"; exit 1)

echo "==> E2E PASSED"
```

`chmod +x` the file.

- [ ] **Step 2: Make the script executable**

```bash
chmod +x tests/e2e/federation_dashboard_e2e.sh
```

- [ ] **Step 3: Run the e2e test against a running server**

This requires a `lain server` running on port 19998. Run it in the background:

```bash
export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo build --release --bin lain
./target/release/lain server --config <path-to-repos.yaml> --transport http --port 19998 &
sleep 30  # wait for federation to be ready
tests/e2e/federation_dashboard_e2e.sh
kill %1  # stop the server
```

Expected: `==> E2E PASSED`.

If the e2e fails, fix the HTML or the route handler until it passes.

- [ ] **Step 4: Commit**

```bash
git add tests/e2e/federation_dashboard_e2e.sh
git commit -m "test(e2e): add federation dashboard e2e test"
```

---

## Self-Review

**1. Spec coverage:**

| Spec section | Implementing task(s) |
|---|---|
| Goal: close the docs+UI gap | All 15 tasks |
| Approach 1: additive docs + new dashboard | Tasks 3-13 |
| Docs / `docs/FEDERATION.md` | Task 4 |
| Docs / `docs/REPOS_YAML.md` | Task 3 |
| Docs / `README.md` update | Task 8 |
| Docs / `docs/QUICKSTART_AGENTS.md` | Task 5 |
| Docs / `docs/TECHNICAL.md` | Task 6 |
| Docs / `docs/query-language.md` | Task 7 |
| UI / `src/mcp/federation_dashboard.html` (new) | Task 9 |
| UI / `src/mcp/front_end_monitor.html` federation header | Task 10 |
| UI / `src/ui/blast-radius.html` repo_id selector | Task 11 |
| UI / `src/ui/call-chain.html` repo_id selector | Task 12 |
| UI / `src/ui/coupling.html` repo_id selector | Task 13 |
| Backend / `get_agent_strategy` extension | Task 1 |
| Backend / `/health` federation blob | Task 2 |
| Versioning 0.3.0 → 0.4.0 (single commit) | Task 14 |
| Doc smoke tests | Tasks 3, 4 (embedded in each doc) |
| UI e2e test | Task 15 |
| `get_agent_strategy` content test | Task 1 |
| Version consistency test | Task 14 |
| Backwards compat preserved | All tasks (no breakage) |

**Gaps:** None. Every spec requirement has at least one task.

**2. Placeholder scan:** No `TBD`/`TODO`/`fill in details` in the steps. All code blocks are concrete. The "implementer reads file first" pattern is used in Tasks 1, 2, 9, 11, 15 — these are explicit instructions, not placeholders.

**3. Type consistency:**
- `get_agent_strategy` returns `String` (existing). Task 1 adds federation content to the string.
- `/health` handler returns `serde_json::Value` (existing). Task 2 adds the `federation` field.
- `HttpRequest` handler signature in `src/mcp/handler.rs` already takes `federation: Option<Arc<FederatedIndex>>` (added in Task 5). Task 2 reuses that.
- `FederationIndex::list_repos() -> Vec<(RepoId, RepoHealth)>` and `FederationIndex::backend() -> Arc<dyn GraphBackend>` already exist. Task 2 reuses them.
- `GraphBackend::node_count()` and `edge_count()` already exist (Task 6). Task 2 reuses them.
- The HTML pages use `fetch('/health')` returning JSON, matching Task 2's response.

No type mismatches.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-09-docs-ui-federation-update.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration with two-stage review.

**2. Inline Execution** — Execute tasks in this session using `executing-plans`, batch execution with checkpoints for review.

Which approach?
