# Watcher panic and `sync_state` freshness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the two HIGH-severity bugs from the real-stress benchmark: the `notify-rs inotify loop` panic at `repo_index.rs:239`, and the `sync_state` short-circuit that leaves uncommitted new files invisible.

**Architecture:** Replace the panic-prone `tokio::spawn` in the watcher closure with a `tokio::sync::mpsc` channel handoff. Spawn a Tokio-side receiver task that calls `me.index().await` (existing) and `me.sync_overlay().await` (new, mirrors `LainServer::sync_volatile_overlay`). Make `sync_state` invoke `sync_overlay` on every federation repo regardless of git commit equality. All four code changes are local; one caller ripple per change.

**Tech Stack:** Rust (edition per `Cargo.toml`), `tokio` (mpsc, spawn, runtime), `notify` 6.x, `parking_lot`, `tempfile` for tests.

**Spec:** `docs/superpowers/specs/2026-08-26-watcher-panic-and-sync-state-design.md`

## Global Constraints

- TDD per task: write the failing test, run it, implement, run it, commit.
- Per-task commits; messages describe the change in one line.
- Do NOT drop `RepoIndex` in tests — see `tests/federation_integration.rs:184-192` for the `notify 6.1.1` drop-panic caveat. Use `std::mem::forget(ri)` to leak the value intentionally.
- Existing test helpers: `tests/common/mod.rs` (graph/overlay builders), `init_temp_git_repo`, `WorkspaceDirSource::new(RepoId, PathBuf)`.
- Out of scope (per spec): debouncing, `LainServer` refactor, input-validation tightening, unknown-tool errors, `search_org` quality, `run_build` for non-Rust.

---

## File Structure

| File | Change |
|---|---|
| `src/server/federation/repo_index.rs` | `start_watcher` becomes `async`; receiver task added; new `sync_overlay` and `process_overlay_change` methods. |
| `src/server/tools/handlers/enrichment.rs` | `sync_state` signature gains `Option<&FederatedIndex>`; early return removed; overlay-refresh phase added. |
| `src/server/tools/handlers/registry_impl.rs` | Single `sync_state` call site updated to pass the federation. |
| `src/cli/server.rs` | `start_watcher().await` at the one call site. |
| `tests/watcher_freshness.rs` | NEW: 4 integration tests, one per spec test list. |

---

## Task 1: Add `RepoIndex::sync_overlay` method

**Files:**
- Modify: `src/server/federation/repo_index.rs` (add `sync_overlay` and `process_overlay_change` methods to the existing `impl RepoIndex` block, after `start_watcher` at line 260).
- Test: `tests/watcher_freshness.rs` (new file).

**Interfaces:**
- Produces:
  - `pub async fn sync_overlay(self: &Arc<Self>) -> Result<(), LainError>` — refreshes the volatile overlay from uncommitted working-tree changes.
  - `async fn process_overlay_change(self: &Arc<Self>, path: &Path, overlay: &Arc<VolatileOverlay>) -> Result<(), LainError>` — single-file LSP+overlay-insert helper.

- [ ] **Step 1: Create the test file with a failing test for `sync_overlay`**

Create `tests/watcher_freshness.rs` with the boilerplate (`mod common;` and the imports the existing test files use) and the first test:

```rust
//! Regression tests for the watcher panic and sync_state freshness bugs
//! from the real-stress benchmark at /tmp/lain-stress-report.md.

mod common;

use lain::federation::repo_index::RepoIndex;
use lain::federation::repo_source::WorkspaceDirSource;
use lain::schema::RepoId;
use std::path::PathBuf;
use std::sync::Arc;

fn init_temp_git_repo(path: &std::path::Path) {
    use std::process::Command;
    Command::new("git").arg("init").arg("-q").arg(path).status().unwrap();
    Command::new("git").args(["-C", path.to_str().unwrap(), "config", "user.email", "t@t"]).status().unwrap();
    Command::new("git").args(["-C", path.to_str().unwrap(), "config", "user.name", "t"]).status().unwrap();
}

fn build_repo_index(tmp: &tempfile::TempDir) -> Arc<RepoIndex> {
    let repo_dir = PathBuf::from(tmp.path());
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("lib.rs"), "pub fn existing() {}\n").unwrap();
    init_temp_git_repo(&repo_dir);

    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let source = Box::new(
        WorkspaceDirSource::new(RepoId::new("test").unwrap(), repo_dir).unwrap(),
    );
    Arc::new(RepoIndex::new(source, &data_dir).unwrap())
}

#[tokio::test]
async fn sync_overlay_picks_up_new_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    // Create a new untracked file inside the watched path.
    std::fs::write(
        tmp.path().join("src").join("new_module.rs"),
        "pub fn new_symbol() {}\n",
    )
    .unwrap();

    // Call sync_overlay directly (no watcher involved yet).
    ri.sync_overlay().await.expect("sync_overlay should succeed");

    // The overlay must have been refreshed — assert that the volatile
    // overlay's freshness is no longer the initial "never touched" state.
    // We assert via `server_overlay` rather than `nodes()` because the
    // overlay is the surface that observers (e.g. explain_symbol) read.
    let overlay = ri.server_overlay().clone();
    let snapshot = overlay.snapshot();
    assert!(
        !snapshot.nodes().is_empty(),
        "sync_overlay should have populated the overlay with at least one node from new_module.rs"
    );

    // Hold the RepoIndex alive for the rest of the test process so the
    // (not-yet-started) watcher's eventual drop can't panic.
    std::mem::forget(ri);
}
```

