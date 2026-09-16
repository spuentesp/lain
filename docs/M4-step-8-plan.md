# Plan — M4 step 8 + cancellation + spawn_blocking + annotation/comment layer

> **Audience:** a fresh agent instance executing this plan against the
> current `origin/dev` tip. The PR that consolidated the v0.7.4-rc1 release
> cleanup has merged to `dev` (merge commit `97cf979`). All version-bearing
> files now agree on `0.7.4-rc1`. Required CI checks are green.
>
> **Scope of this plan (in priority order):**
>
> 1. **M4 step 8** — federation per-repository readiness aggregation
> 2. **Cooperative cancellation token** — propagated through every long-running phase
> 3. **`spawn_blocking` isolation** — synchronous work moved off the async runtime
> 4. **Annotation / comment layer** — symbol/edge annotations + agent handoff notes
>
> **Not in this plan** (separate scope, future plans): the remaining
> roadmap items (M5 bootstrap context, M6 small Agent API, M8 client
> recipes, M9 distribution acceptance CI). All are multi-day scope.

---

## 0. Pre-flight: ground state for the fresh instance

Run these before writing any code:

```bash
git fetch origin dev --quiet
git switch -c feat/m4-step-8 origin/dev    # or `docs/m4-step-8-plan` if you want this plan alongside
git log --oneline origin/main..HEAD        # confirm you're 0 commits ahead of dev (or know what's there)
cargo build --workspace --all-targets      # must be clean
cargo fmt --check                          # must be clean
cargo clippy --workspace --all-targets -- -D warnings \
    -A clippy::style -A clippy::complexity -A clippy::perf \
    -A clippy::pedantic -A unused -A dead_code
```

Working version on this branch is **`0.7.4-rc1`** (every file:
`Cargo.toml`, `server.json`, `npm-shim/package.json`,
`npm-shim/package-lock.json`, `Formula/lain.rb`). Don't bump to
`0.7.4` or `0.8.0` in this work; release PR is a separate scope.

---

## 1. M4 step 8 — federation per-repository readiness aggregation

**Why this is its own milestone:** the federation backend already has
per-repo indexing state (`RepoIndex::indexed_signal`, `RepoIndex::state`)
and a federation-wide projection (`FederatedIndex::aggregate_state`).
What's missing is the *deterministic, gate-able aggregation* the
implementation sequence asks for: every MCP tool that operates on the
federation aggregate must report a per-repo readiness state so a
caller can decide whether to wait, fail, or proceed.

### 1.1 Where the work goes

| File | What to add |
|---|---|
| `src/server/federation/repo_index.rs` | Add `pub fn readiness_state(&self) -> PerRepoReadiness` returning the per-repo state for this `RepoIndex`. Use the existing `indexed_signal` + `state` fields; do not introduce a second state model. |
| `src/server/federation/federated_index.rs` | Add `pub fn per_repo_readiness(&self) -> BTreeMap<RepoId, PerRepoReadiness>`. Snapshot under the same lock as `aggregate_state`. |
| `src/server/readiness.rs` | Extend `CapabilitySnapshot` (or sibling DTO) with `per_repo: BTreeMap<RepoId, PerRepoReadiness>` field. Serialize in the existing `get_capabilities` MCP output. |
| `src/server/mcp/handler.rs` | In every tool handler whose classification is `ReadinessRequirement::FederationAggregate` (grep for that enum variant in `src/server/mcp/definitions.rs`), gate on the new per-repo readiness field — refuse if any required repo is `warming_up` past the existing 200 ms budget. |
| `tests/mcp_cold_start.rs` (or new `tests/federation_readiness.rs`) | Add a test: spin up two repos in a federation, force one to a `warming_up` barrier, call a federation-aggregate tool, assert the structured `warming_up` response names the specific repo by id. |

### 1.2 Definition: `PerRepoReadiness`

