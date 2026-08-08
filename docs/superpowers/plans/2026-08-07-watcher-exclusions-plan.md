# Watcher Exclusions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep Lain’s live watcher running around unreadable directories while honoring `.gitignore` and preserving updates from readable source directories.

**Architecture:** Replace the one recursive `notify` registration with Git-ignore-aware directory discovery and one non-recursive watch per readable directory. A watcher-thread control channel will register newly created directories; individual discovery or registration failures will be logged and skipped without dropping the file-event sender.

**Tech Stack:** Rust 2021, `notify = 6`, `ignore = 0.4`, `git2 = 0.19`, Tokio MPSC, `tempfile`, existing `GitSensor` and `parking_lot::Mutex`.

## Global Constraints

- Use the repository’s `.gitignore` rules as the watcher exclusion source; do not introduce `.lainignore`.
- A directory-level `EACCES` or other discovery/watch error must not terminate the watcher thread or event processor.
- Keep the existing 100 ms debounce, batch size of 20, LSP extraction, and volatile-overlay update behavior.
- Do not change directory ownership or permissions.
- Keep explicit workspace selection through `lain-server-manager.sh` unchanged.
- Do not add dependencies; `ignore`, `notify`, `git2`, and `tempfile` already exist in `Cargo.toml`.
- Do not run `git commit` unless the user explicitly authorizes it.

---

## File Map

- **Modify:** `src/watcher.rs`
  - Add Git-ignore-aware directory discovery.
  - Add per-directory non-recursive registration.
  - Add watcher-thread commands for newly created directories.
  - Log notify and per-directory failures without disconnecting the processor.
  - Add focused unit/integration tests beside private watcher helpers.
- **Modify:** `docs/TECHNICAL.md:255-262`
  - Document that live watching respects `.gitignore`, watches readable directories independently, and skips unreadable directories.
- **Create:** `docs/superpowers/specs/2026-08-07-watcher-exclusions-design.md`
  - Approved design record; already written and reviewed.
- **Create:** `docs/superpowers/plans/2026-08-07-watcher-exclusions-plan.md`
  - This implementation plan.

---

### Task 1: Add failing tests for permission-tolerant directory discovery

**Files:**
- Modify: `src/watcher.rs` (new `#[cfg(test)] mod tests`)
- Test: `src/watcher.rs` private-helper tests

**Interfaces:**
- Consumes: the planned `discover_watch_directories(workspace: &Path) -> Vec<PathBuf>` helper.
- Produces: regression tests proving Git-ignored and unreadable directories do not prevent readable siblings from being discovered.

- [ ] **Step 1: Add a Unix-gated fixture test for `.gitignore` and `EACCES`**

Create a temporary Git repository with this layout:

```text
repo/
  .gitignore       # contains `ignored/`
  visible/
    source.rs
  ignored/
    ignored.rs
  blocked/
    blocked.rs     # chmod 000 on Unix
```

Initialize the repository with `git2::Repository::init`, write `.gitignore`, create the directories, and set `blocked` to mode `0o000`. The test should call `discover_watch_directories(&repo)` and assert:

```rust
assert!(watched.contains(&repo.to_path_buf()));
assert!(watched.iter().any(|p| p.ends_with("visible")));
assert!(!watched.iter().any(|p| p.ends_with("ignored")));
assert!(!watched.iter().any(|p| p.ends_with("blocked")));
```

Restore `blocked` to `0o755` before the temporary directory is dropped so cleanup remains reliable.

- [ ] **Step 2: Run the focused test and verify it fails for the missing helper**

Run:

```bash
cargo test watcher::tests::directory_discovery_skips_ignored_and_inaccessible -- --nocapture
```

Expected: FAIL because `discover_watch_directories` does not exist yet.

- [ ] **Step 3: Add a focused event-eligibility test for Git-ignored source files**

Create a temporary Git repository with `.gitignore` containing `ignored/`, create `visible/source.rs` and `ignored/source.rs`, construct `notify::Event` values for both paths, and assert that the planned `filter_event(event, git)` returns the visible path and returns `None` for the ignored path.

- [ ] **Step 4: Run the event-filter test and verify it fails**

Run:

```bash
cargo test watcher::tests::ignored_source_events_are_filtered -- --nocapture
```

Expected: FAIL because the new Git-aware filter signature and behavior are not implemented.

---

### Task 2: Implement Git-ignore-aware discovery and resilient registration

**Files:**
- Modify: `src/watcher.rs` near imports and `FileWatcher::start`
- Test: `src/watcher.rs` tests from Task 1

**Interfaces:**
- Consumes: `ignore::WalkBuilder`, `LainServer::git`, and the existing `sender`/`receiver` channels.
- Produces:
  - `fn discover_watch_directories(workspace: &Path) -> Vec<PathBuf>`
  - `fn register_directory(watcher: &mut RecommendedWatcher, watched: &mut HashSet<PathBuf>, directory: PathBuf)`
  - `fn filter_event(event: &Event, git: &Arc<parking_lot::Mutex<GitSensor>>) -> Option<PathBuf>`

