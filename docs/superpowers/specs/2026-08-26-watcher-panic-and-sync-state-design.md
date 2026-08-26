# Fix watcher panic and `sync_state` freshness

**Date:** 2026-08-26
**Scope:** `src/server/federation/repo_index.rs`, `src/server/tools/handlers/enrichment.rs`, `src/cli/server.rs`, plus tests.
**Goal:** repair the two bugs surfaced by the real-stress benchmark so the README promises "stays fresh during editing" and "multi-agent up to 6+ agents" hold against a real Python codebase with concurrent edits.

---

## Background

The real-stress benchmark at `/tmp/lain-stress-report.md` found two HIGH-severity bugs:

1. **`notify-rs inotify loop` panics** at `src/server/federation/repo_index.rs:239:21` on the first FS event in the watched repo. The closure calls `tokio::spawn` from a non-Tokio thread. The watcher thread dies, the volatile overlay never updates for the rest of the server's lifetime, and the HTTP frontend keeps returning 200 while the watcher is silently dead. This breaks "stays fresh during editing" and crashes multiplayer at 6+ agents (where state-file writes from concurrent agents fan out into the watched path).

2. **`sync_state` never refreshes the working tree.** `src/server/tools/handlers/enrichment.rs:91` short-circuits on git commit equality: if HEAD hasn't moved, it returns `"No new commits. State is already up to date."` and never touches the volatile overlay. The function that *does* walk uncommitted changes (`LainServer::sync_volatile_overlay`, `src/server/ingest/ingestion.rs:412`) is never called from the `sync_state` MCP tool. New uncommitted `.py` files therefore never become visible through `explain_symbol` / `find_dead_code` / `find_anchors`, even after a manual `sync_state` or `request_reload`.

A second observation: the watcher's existing call to `me.index().await` runs the commit-based pipeline (`index_one_repo`), which reads the git tree and never refreshes the volatile overlay for uncommitted edits. So fixing only the panic would not deliver "stays fresh during editing" for uncommitted changes — the receiver must also refresh the overlay.

---

## Design

### Change 1 — `RepoIndex::start_watcher` becomes async, uses a channel handoff, and refreshes both the static graph and the volatile overlay

**File:** `src/server/federation/repo_index.rs`

**Signature change:**

```rust
// before
pub fn start_watcher(self: &Arc<Self>) -> Result<(), LainError> { … }

// after
pub async fn start_watcher(self: &Arc<Self>) -> Result<(), LainError> { … }
```

**Body (shape, not full code):**

```rust
let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Result<notify::Event>>();

let me_for_task = Arc::clone(self);
tokio::spawn(async move {
    while let Some(res) = rx.recv().await {
        if res.is_ok() {
            if let Err(e) = me_for_task.index().await {
                tracing::debug!(
                    "[federation] watcher-triggered index failed for {:?}: {}",
                    me_for_task.source.local_path(),
                    e
                );
            }
            if let Err(e) = me_for_task.sync_overlay().await {
                tracing::debug!(
                    "[federation] watcher-triggered overlay refresh failed for {:?}: {}",
                    me_for_task.source.local_path(),
                    e
                );
            }
        }
    }
});

let mut watcher = RecommendedWatcher::new(
    move |res: notify::Result<notify::Event>| {
        // Receiver is held by the spawned task. If it was dropped, the
        // RepoIndex is being torn down — silently drop the event.
        let _ = tx.send(res);
    },
    notify::Config::default().with_poll_interval(Duration::from_secs(2)),
)?;
watcher.watch(&path, RecursiveMode::Recursive)?;
*self.watcher.lock() = Some(watcher);
Ok(())
```

**Why this fixes Bug #1:**
- The closure no longer calls `tokio::spawn`. It just pushes the event into a channel.
- The receiver task is spawned from `start_watcher`, which now runs in a Tokio context, so `tokio::spawn` is sound there.
- The receiver drains events on a Tokio worker, so `me.index().await` and `me.sync_overlay().await` run on Tokio and can hold `.await` points without panicking.