```rust
pub struct PerRepoReadiness {
    pub repo_id: RepoId,
    pub state: RepoIndexState,           // existing enum in repo_index.rs
    pub indexed_signal: bool,            // true once index() has fired
    pub last_indexed_commit: Option<git2::Oid>,
    pub last_indexed_at_unix_ms: Option<u64>,
    pub outstanding_files: u64,          // from the watcher (see section 1.4)
    pub staleness: Staleness,            // fresh | stale_usable | stale_unusable
}
```

`Staleness` matches the existing per-tool readiness taxonomy in
`src/server/readiness.rs`; do not invent a parallel vocabulary.

### 1.3 Acceptance

- `get_capabilities` JSON output includes `per_repo` keyed by `repo_id`
  for every repo in the federation
- A federation-aggregate tool called during a per-repo warm-up returns
  structured `warming_up` with `repo_id` populated, not a partial
  answer
- The gate logic reuses the existing `ReadinessRequirement` enum and
  the existing `if always()` pattern from `src/server/mcp/handler.rs`
  — no second gating mechanism
- All existing `tests/mcp_cold_start.rs` tests still pass

---

## 2. Cooperative cancellation token

**Why this is its own section:** every long-running phase (file scan,
graph ingest, federation index, watcher reconcile) currently runs to
completion or aborts via tokio's task abort. There's no
cooperative-cancel signal the MCP layer can send when a client
disconnects, when a graceful shutdown is requested, or when the
parent task wants to stop early.

### 2.1 The token

Add a single shared `CancellationToken` field to `LainServer` (or a
new `RunContext` if the layering works better — see the docstring at
`src/server/ingest/server.rs` for the existing layering).

```rust
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct RunContext {
    pub cancel: CancellationToken,
    pub health: Arc<Health>,
    // ...existing fields
}
```

Pass `RunContext` (or just the token) through every long-running
phase. On `cancel.cancelled()`, each phase must:
- Stop reading from the source
- Flush whatever partial state it has to disk (graph store accepts partial)
- Exit with `Ok(Cancelled)` (a new variant on whatever the phase's
  result enum is) — NOT `Err`

### 2.2 Where to plumb

| Phase | Current location | Plumb the token through |
|---|---|---|
| `RepoIndex::index` | `src/server/federation/repo_index.rs` | Already takes `&self` + args; add `cancel: &CancellationToken` parameter. Check at every file boundary (not in inner loops — that's too noisy). |
| `RepoSource::clone_into` | `src/server/federation/repo_source.rs` | Same — add `cancel` parameter |
| `Workspace::reload` | `src/server/federation/workspace.rs` | Same |
| `Watcher` reconcile loop | `src/server/watcher.rs` | Already uses `select!`; add `cancel.cancelled()` arm |
| Federation aggregate rebuild | `src/server/federation/federated_index.rs` | Same |
| MCP handler request body collect | `src/server/mcp/handler.rs` | The `/mcp` handler already awaits `Limited::collect()`; pass `cancel` so a client disconnect cancels the body read |

### 2.3 Cancellation triggers

- **Client disconnect** — the `/mcp` request handler's task should
  register `cancel.cancel()` on connection drop. Use `hyper`'s
  `connection`-level cancellation, not per-request (the server keeps
  running; only the in-flight request dies).
- **Graceful shutdown** — the existing `signal::shutdown_signal()`
  path should call `cancel.cancel()` once, then join all spawned
  tasks. Add a 30-second join budget; force-abort after that.
- **Manual abort** — a new `POST /admin/cancel` route that triggers
  the same `cancel.cancel()`. Not exposed in MCP tool list.

### 2.4 Acceptance

- Killing a client mid-`/mcp` request (TCP RST) does not leave the
  server hung in a `select!`-blocked future
- Sending SIGINT to the server, after the cancellation token fires,
  completes shutdown within 30 seconds; before, it could hang on a
  partial index forever
- `cargo test --workspace --all-targets` still passes; existing
  timeout tests don't get faster (cancellation doesn't shorten
  legitimate work, only aborts it)