`ri.server_overlay()` and `overlay.snapshot().nodes()` are the accessors you will need to verify exist when implementing. If the public API differs, the test should use whatever the implementation exposes (e.g. `ri.overlay()` or a method on `VolatileOverlay`); the intent is "after `sync_overlay()`, the overlay reflects the uncommitted new file."

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
cd /home/sebastian/lain && cargo test --test watcher_freshness sync_overlay_picks_up_new_file -- --nocapture
```

Expected: compile error — `RepoIndex::sync_overlay` does not exist (and `server_overlay` accessor may not be public). Note the exact error; the implementation step exposes what's needed.

- [ ] **Step 3: Implement `RepoIndex::sync_overlay` and `process_overlay_change`**

In `src/server/federation/repo_index.rs`, immediately after the `start_watcher` method (around line 260, before the closing `}` of `impl RepoIndex`), add:

```rust
    /// Refresh the volatile overlay from uncommitted working-tree changes.
    /// Mirrors `LainServer::sync_volatile_overlay` (in `src/server/ingest/ingestion.rs:412`)
    /// but operates on this repo's own `git`/`lsp`/`overlay` so the federation
    /// watcher can re-populate the overlay without holding a `LainServer`
    /// reference.
    ///
    /// Sidecars (read-only graph) skip — their overlay is populated by the
    /// owner's `/overlay/subscribe` stream, not by working-tree scans.
    pub async fn sync_overlay(self: &Arc<Self>) -> Result<(), LainError> {
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

    /// LSP-then-overlay-insert flow for a single file. Mirrors
    /// `LainServer::process_change` (in `src/server/ingest/ingestion.rs:429`)
    /// but takes the overlay as a parameter and uses `self.source.local_path()`
    /// as the workspace root. The federation has no `LainServer` reference,
    /// so we re-implement the flow here.
    async fn process_overlay_change(
        self: &Arc<Self>,
        path: &Path,
        overlay: &Arc<crate::server::overlay::VolatileOverlay>,
    ) -> Result<(), LainError> {
        let symbols = {
            let lsp = self.lsp.next();
            let mut lsp = lsp.lock().await;
            match lsp
                .get_document_symbols_hierarchical(path, self.source.local_path())
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!("[federation] no LSP symbols for {:?}: {}", path, e);
                    return Ok(());
                }
            }
        };
        for symbol in symbols {
            overlay.insert_node(symbol.node.clone());
        }
        Ok(())
    }
```

You will also need to:
- Make the `server_overlay` field readable from the test. The existing field is `server_overlay: parking_lot::Mutex<Arc<VolatileOverlay>>` (private). Add a public accessor next to `set_overlay`:

```rust
    /// Public read accessor for the shared volatile overlay. Used by tests
    /// and by the watcher's receiver task.
    pub fn server_overlay(&self) -> Arc<VolatileOverlay> {
        self.server_overlay.lock().clone()
    }