**Why this also delivers "stays fresh during editing" for uncommitted changes:**
- `me.index().await` re-runs the commit-based pipeline (existing behavior, kept).
- `me.sync_overlay().await` re-populates the volatile overlay from uncommitted changes via LSP (new behavior). This is the missing piece that made the promise half-true before.

**Channel choice:** unbounded is fine for now. The receiver runs both calls per event; a storm of FS events will queue briefly but not OOM. Debouncing is a follow-up.

**Caller ripple:** exactly one caller at `src/cli/server.rs:94`:

```rust
// before
if let Err(e) = repo.start_watcher() {

// after
if let Err(e) = repo.start_watcher().await {
```

The call site is already inside an async function (the main server setup loop in `cli/server.rs`), so `.await` is well-formed.

### Change 2 — New `RepoIndex::sync_overlay` method

**File:** `src/server/federation/repo_index.rs`

Mirrors the working-tree path of `LainServer::sync_volatile_overlay` (`src/server/ingest/ingestion.rs:412`), but scoped to a single `RepoIndex`'s `git` / `lsp` / `overlay` so the federation case has its own self-contained refresh.

**Shape:**

```rust
impl RepoIndex {
    /// Refresh the volatile overlay from uncommitted working-tree changes.
    /// Mirrors `LainServer::sync_volatile_overlay` but operates on this
    /// repo's own `git`/`lsp`/`overlay` so the federation watcher can
    /// re-populate the overlay without holding a reference to `LainServer`.
    pub async fn sync_overlay(self: &Arc<Self>) -> Result<(), LainError> {
        // Sidecars (read-only graph) skip: their overlay is populated
        // by the owner's /overlay/subscribe stream, not by working-tree scans.
        if self.db.is_read_only() {
            return Ok(());
        }
        let overlay = self.server_overlay.lock().clone();
        overlay.clear();

        let changes = self.git.lock().await.get_uncommitted_changes()?;

        for change in &changes {
            if let Err(e) = self.process_overlay_change(&change.path, &overlay).await {
                tracing::warn!(
                    "[federation] overlay refresh: failed for {:?}: {}",
                    change.path,
                    e
                );
            }
        }
        Ok(())
    }

    async fn process_overlay_change(
        self: &Arc<Self>,
        path: &Path,
        overlay: &Arc<VolatileOverlay>,
    ) -> Result<(), LainError> {
        // Same LSP-then-overlay-insert flow as `LainServer::process_change`
        // in `src/server/ingest/ingestion.rs:429`. We re-implement it here
        // (rather than calling `LainServer::process_change`) because the
        // federation has no `LainServer` reference, only `RepoIndex`.
        let symbols = {
            let lsp = self.lsp.next();
            let mut lsp = lsp.lock().await;
            match lsp.get_document_symbols_hierarchical(path, /* workspace */ &self.source.local_path()).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(
                        "[federation] no LSP symbols for {:?}: {}",
                        path, e
                    );
                    return Ok(());
                }
            }
        };
        for symbol in symbols {
            overlay.insert_node(symbol.node.clone());
        }
        Ok(())
    }
}
```

**Why a new method (not reusing `LainServer::sync_volatile_overlay`):**
- The federation has no `LainServer`; only `RepoIndex`. Pulling the logic onto `RepoIndex` keeps the federation case self-contained.
- The `LainServer` version is left untouched. The single-workspace (non-federated) MCP path still calls `LainServer::sync_volatile_overlay` from the existing call sites.
- Both paths share the same LSP-then-overlay-insert flow; only the *call surface* is duplicated.