---

## 3. `spawn_blocking` isolation

**Why this is its own section:** `src/server/federation/repo_index.rs:109`
already notes in its docstring that the pipeline should use
`spawn_blocking` for the synchronous git plumbing. Right now the git
calls run inline on the tokio runtime — slow, blocking the executor,
and the reason federation indexing has measurable latency on cold boot.

### 3.1 What to move

Find every synchronous git/filesystem call on the async runtime:

```bash
grep -rn "repo\.statuses\|repo\.revparse\|repo\.log\|repo\.diff\|repo\.blob\|fs::read\|fs::write\|fs::metadata\|git2::" src/ 2>&1 \
  | grep -v "test\|spawn_blocking\|// " \
  | head -30
```

For each hit, decide:
- If it's a **one-shot** sync call inside an async function → wrap in `spawn_blocking`
- If it's **inside a hot loop** (per-file during index) → consider
  batching (see 3.2)
- If it's **inside a `#[tokio::test]`** → no change; tests are fine

### 3.2 The hot loop case

`RepoIndex::index_one_repo` walks every tracked file, calling
`git2::Repository::statuses` per path. That's per-file blocking work.
Don't wrap each call in `spawn_blocking` — the overhead would dominate.
Instead:

1. Collect every `(path, mode)` into a `Vec<PathBuf>`
2. One `spawn_blocking` that walks the whole list sequentially
3. Stream the results back via a channel as they're produced (so
   downstream work can start before the whole walk finishes)

Same pattern for any other "do this for every file" loop.

### 3.3 The blocking helper

Put this in `src/server/ingest/blocking.rs` (new file):

```rust
/// Wrap a synchronous closure in `spawn_blocking` with consistent
/// error mapping. Use this *only* for sync work that has to run; never
/// for parallelism that already exists.
pub async fn offthread<F, T>(cancel: &CancellationToken, f: F) -> Result<T, LainError>
where
    F: FnOnce() -> Result<T, LainError> + Send + 'static,
    T: Send + 'static,
{
    let cancel = cancel.clone();
    let handle = tokio::task::spawn_blocking(move || f());
    tokio::select! {
        res = handle => res,
        _ = cancel.cancelled() => Err(LainError::Cancelled),
    }
}
```

Use `offthread` instead of bare `spawn_blocking` everywhere in this
section so the cancellation token is wired in for free.

### 3.4 Acceptance

- Federation cold-boot wall-clock time on the canonical fixture drops
  measurably (record the baseline first; compare after)
- `tokio-console` (if added) or `RUST_LOG=trace` shows file-walk
  phases happening on blocking threads, not on the async runtime
- No new panics, no lost cancellation (the `select!` arm above
  ensures the spawned task is dropped, not left running)

---

## 4. Annotation / comment layer

**Why this is its own section:** the existing PR-comment machinery
(`.github/actions/lain-health-badge/`) is a **CI-side** artifact — it
calls LAIN's MCP tools, formats the output, posts it as a sticky PR
comment via `marocchino/sticky-pull-request-comment@v2.9.4`. What's
missing is the **agent-side** mirror: an agent that calls
`explain_symbol` should get back prior agents' notes about that
symbol. The two sides share the same data model.

### 4.1 The asymmetry to fix

| | Today | After this plan |
|---|---|---|
| **CI → humans** | Rich sticky PR comment (server health, architectural observations, per-PR blast radius for new symbols) | Same, plus annotation summary (open notes on symbols touched by the PR), plus previous-run delta |
| **Agent → humans** | Nothing (no `add_comment` / `add_note` MCP tool) | `add_annotation`, `list_annotations`, `resolve_annotation`, `leave_handoff_note`, `get_pending_handoffs` |
| **Agent → next agent** | Nothing | `leave_handoff_note` / `get_pending_handoffs` (note + presence infra already has agent-id and TTL) |