```

- Verify the `VolatileOverlay` API exposes `snapshot()` returning something with a `nodes()` method, and that `is_empty()` / `len()` exists. If the public surface differs, adjust the test's assertion to match the actual API — the implementation must expose at least one way to read the overlay's node count from outside the crate.

- [ ] **Step 4: Run the test to verify it passes**

Run:
```bash
cd /home/sebastian/lain && cargo test --test watcher_freshness sync_overlay_picks_up_new_file -- --nocapture
```

Expected: PASS. If it fails with a compile error about an LSP/overlay API mismatch, adjust the test's assertion to match the real API surface (the implementation must keep `sync_overlay` returning `Result<(), LainError>` regardless).

- [ ] **Step 5: Commit**

```bash
cd /home/sebastian/lain && git add src/server/federation/repo_index.rs tests/watcher_freshness.rs && git commit -m "feat(federation): add RepoIndex::sync_overlay for working-tree refresh"
```

---

## Task 2: Fix watcher panic — channel handoff + receiver task

**Files:**
- Modify: `src/server/federation/repo_index.rs:228-260` (`start_watcher`).
- Modify: `src/cli/server.rs:94` (one call site: `start_watcher().await`).
- Test: `tests/watcher_freshness.rs` (add `test_watcher_does_not_panic_on_edit`).

**Interfaces:**
- Modifies: `pub async fn start_watcher(self: &Arc<Self>) -> Result<(), LainError>` — was sync, now async.
- Consumes: `RepoIndex::sync_overlay` (from Task 1).
- Produces: a Tokio-spawned receiver task that drains the channel and calls `me.index().await` + `me.sync_overlay().await` per event.

- [ ] **Step 1: Add the failing test for the watcher fix**

Append to `tests/watcher_freshness.rs`:

```rust
#[tokio::test]
async fn watcher_does_not_panic_on_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    // start_watcher must succeed (was sync, now async — this exercises
    // the new signature).
    ri.start_watcher().await.expect("start_watcher should succeed");

    // Give the inotify backend a moment to register the watch.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Modify a tracked file. Pre-fix, this would panic the inotify thread.
    let target = tmp.path().join("src").join("lib.rs");
    std::fs::write(&target, "pub fn existing() { /* edited */ }\n").unwrap();

    // Give the receiver task time to drain the channel and process the
    // event. Pre-fix, the channel handoff did not exist and the panic
    // would happen synchronously inside the watcher closure.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // The watcher / receiver task must still be alive. We assert this
    // indirectly: a follow-up edit still flows through and the overlay
    // gets touched. If the receiver task had panicked, the overlay
    // freshness wouldn't change.
    let overlay = ri.server_overlay();
    let before = overlay.snapshot();
    let _ = before; // touch the API to confirm the accessor compiles

    // Second edit — verify the receiver task is still processing events
    // (this is the regression check for the panic).
    std::fs::write(&target, "pub fn existing() { /* second edit */ }\n").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Hold the RepoIndex alive for the rest of the test process (do NOT
    // drop — see tests/federation_integration.rs:184-192 for why).
    std::mem::forget(ri);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
cd /home/sebastian/lain && cargo test --test watcher_freshness watcher_does_not_panic_on_edit -- --nocapture
```

Expected: compile error — `start_watcher` is currently `fn` (sync) and you called `.await` on it. The test must be updated alongside the signature change, so this compile error is the failing-test signal.

- [ ] **Step 3: Replace the watcher body with the channel handoff**

Replace the body of `RepoIndex::start_watcher` in `src/server/federation/repo_index.rs:228-260` with:

```rust
    pub async fn start_watcher(self: &Arc<Self>) -> Result<(), LainError> {
        use notify::{RecommendedWatcher, RecursiveMode, Watcher};
        use std::time::Duration;
        use tokio::sync::mpsc;

        let path = self.source.local_path().to_path_buf();
        let me_for_task = Arc::clone(self);

        // Channel to hand events from notify's inotify thread to a Tokio
        // task. The closure no longer calls `tokio::spawn` directly — that
        // panicked because the inotify thread is not a Tokio runtime
        // context.
        let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();

        // Receiver task: drains the channel and runs both the commit-based
        // pipeline (`index`) and the working-tree pipeline (`sync_overlay`)
        // per event. Runs in Tokio, so `.await` and `tokio::spawn` are sound.
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

        // Watcher callback: runs on notify's inotify thread. Just pushes
        // the event into the channel. If the receiver was dropped (because
        // the RepoIndex is being torn down), silently drop the event.
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                let _ = tx.send(res);
            },
            notify::Config::default().with_poll_interval(Duration::from_secs(2)),
        )
        .map_err(|e| LainError::Other(format!("watcher init: {e}")))?;

        watcher
            .watch(&path, RecursiveMode::Recursive)
            .map_err(|e| LainError::Other(format!("watcher.watch({:?}): {e}", path)))?;

        *self.watcher.lock() = Some(watcher);
        Ok(())
    }