- [ ] **Step 1: Implement directory discovery using the existing `ignore` crate**

Use an `ignore::WalkBuilder` configured with Git ignore and hidden-entry filtering:

```rust
let walker = ignore::WalkBuilder::new(workspace)
    .hidden(true)
    .git_ignore(true)
    .git_exclude(true)
    .git_global(true)
    .build();
```

Iterate every `Result<DirEntry, ignore::Error>`:

- add only directories for which `std::fs::read_dir(path)` succeeds;
- ignore non-directory entries;
- log an error entry’s path and continue when the walker yields an error;
- when the explicit readability probe fails, log the directory and continue without adding it.

Add a helper with this exact behavior:

```rust
fn is_readable_directory(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(entries) => {
            drop(entries);
            true
        }
        Err(error) => {
            warn!("FileWatcher: skipping directory {:?}: {}", path, error);
            false
        }
    }
}
```

Do not call `.flatten()`, because that would hide the permission failure that must be diagnosed.

- [ ] **Step 2: Implement non-recursive registration with deduplication**

Track registered directories in a `HashSet<PathBuf>`. For each new path:

1. Return immediately if it is already in the set.
2. Call `watcher.watch(&directory, RecursiveMode::NonRecursive)`.
3. On success, retain it in the set.
4. On failure, remove it from the set and emit a warning containing the directory and original `notify::Error`.

The helper must not return an error that terminates the watcher thread.

- [ ] **Step 3: Update the file filter to consult Git ignore**

Keep the existing event-kind, hidden-component, regular-file, and source-extension checks. For candidate files, call `GitSensor::is_ignored` through the shared `Arc<parking_lot::Mutex<GitSensor>>`:

- `Ok(true)` means return `None`;
- `Ok(false)` means return the path;
- an ignore-check error is logged and treated as not ignored so a transient Git metadata problem does not suppress live updates.

- [ ] **Step 4: Run the focused tests and verify they pass**

Run:

```bash
cargo test watcher::tests::directory_discovery_skips_ignored_and_inaccessible -- --nocapture
cargo test watcher::tests::ignored_source_events_are_filtered -- --nocapture
```

Expected: PASS, with the unreadable directory absent and the readable sibling still discovered.

---

### Task 3: Keep the watcher thread alive and add dynamic directory registration

**Files:**
- Modify: `src/watcher.rs` inside `FileWatcher::start` and watcher helpers
- Test: `src/watcher.rs` watcher behavior tests

**Interfaces:**
- Consumes: `discover_watch_directories`, `register_directory`, and the Git-aware `filter_event` from Task 2.
- Produces: a watcher thread that remains connected after per-directory errors and can watch new directories.

- [ ] **Step 1: Add a watcher-thread command channel**

Create a `std::sync::mpsc::channel::<PathBuf>()` before constructing `RecommendedWatcher`. The callback owns the command sender; the watcher thread owns the receiver.

The callback must:

```rust
if is_created_directory_event(&event, path) {
    let _ = watch_request_sender.send(path.to_path_buf());
}

if let Some(file) = filter_event(&event, &git) {
    if let Err(error) = sender.blocking_send(file) {
        debug!("FileWatcher: failed to send path: {}", error);
    }
}
```

For `Err(error)` callback results, log a warning containing the notify error instead of silently ignoring it.

- [ ] **Step 2: Replace the recursive startup watch**

After constructing `RecommendedWatcher`:

1. Discover the initial readable, non-ignored directories.
2. Call `register_directory(..., directory)` for each one.
3. Do not call `watcher.watch(&workspace, RecursiveMode::Recursive)`.

This ensures `infra/neo4j/import` cannot abort setup for the rest of the workspace.

- [ ] **Step 3: Process dynamic directory requests on the watcher thread**

Replace the one-minute sleep loop with a blocking receive loop over the command channel. For each requested path:

- verify it is a directory;
- reject hidden or Git-ignored directories;
- call `register_directory` with the deduplication set;
- log and continue if the directory is inaccessible or registration fails.

Add a pure helper `is_created_directory_event(event: &Event, path: &Path) -> bool` that returns true only for create events whose path is a directory and is not hidden.

- [ ] **Step 4: Add a regression test for readable events after an inaccessible sibling**

Use a temporary repository with `visible/source.rs` and a Unix `blocked` directory. Discover and register all returned directories using `RecursiveMode::NonRecursive`, write to `visible/source.rs`, and receive a notify event with a bounded `recv_timeout`. Assert the visible path is delivered even though `blocked` was inaccessible.

- [ ] **Step 5: Add a regression test for new directories**

Watch a readable parent non-recursively, create a child directory, send that create path through the same registration helper, register it, then create `child/new_source.rs`. Assert a subsequent event identifies the new source file. Restore permissions and drop the watcher after the assertion.