### 4.2 Storage

New module `src/server/annotations.rs`:

```rust
pub struct Annotation {
    pub id: AnnotationId,
    pub target: AnnotationTarget,    // Symbol | Edge | File | Repo
    pub kind: AnnotationKind,        // Note | Warning | Todo | Investigation | Fix
    pub body: String,                // 1..=4096 bytes, validated
    pub author: AgentId,             // presence subsystem
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub status: AnnotationStatus,    // Open | Resolved | Stale
    pub resolved_by: Option<AgentId>,
    pub resolved_at_unix_ms: Option<u64>,
    pub refs: Vec<AnnotationTarget>, // cross-references
}
```

Persistence: per-repo sqlite table, owned by `RepoIndex` (so
federation propagation reuses the existing channel):

```sql
CREATE TABLE annotations (
    id          TEXT PRIMARY KEY,
    target_kind TEXT NOT NULL,    -- 'symbol' | 'edge' | 'file' | 'repo'
    target_id   TEXT NOT NULL,    -- canonical id (symbol fqn, edge src→dst, file path, repo id)
    kind        TEXT NOT NULL,
    body        TEXT NOT NULL,
    author      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'open',
    resolved_by TEXT,
    resolved_at INTEGER,
    refs_json   TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX annotations_target ON annotations(target_kind, target_id);
CREATE INDEX annotations_author ON annotations(author);
CREATE INDEX annotations_status ON annotations(status);
```

### 4.3 MCP tools

Add to `src/server/mcp/definitions.rs`:

```rust
"add_annotation": {
    input: { target: TargetSpec, body: String, kind: AnnotationKind, refs?: Vec<TargetSpec> },
    output: { id: AnnotationId, created_at_unix_ms: u64 },
    readiness: Always,
},
"list_annotations": {
    input: { target?: TargetSpec, author?: AgentId, kind?: AnnotationKind, status?: AnnotationStatus, limit?: u32 /* default 100 */ },
    output: { annotations: Vec<Annotation> },
    readiness: Always,
},
"resolve_annotation": {
    input: { id: AnnotationId, note?: String },
    output: { resolved: Annotation },
    readiness: Always,
},
"leave_handoff_note": {
    input: { body: String, scope?: String /* "workspace" | "repo:<id>" | "agent_kind:<k>" */, refs?: Vec<TargetSpec> },
    output: { id: AnnotationId, expires_at_unix_ms: u64 },
    readiness: Always,
},
"get_pending_handoffs": {
    input: { scope?: String, since_unix_ms?: u64 },
    output: { handoffs: Vec<Annotation> },
    readiness: Always,
},
```

`TargetSpec` is a discriminated union:

```rust
{ symbol: "fn validate_token" }
| { edge: { from: "fn foo", to: "fn bar" } }
| { file: "src/auth.rs" }
| { repo: "auth-svc" }
```

Canonicalization: `file` and `repo` paths go through
`crate::server::path_util::posix_string` so what the agent sends
matches what `explain_symbol` / `get_blast_radius` already return.

### 4.4 Auto-include in existing tool outputs

**`explain_symbol`** (`src/server/tools/handlers/explain_symbol.rs`):
add an `annotations: Option<Vec<AnnotationSummary>>` field to the
output. `None` if no open annotations exist; `Some(vec)` if there are
any. Only open + not-stale annotations are included. Same for
`get_blast_radius` (per impacted symbol).

**`get_capabilities`** (no change needed — annotations are queryable
via `list_annotations`, not capability-tracked).

### 4.5 Inferences we get for free

- **`last_touched_by`** for any symbol — already in the graph
  metadata; `list_annotations(target={symbol: "fn foo"})` can prepend
  a "touched by @user in commit `abc123` 2 weeks ago" line per file
  with zero extra API calls
- **`repo_id`** for each file path — already canonicalized by
  `path_util::posix_string`; cross-repo refs go through federation