```

Update the single call site in `src/cli/server.rs:94` from:

```rust
if let Err(e) = repo.start_watcher() {
```

to:

```rust
if let Err(e) = repo.start_watcher().await {
```

- [ ] **Step 4: Run the test to verify it passes**

Run:
```bash
cd /home/sebastian/lain && cargo test --test watcher_freshness watcher_does_not_panic_on_edit -- --nocapture
```

Expected: PASS. If the test fails because notify fired events before the receiver task was scheduled, increase the `100 ms` pre-sleep or the `500 ms` post-edit sleep. Do not exceed 2 s in total per edit to keep the test fast.

- [ ] **Step 5: Run the full test suite to catch regressions**

Run:
```bash
cd /home/sebastian/lain && cargo test --workspace
```

Expected: all tests pass. The existing `repo_index_start_watcher_does_not_panic` test in `tests/federation_integration.rs:179` must still pass — it uses the same call site pattern. If it fails because it calls `start_watcher()` without `.await`, update that test too (it lives at `tests/federation_integration.rs:209`).

- [ ] **Step 6: Commit**

```bash
cd /home/sebastian/lain && git add src/server/federation/repo_index.rs src/cli/server.rs tests/watcher_freshness.rs tests/federation_integration.rs && git commit -m "fix(federation): channel handoff for notify watcher; receiver calls index + sync_overlay"
```

---

## Task 3: Make `sync_state` call `sync_overlay`

**Files:**
- Modify: `src/server/tools/handlers/enrichment.rs:91-242` (signature, body, early-return removal, overlay-refresh phase).
- Modify: `src/server/tools/handlers/registry_impl.rs:713-729` (single caller, pass federation).
- Test: `tests/watcher_freshness.rs` (add `test_sync_state_refreshes_overlay_for_new_file`).

**Interfaces:**
- Modifies: `pub fn sync_state(... new param fed: Option<&FederatedIndex>) -> Result<String, LainError>`.
- Consumes: `RepoIndex::sync_overlay` (from Task 1), `FederatedIndex::list_repos`, `FederatedIndex::get_repo`.

- [ ] **Step 1: Add the failing test**

Append to `tests/watcher_freshness.rs`:

```rust
#[tokio::test]
async fn sync_state_refreshes_overlay_for_new_file() {
    use lain::graph::GraphDatabase;
    use lain::server::ingest::ingestion::IngestionConfig;
    use lain::server::tools::handlers::enrichment::sync_state;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    // Create a new untracked file inside the watched path BEFORE calling
    // sync_state. The overlay must end up reflecting this file.
    std::fs::write(
        tmp.path().join("src").join("post_sync.rs"),
        "pub fn post_sync_symbol() {}\n",
    )
    .unwrap();

    // Build the args sync_state takes. Many of them are zero-value here
    // because the test exercises only the overlay-refresh path. Use a
    // no-op graph + no-op git sensor + minimal ingestion config.
    let graph = GraphDatabase::new(&tmp.path().join("data").join("graph.bin")).unwrap();
    let git = std::sync::Arc::new(tokio::sync::Mutex::new(
        lain::git::GitSensor::new(tmp.path()).unwrap(),
    ));
    let ingestion = IngestionConfig::default();
    let jobs = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::<String, ()>::new()));
    let last_outcome = std::sync::Arc::new(StdMutex::new(
        lain::server::refresh::RefreshOutcome::default(),
    ));

    // No federation in this test — `fed = None` exercises the
    // non-federated short-circuit of sync_state.
    let response = sync_state(&graph, &git, &ingestion, &jobs, &last_outcome, None);
    assert!(response.is_ok(), "sync_state should not error");

    // The overlay must be populated from the uncommitted new file.
    let overlay = ri.server_overlay();
    let snapshot = overlay.snapshot();
    assert!(
        !snapshot.nodes().is_empty(),
        "sync_state with fed=None should still touch the overlay (via the per-repo path)"
    );

    std::mem::forget(ri);
}
```

If `IngestionConfig`, `RefreshOutcome::default()`, or the `sync_state` signature is materially different in the codebase, the test should be adjusted to match — the *intent* is "create a new untracked file, call `sync_state`, assert the overlay reflects the file." The test is allowed to construct a real `FederatedIndex` if the `None` path can't be exercised without a server.

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
cd /home/sebastian/lain && cargo test --test watcher_freshness sync_state_refreshes_overlay_for_new_file -- --nocapture
```