- [ ] **Step 6: Run all watcher tests**

Run:

```bash
cargo test watcher -- --nocapture
```

Expected: PASS, including permission skipping, Git-ignore filtering, readable sibling events, and newly-created directory coverage.

---

### Task 4: Update watcher documentation

**Files:**
- Modify: `docs/TECHNICAL.md:255-262`

**Interfaces:**
- Consumes: the implemented watcher behavior.
- Produces: documentation matching runtime behavior.

- [ ] **Step 1: Update the incremental-sync section**

Keep the existing four-step flow and add a short note directly below it:

```text
The file watcher discovers readable directories using the repository's
.gitignore rules and registers them independently. Unreadable directories
(such as Docker-owned bind mounts) are logged and skipped; they do not stop
watching or overlay updates for the remaining workspace. Newly-created
readable directories are registered as they appear.
```

- [ ] **Step 2: Check for stale watcher claims**

Search the repository for `watching`, `RecursiveMode::Recursive`, `channel disconnected`, and `file_watcher`. Update only comments or documentation that still claim the entire workspace is watched by one recursive registration.

- [ ] **Step 3: Run formatting and documentation-adjacent checks**

Run:

```bash
cargo fmt --all -- --check
```

Expected: PASS.

---

### Task 5: Run the full automated verification

**Files:**
- No additional files.

**Interfaces:**
- Consumes: the completed watcher implementation and tests.
- Produces: verified Rust build and test results.

- [ ] **Step 1: Run focused watcher tests again after formatting**

```bash
cargo test watcher -- --nocapture
```

Expected: PASS.

- [ ] **Step 2: Run the full Rust test suite**

```bash
cargo test --all-targets
```

Expected: PASS with no regressions.

- [ ] **Step 3: Build the release binary used for live validation**

```bash
cargo build --release
```

Expected: PASS and a rebuilt `target/release/lain`.

- [ ] **Step 4: Inspect the diff and working tree**

```bash
git diff --check
git status --short
git diff -- src/watcher.rs docs/TECHNICAL.md
```

Expected: only the watcher implementation, watcher tests, documentation, and approved spec/plan files are changed; no permission or generated workspace data changes appear.

---

### Task 6: Validate one server with Kimi Code and Claude Code concurrently

**Files:**
- Runtime only; no source files.
- Existing runtime log: `/home/sebastian/monitor/monitor_dm_system/.lain/server.log`

**Interfaces:**
- Consumes: the rebuilt binary and existing `scripts/lain-server-manager.sh` singleton configuration.
- Produces: evidence that the watcher survives the unreadable Neo4j directory and two MCP clients can use the same server.

- [ ] **Step 1: Confirm the test workspace and permissions**

Run:

```bash
stat -c '%A %u:%g %n' \
  /home/sebastian/monitor/monitor_dm_system/infra/neo4j/import
```

Expected: mode `700` and owner UID/GID `7474:7474` remain unchanged.

- [ ] **Step 2: Restart the singleton with the rebuilt binary**

Before this step, obtain explicit confirmation to replace the installed binary at `/home/sebastian/.local/lain/lain` and stop/restart the current singleton process. After confirmation, install the verified build and restart only through the existing manager:

```bash
install -m 755 target/release/lain /home/sebastian/.local/lain/lain
/home/sebastian/monitor/monitor_dm_system/scripts/lain-server-manager.sh restart
```

The manager must continue to pass the explicit workspace path and port 9999. Do not start a second Lain server on port 9999.

- [ ] **Step 3: Verify startup logs**

Inspect the new tail of:

```text
/home/sebastian/monitor/monitor_dm_system/.lain/server.log
```

Expected:

- a warning identifying the unreadable or ignored Neo4j directory;
- a successful watcher-start message or equivalent per-directory registration evidence;
- no `FileWatcher: failed to watch workspace`;
- no `FileWatcher: channel disconnected`.

- [ ] **Step 4: Connect the Kimi Code window**

Use the existing MCP proxy configuration and call a read-only health/query operation. Confirm the response identifies `monitor_dm_system` and the request completes through port 9999.

- [ ] **Step 5: Connect the Claude Code window**

Use its existing MCP proxy configuration and perform the same read-only health/query operation while Kimi remains connected. Confirm both clients receive valid responses from the same server.

- [ ] **Step 6: Exercise live watching from both clients**

From one client, edit a small tracked source file in a readable, non-ignored directory. From the other client, query the affected symbol or overlay-backed result after the debounce interval. Confirm the update is visible without restarting the server.

- [ ] **Step 7: Confirm stability and permissions**

Recheck the server log and Neo4j directory:

```bash
stat -c '%A %u:%g %n' \
  /home/sebastian/monitor/monitor_dm_system/infra/neo4j/import
```

Expected: the watcher remains running, no channel-disconnect warning appears, and the directory remains mode `700` owned by `7474:7474`.

Do not commit or push the changes unless the user explicitly authorizes that git mutation.