- **`agent_id`** is already the presence subsystem's identity — use
  it as `author` directly, no new identity work
- **Stale detection** — `list_annotations` can compare `target_id`
  against the live graph and return `status: stale` automatically
  for annotations pointing at symbols that no longer exist

### 4.6 Stale-status logic

When listing annotations, walk each one's target against the live
graph:
- `symbol` target → if no node in the graph, mark `stale`
- `edge` target → if either endpoint missing, mark `stale`
- `file` target → if `list_repos` doesn't include the repo, mark `stale`
- `repo` target → if `repo_id` not in `list_repos`, mark `stale`

Do this on read, not on write — keeps the table simple, and stale
detection is just `list_annotations` checking against the current
graph state.

### 4.7 CI-side enrichment

Update `.github/actions/lain-health-badge/health.sh` to:

1. After the existing blast-radius section, query `list_annotations`
   for each file in the PR and append a per-file "Open annotations"
   subsection if any exist (capped at 3 per file with a "more..." link
   to a `list_annotations` MCP call)
2. Add a top-of-comment line: `Capability readiness: N/N ready (Ts)`
   from `get_capabilities`
3. Compute the previous-run delta: `git diff <prev-sha>..HEAD --
   <file>` and call `explain_symbol` for each modified (not just new)
   function. The diff vs the new-symbol section is one extra
   `git log -1` + per-symbol MCP call.

### 4.8 Tests

- `tests/annotations_e2e.rs` (new): write → read → resolve → assert
  status transitions; assert stale detection (target a deleted
  symbol, then re-list)
- `tests/handoff_e2e.rs` (new): agent A registers → leaves note →
  unregisters; agent B registers → sees pending handoffs
- Existing `tests/mcp_cold_start.rs` still passes (annotations
  should not be on the cold-start path)

### 4.9 Acceptance

- All 5 new MCP tools in `docs/tool-schema.json` (regenerated via
  the existing `cargo run -- schema --update`)
- `explain_symbol` output includes `annotations: Some(...)` when any
  open annotation exists for the symbol
- Stale detection works: target a deleted symbol, `list_annotations`
  returns it with `status: stale`
- Handoff note flow: agent A leaves, agent B sees
- CI PR comment includes the new sections (per-file annotations,
  capability readiness line, previous-run delta)
- No regressions in `cargo test --workspace --all-targets`

---

## 5. Ordering — single PR or multiple?

**Recommendation: one PR, three commits.**