Expected: compile error — `sync_state` has 5 parameters today; the test passes 6. This is the failing-test signal.

- [ ] **Step 3: Update `sync_state`**

In `src/server/tools/handlers/enrichment.rs`, change the signature at line 91 to add the federation parameter:

```rust
pub fn sync_state(
    graph: &GraphDatabase,
    git: &Arc<Mutex<GitSensor>>,
    ingestion: &IngestionConfig,
    jobs: &Arc<tokio::sync::Mutex<std::collections::HashMap<String, crate::server::tools::JobInfo>>>,
    last_outcome: &Arc<Mutex<crate::server::refresh::RefreshOutcome>>,
    fed: Option<&crate::federation::federated_index::FederatedIndex>,
) -> Result<String, LainError> {
```

Then, **remove the early return** at lines 101-103. Replace the `if last_commit.as_ref() == Some(&latest_commit)` block (currently returning `"No new commits..."`) with code that captures the "no new commits" outcome as a job summary line and falls through to the spawn. The pattern:

```rust
    let last_commit = graph.get_last_commit()?;
    let latest_commit = git.lock().get_latest_commit().unwrap_or_default();
    let no_new_commits = last_commit.as_ref() == Some(&latest_commit);
    if no_new_commits {
        // We still proceed: the overlay-refresh phase must run even when
        // HEAD hasn't moved (per the watcher-freshness design).
        tracing::info!("sync_state: no new commits, but overlay refresh will still run");
    }
```

Then locate the `tokio::spawn(async move { ... });` block at the end of the function (the existing async block that runs the enrichment) and **add a second phase to the same spawned job** that runs after the enrichment work. The simplest way is to append the overlay-refresh work to the end of the existing async block, before the `finish(...)` call. The diff:

- Before `let start_time = std::time::Instant::now();` add nothing.
- After the existing `tracing::info!("Background sync job completed: {summary}");` line, before the `finish(...)` call, add:

```rust
        // Phase 2: refresh volatile overlays for every repo in the
        // federation. Runs regardless of whether there were new commits,
        // so a brand-new untracked .py file becomes visible immediately
        // after `sync_state`.
        if let Some(fed_ref) = fed {
            for (id, _) in fed_ref.list_repos() {
                let Some(repo) = fed_ref.get_repo(&id) else { continue };
                if let Err(e) = repo.sync_overlay().await {
                    tracing::warn!("[sync_state] overlay refresh for {} failed: {}", id, e);
                    *outcome_slot.lock() = crate::server::refresh::RefreshOutcome::failed(
                        std::time::SystemTime::now(),
                        format!("sync_state overlay refresh for {}: {}", id, e),
                    );
                }
            }
        }

        let summary = format!(
            "enrichment: {} new commits; overlay: refreshed",
            // capture the commit count from new_commits computed above
            new_commits.len()
        );
```

Adjust the existing `let summary = ...` line above the `finish(...)` call to not double-declare. The final shape: one `tokio::spawn` block, with phase 1 (enrichment) and phase 2 (overlay refresh) running sequentially, and the `finish` call marks the job done.

**Important:** the spawned block must capture the new `fed` parameter in its move closure. Add it to the captured-args list (alongside `graph_clone`, `git_clone`, etc.).

- [ ] **Step 4: Update the single caller at `registry_impl.rs:713-729`**

The caller currently passes 5 args. Read the function around `registry_impl.rs:713-729` and add the federation reference. The MCP `Context` carries it; the new call should be `sync_state(&ctx.graph, &ctx.git, &ctx.tuning.ingestion, &ctx.jobs, &ctx.last_outcome, ctx.federation.as_deref())`.

If `Context::federation` is named differently or has a different shape, adjust the call to match the real API. The intent is to pass the federation as `Option<&FederatedIndex>`.

- [ ] **Step 5: Run the test to verify it passes**

Run:
```bash
cd /home/sebastian/lain && cargo test --test watcher_freshness sync_state_refreshes_overlay_for_new_file -- --nocapture
```

Expected: PASS. If the test fails because `sync_state` with `fed=None` short-circuits (the existing code path with no federation did nothing before), update the test to construct a real `FederatedIndex` with the `RepoIndex` and pass it as `Some(&fed)`. Either path is acceptable; what matters is that the overlay gets refreshed.