**LSP API note (resolved at spec time):** `LspPool::get_document_symbols_hierarchical(&mut self, path: &Path, workspace: &Path)` takes two `&Path` arguments — the file path and the workspace root. For `RepoIndex` the workspace root is `self.source.local_path()`. The existing call from `LainServer::process_change` uses `self.config.workspace` (the configured workspace), which is the same path. The implementation passes `self.source.local_path()` directly; no new struct or wrapper is needed.

### Change 3 — `sync_state` MCP tool also refreshes the volatile overlay

**File:** `src/server/tools/handlers/enrichment.rs`

After the existing commit-keyed enrichment runs (and on the no-new-commits short-circuit path too), refresh the volatile overlay for every repo in the federation.

**Shape:**

```rust
pub fn sync_state(
    graph: &GraphDatabase,
    git: &Arc<Mutex<GitSensor>>,
    ingestion: &IngestionConfig,
    jobs: &Arc<…>,
    last_outcome: &Arc<Mutex<…>>,
    fed: Option<&FederatedIndex>,        // NEW
) -> Result<String, LainError> {
    // ... existing commit check + spawn ...

    // NEW: also refresh volatile overlays for every repo in the federation.
    // The overlay holds symbols for uncommitted working-tree changes; this
    // is what makes a brand-new untracked .py file visible to
    // explain_symbol / find_dead_code / find_anchors immediately.
    let fed_ref = fed;
    let last_outcome_clone = Arc::clone(last_outcome);
    let job_id_for_overlay = job_id.clone();
    let jobs_for_overlay = Arc::clone(jobs);

    tokio::spawn(async move {
        if let Some(fed) = fed_ref {
            for (id, _) in fed.list_repos() {
                let Some(repo) = fed.get_repo(&id) else { continue };
                if let Err(e) = repo.sync_overlay().await {
                    tracing::warn!("[sync_state] overlay refresh for {} failed: {}", id, e);
                    *last_outcome_clone.lock() = crate::server::refresh::RefreshOutcome::failed(
                        SystemTime::now(),
                        format!("sync_state overlay refresh for {}: {}", id, e),
                    );
                }
            }
        }
        // mark the job complete only after both phases finish
    });
}
```

**Important: ordering and short-circuit.**
- The commit-based enrichment must still run (it's what updates the static graph on new commits).
- The early return at `enrichment.rs:101-103` (`if last_commit == latest_commit { return "No new commits..." }`) is **removed**; the function must fall through to the overlay-refresh phase. The "no new commits" outcome is reported as a job summary line, not a function return.
- The overlay refresh runs **regardless of commit equality** — both when there are new commits and when there aren't.
- Both phases run inside the same background job; the existing `get_job_status` / `get_health` plumbing already handles "still running" vs "failed" — the new code reuses it. The final job summary reports both phases' outcomes (e.g. "enrichment: 3 new commits; overlay: refreshed 2 repos").

**Federation reference plumbing.** `sync_state`'s signature gains an `Option<&FederatedIndex>` parameter. The single caller is the registry in `src/server/tools/handlers/registry_impl.rs:713`. The MCP `Context` struct carries the federation as `Option<Arc<FederatedIndex>>`; the registry passes `ctx.federation.as_deref()`.

**Federation iteration API (resolved at spec time):** `FederatedIndex::list_repos() -> Vec<(RepoId, RepoHealth)>` returns IDs (with health metadata, which we ignore). The `Arc<RepoIndex>` for each ID is fetched via `FederatedIndex::get_repo(&RepoId) -> Option<Arc<RepoIndex>>`. The spawned job iterates `list_repos`, `filter_map`s with `get_repo`, and runs `sync_overlay().await` on each `Arc<RepoIndex>`.

**Single-workspace (non-federated) case.** A test or a future single-repo path may not have a `FederatedIndex`. We make the new parameter `Option<&FederatedIndex>` and skip the overlay-refresh phase when `None`. The `MCP Context` carries the federation as `Option<Arc<FederatedIndex>>`; the registry passes `ctx.federation.as_deref()` — the implementation step will verify the exact `Option`-shaped accessor.

### Change 4 — Tests

Four integration tests live in `tests/`, exercising the two real-bug scenarios from the stress report:

1. **`test_watcher_does_not_panic_on_edit`** — Start a real `RepoIndex`, create a watcher, modify a tracked file in the watched path, wait 200 ms, assert no panic in the server log and the receiver task is still alive (e.g. via `tokio::task::yield_now()` until the next event is processed).
2. **`test_watcher_picks_up_new_file`** — Create a new untracked `.py` file in the watched path, wait for the watcher's overlay refresh, assert `repo.nodes()` (or the volatile overlay's nodes) contains a symbol from the new file.
3. **`test_sync_state_refreshes_overlay_for_new_file`** — Create a new untracked `.py` file, call `sync_state`, poll `explain_symbol` until the new symbol is visible. Use a short timeout (e.g. 2 s) so the test fails fast.
4. **`test_watcher_survives_six_concurrent_agents`** — Run six concurrent agents each writing to a file in the watched path. Assert the watcher still fires (a counter incremented in the receiver task reaches ≥1) and no panic. This is the regression test for the threshold bug.