Commit 1: M4 step 8 + cancellation + spawn_blocking (the "M4
finishing" commit). Touches `src/server/{federation,ingest,mcp,tools}`
+ `src/server/watcher.rs` + new `src/server/ingest/blocking.rs`.
Reuses existing patterns; minimal new surface.

Commit 2: annotations storage + MCP tools (the "agent-side comment
layer" commit). New file `src/server/annotations.rs`, plus tool
defs in `src/server/mcp/definitions.rs` and a small handler block in
`src/server/mcp/handler.rs`. Plus `tests/annotations_e2e.rs` and
`tests/handoff_e2e.rs`. Touches the schema, so commit 3 must follow.

Commit 3: CI-side enrichment (`.github/actions/lain-health-badge/`).
Touches `health.sh` + the action's `inputs` block (the new
`comment-header` doesn't change; no new inputs needed for this
section).

Why one PR not three: the three sections share test infrastructure
(presence subsystem) and the schema regen ties commits 2 + 3
together.

---

## 6. Files the fresh instance will likely touch (for context)

```
src/server/annotations.rs                  (NEW)
src/server/ingest/blocking.rs             (NEW — or fold into existing module)
src/server/federation/repo_index.rs       (PerRepoReadiness + cancellation)
src/server/federation/federated_index.rs  (per_repo_readiness() snapshot)
src/server/federation/repo_source.rs      (cancellation plumbing)
src/server/federation/workspace.rs        (cancellation plumbing)
src/server/readiness.rs                   (PerRepoReadiness field on CapabilitySnapshot)
src/server/ingest/server.rs               (RunContext / CancellationToken field)
src/server/tools/registry.rs              (RunContext propagation)
src/server/mcp/handler.rs                 (gate + cancellation + annotation handlers)
src/server/mcp/definitions.rs             (5 new tool defs + auto-include fields)
src/server/tools/handlers/explain_symbol.rs  (annotations: Some(...) output field)
src/server/tools/handlers/blast_radius.rs (same)
src/server/watcher.rs                     (cancellation arm in select!)
src/server/tools/handlers/                (any handler that touches git2/filesystem)
tests/mcp_cold_start.rs                   (new federation-readiness test)
tests/annotations_e2e.rs                  (NEW)
tests/handoff_e2e.rs                      (NEW)
.github/actions/lain-health-badge/health.sh   (PR-comment enrichment)
.github/actions/lain-health-badge/action.yml  (only if new inputs needed)
docs/tool-schema.json                     (regenerated by `cargo run -- schema --update`)
docs/AGENT_UX_ROADMAP.md                  (update M4 status to ✅ Done + M8 status if any of this counts)
CHANGELOG.md                              ([Unreleased] ### Added entries for each tool, ### Changed for M4 closing)
```

---

## 7. What NOT to do (explicit non-goals, repeated from the roadmap)

- Do not serve stale graph queries while a replacement graph is being
  built (the readiness gate is the contract; `warming_up` is the
  answer)
- Do not dynamically add or remove tools from `tools/list` at runtime
- Do not queue and replay graph-tool calls made during warm-up
- Do not make optional semantic initialization a prerequisite for
  structural readiness
- Do not require clients to understand custom notifications
- Do not introduce a parallel readiness state model (use the
  existing `ReadinessRequirement` enum + `IndexLifecycleSnapshot`)
- Do not delete the old blocking-startup helper until every step's
  contract tests pass and the old code is unreachable in production
- Do not bump versions. The plan stays on `0.7.4-rc1`; release PR is
  separate

---

## 8. Verification before declaring done

```bash
# Build + lint + fmt
cargo build --workspace --all-targets
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings \
    -A clippy::style -A clippy::complexity -A clippy::perf \
    -A clippy::pedantic -A unused -A dead_code

# All tests
cargo test --workspace --all-targets

# Schema regen
cargo run -- schema --update
git diff docs/tool-schema.json   # should only show 5 new tools + per-repo + annotation fields

# New tests
cargo test --test annotations_e2e
cargo test --test handoff_e2e
cargo test --test mcp_cold_start

# M4 step 8 specifically — federation readiness
cargo test --test federation_readiness  # if you added this test
```

CI on the PR must pass:
- ✅ `lain/agent-contract` (status check)
- ✅ `Lint + format + doc tests`
- ✅ `Cargo Test (ubuntu-latest)`
- ✅ Tool schema matches `docs/tool-schema.json`
- ✅ `npm-shim install tests`
- (skipped on PR-to-dev: cross-platform tests, capability suite, coverage,
  version-drift, action-contracts)

---

## 9. Hand-off back to the user

When the fresh instance is done, the user expects:

1. PR open against `dev`, head named something like `feat/m4-step-8-and-annotations`
2. All CI green on the PR
3. The user merges with admin bypass (their workflow per
   `docs/BRANCHING.md`)
4. After merge, dev is "v0.7.4-rc1 + M4 closed + agent-side comment
   layer". The next release PR (separate scope) bumps to `v0.7.4`
   final or straight to `v0.8.0`, depending on user decision.

If the fresh instance hits blockers (e.g. a section proves too big
for one PR, or a test fails for reasons that look like a deeper bug),
the right move is to split that section into a follow-up PR and
land the rest, not to silently expand scope. The plan is sized to
land in one PR but every section is independently mergeable.