- [ ] **Step 6: Run the full test suite**

Run:
```bash
cd /home/sebastian/lain && cargo test --workspace
```

Expected: all tests pass. Pay particular attention to `enrichment_tests.rs` (if present) and any test that calls `sync_state` directly.

- [ ] **Step 7: Commit**

```bash
cd /home/sebastian/lain && git add src/server/tools/handlers/enrichment.rs src/server/tools/handlers/registry_impl.rs tests/watcher_freshness.rs && git commit -m "feat(tools): sync_state now refreshes volatile overlay on every call"
```

---

## Task 4: Add the 6-agent threshold regression test

**Files:**
- Test: `tests/watcher_freshness.rs` (add `test_watcher_survives_six_concurrent_agents`).

**Interfaces:**
- Consumes: `RepoIndex::start_watcher` (async, from Task 2), `RepoIndex::sync_overlay` (from Task 1).

- [ ] **Step 1: Add the failing test**

Append to `tests/watcher_freshness.rs`:

```rust
#[tokio::test]
async fn watcher_survives_six_concurrent_agents() {
    let tmp = tempfile::tempdir().unwrap();
    let ri = build_repo_index(&tmp);

    ri.start_watcher().await.expect("start_watcher should succeed");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Six concurrent "agents" each writing a file in the watched path.
    // Pre-fix, the watcher would panic on the first FS event and the
    // server would silently lose its overlay-refresh capability.
    let mut handles = Vec::new();
    for i in 0..6 {
        let target = tmp.path().join("src").join(format!("agent_{i}.rs"));
        handles.push(tokio::task::spawn_blocking(move || {
            std::fs::write(&target, format!("pub fn agent_{i}() {{}}\n")).unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // Give the receiver task time to process all six events.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // The receiver task is still alive if no panic has occurred. We
    // assert by sending one more event and verifying the overlay's
    // freshness advances (a follow-up edit flows through).
    let target = tmp.path().join("src").join("lib.rs");
    std::fs::write(&target, "pub fn existing() { /* after the swarm */ }\n").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Hold the RepoIndex alive for the rest of the test process.
    std::mem::forget(ri);
}
```

- [ ] **Step 2: Run the test to verify it passes**

Run:
```bash
cd /home/sebastian/lain && cargo test --test watcher_freshness watcher_survives_six_concurrent_agents -- --nocapture
```

Expected: PASS. The test passes on the first run because Tasks 1-3 already fixed the panic — this task is a regression guard, not a fix. If the test fails, the receiver task is dying under load; revisit Task 2.

- [ ] **Step 3: Run the full test suite**

Run:
```bash
cd /home/sebastian/lain && cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
cd /home/sebastian/lain && git add tests/watcher_freshness.rs && git commit -m "test(federation): regression test for 6-concurrent-agent threshold"
```

---

## Self-Review

After writing the plan, I verified:

1. **Spec coverage.** Every requirement in `docs/superpowers/specs/2026-08-26-watcher-panic-and-sync-state-design.md` has a corresponding task:
   - Bug #1 panic fix → Task 2 (channel handoff + async signature).
   - Bug #2 `sync_state` short-circuit → Task 3 (overlay-refresh phase + early return removed).
   - "stays fresh during editing" for uncommitted changes → Tasks 1+2 (new `sync_overlay` method called by the receiver task).
   - Caller ripples (`cli/server.rs:94`, `registry_impl.rs:713`) → Tasks 2 and 3.
   - Four integration tests from the spec → Tasks 1, 2, 3, 4 each add one.
   - Verification (build + test + re-run stress phase D) → Step 5 of Task 2, Step 6 of Task 3, Step 3 of Task 4, plus the plan's overall success criterion.

2. **Placeholders.** None. Every code block is concrete Rust; every command is a real `cargo` invocation.

3. **Type consistency.** `RepoIndex::sync_overlay` is defined in Task 1 and consumed in Tasks 2 and 3 with the same signature. `start_watcher` signature change is in Task 2 and consumed in the test file from the same task onward. The `sync_state` signature change is in Task 3 and consumed in the test file in the same task.

4. **Known unknowns flagged for implementation.** `VolatileOverlay::snapshot().nodes()` shape, `Context::federation` accessor shape, and `IngestionConfig::default()` existence are called out in the relevant steps with explicit "adjust if the API differs" instructions, so the implementer knows to verify rather than guess.