Tests use the existing test harness from `tests/common/`. No new framework dependencies.

---

## Error handling

- **Channel send failure (closure side):** ignored. The receiver task is held by the spawned future; the only way the receiver is gone is if `RepoIndex` was dropped, in which case dropping events is correct.
- **Receiver task failure:** logged at `tracing::debug!` for `index` and `tracing::warn!` for `sync_overlay`. The receiver task continues to the next event.
- **Watcher init failure:** propagated as `LainError::Other` from `start_watcher`, logged at the call site (`cli/server.rs:94`), server continues with the repo in its current state (degraded). This matches the existing behavior — the call site already logs and demotes.
- **`sync_state` overlay refresh failure per repo:** logged and recorded on `last_outcome`. Other repos' refreshes still proceed. The job is marked completed with the per-repo error in the summary; `get_job_status` returns the failure for inspection.

## Out of scope (explicit non-goals)

- **Debouncing / coalescing FS events.** A noisy editor (write+close+rename) will fire several events and trigger several re-index/refresh cycles. That's wasteful but not wrong. We will note it as a follow-up.
- **Refactoring `LainServer::sync_volatile_overlay` to share code with `RepoIndex::sync_overlay`.** The two functions are kept parallel; a later cleanup can extract a free function `refresh_overlay_from_uncommitted(git, lsp, overlay, workspace)`. Not part of this fix.
- **Tightening input validation** for `find_anchors({"limit":"abc"})` etc. — these are real findings from the stress report but a separate spec.
- **Rejecting unknown tool names with a proper error** — separate spec.
- **Federation-wide `search_org` quality** — separate spec.
- **`run_build` for non-Rust projects** — separate spec.

## Files touched

- `src/server/federation/repo_index.rs` — `start_watcher` becomes async; new `sync_overlay` and `process_overlay_change` methods; receiver task added.
- `src/server/tools/handlers/enrichment.rs` — `sync_state` signature gains a federation ref; overlay-refresh phase added to the spawned job.
- `src/server/tools/handlers/registry_impl.rs` — single `sync_state` call site updated to pass the federation.
- `src/cli/server.rs:94` — `start_watcher().await`.
- `tests/watcher_freshness.rs` (new) — four integration tests.
- `docs/superpowers/specs/2026-08-26-watcher-panic-and-sync-state-design.md` — this spec.

## Verification

The spec is complete when:
- `cargo build --workspace` succeeds.
- `cargo test --workspace` passes, including the four new tests.
- A re-run of the stress benchmark's phase D ("edit-during-query") shows new untracked `.py` files becoming visible through `explain_symbol` within 3 seconds, and no panic in the server log.
- A re-run of phase A's multi-agent race at 6 agents no longer panics; latency stays in the previous range (19-46 ms).
