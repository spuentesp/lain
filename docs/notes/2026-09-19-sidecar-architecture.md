# Bug #2 sidecar — full architecture (2026-09-19)

Companion to the benchmark findings (`2026-09-19-sidecar-prototype-bench.md`). The prototype verified the IPC overhead is negligible (~10 µs avg p95). This note designs the production shape: where `SidecarGitSensor` lives, how the schema is versioned, how the child is respawned on crash, and how federation health surfaces the sidecar state.

## Goals

1. **Eliminate the Bug #2 cascade entirely.** A hung libgit2 call can no longer block any parent thread; the parent can `kill -9` the child and respawn it transparently.
2. **Keep the in-process fast path.** For tests and small-scale runs, fall back to the in-process `GitSensor` so unit tests don't have to spawn a child.
3. **Production-grade error handling.** Child crash → respawn → retry → surface error to federation health after N retries.
4. **Backwards-compatible migration.** Existing call sites (`build_core_memory`, watcher) keep calling `GitSensor::method()` — `SidecarGitSensor` is a drop-in replacement behind a config flag.

## Public API shape

```rust
// src/server/git.rs — the existing in-process sensor.
pub struct GitSensor { /* unchanged */ }
impl GitSensor {
    pub fn new(workspace: &Path) -> Result<Self, LainError> { /* unchanged */ }
    pub fn get_latest_commit_info(&self) -> Result<(String, i64), LainError> { /* unchanged */ }
    pub fn get_changed_files_since(&self, since: &str) -> Result<Vec<PathBuf>, LainError> { /* unchanged */ }
    pub fn get_all_tracked_files(&self) -> Result<Vec<PathBuf>, LainError> { /* unchanged */ }
    pub fn analyze_co_changes(&self, w: usize, mp: usize, mf: usize) -> Result<Vec<CoChangePair>, LainError> { /* unchanged */ }
    pub fn get_uncommitted_changes(&self) -> Result<Vec<FileChange>, LainError> { /* unchanged */ }
    pub fn get_commit_history(&self, count: usize) -> Result<Vec<CommitInfo>, LainError> { /* unchanged */ }
    pub fn is_valid(&self) -> bool { /* unchanged */ }
}

// New: same surface, backed by IPC to a child process.
pub struct SidecarGitSensor {
    child: Arc<Mutex<ChildHandle>>,
    workspace: PathBuf,
}
impl SidecarGitSensor {
    pub fn new(workspace: &Path) -> Result<Self, LainError> { /* spawn child, handshake */ }
    // ... method signatures identical to GitSensor ...
}

// New: ergonomic enum so existing call sites switch with one line.
pub enum AnyGitSensor {
    InProcess(GitSensor),
    Sidecar(Arc<SidecarGitSensor>),
}
impl AnyGitSensor {
    pub fn new(workspace: &Path, mode: GitSensorMode) -> Result<Self, LainError> { ... }
    // ... method signatures identical ...
}

pub enum GitSensorMode {
    /// Default. Same-process libgit2 — current behavior. No IPC.
    InProcess,
    /// Host `git2::Repository` in a child process; route calls over Unix socket.
    Sidecar,
}
```

`build_core_memory` and the watcher hold `Arc<AnyGitSensor>` instead of `Arc<Mutex<GitSensor>>`. The mutex around the sensor goes away entirely (the in-process variant is `Send + Sync` because the parking_lot mutex was only needed to serialize concurrent libgit2 calls, which the sidecar handles implicitly by serializing on the child's listener).

## IPC schema versioning

The prototype uses raw bincode. Production needs:

- A `version: u32` field in the first frame after connect. Mismatch → parent kills the child, spawns the right one, retries.
- A schema constant per protocol version. The protocol lives in `src/sidecar_proto.rs` (already exists); add `pub const PROTOCOL_VERSION: u32 = 1;`.
- On connect: child sends its protocol version; parent checks; if mismatch, error out (or kill+respawn with a downgrade flag — out of scope for the first cut).

## Child lifecycle

```
                 +-----------+
                 |  parent   |
                 |  (lain)   |
                 +-----+-----+
                       |
   spawn  +------------|------------+
          |            |            |
          v            v            v
    [child alive]  [child hung]  [child dead]
       IPC ok      kill -9 +      respawn +
                   retry          retry
```

State machine:
- **Spawning**: parent has called `Command::spawn`, waiting for the socket file to appear.
- **Healthy**: socket connected, last call returned successfully within the timeout.
- **Suspect**: last call timed out or returned an error. Next call attempts respawn before retry.
- **Dead**: too many consecutive respawns without a successful call. Surface `LainError::Unavailable("git sidecar down: ...")` to the caller.

The respawn budget: 3 retries in 30 s. Exceeded → mark dead, return `Unavailable` until the operator intervenes (or until a watchdog kicks the child externally).

## Federation health

`SidecarGitSensor::health() -> SidecarHealth` exposes:
- `alive: bool` — last call succeeded within timeout.
- `last_call_duration: Duration` — for observability.
- `consecutive_failures: u32` — for the respawn budget.
- `child_pid: Option<u32>` — for `/health` and `get_capabilities`.

`LainServer` reads this on every `get_health` call and surfaces it under `health.federation.git_sensor.kind = "sidecar" | "in_process"`. If `kind = "sidecar"` and `alive = false`, the federation transitions to `Degraded` with a clear message: `"git sidecar down: <reason>; respawn attempts: N"`.

## Error handling

| Failure mode              | Parent action                          | Caller sees                              |
|---------------------------|----------------------------------------|------------------------------------------|
| Child process killed       | Respawn once, retry                     | `Unavailable` if respawn+retry fails     |
| Socket read timeout (>2s) | Kill child, respawn, retry              | `Unavailable` after N consecutive        |
| libgit2 error inside child | Surface via `Response::Error(String)`   | `LainError::Git(msg)` (preserves surface) |
| Schema mismatch             | Kill child, log fatal, surface           | `Fatal("git sidecar schema mismatch")`    |
| Child exits unexpectedly   | Detect via broken pipe, respawn          | `Unavailable` until next successful call  |

The `LainError::Git(msg)` path is the same as today's in-process behavior, so existing call sites in `build_core_memory` and the watcher don't change their error handling.

## Backwards compatibility

1. **`AnyGitSensor` enum** is the only public-facing change. `Arc<Mutex<GitSensor>>` → `Arc<AnyGitSensor>` is mechanical at call sites.
2. **Default mode is `InProcess`.** Operators opt in with `LAIN_GIT_SENSOR=sidecar` (env var) or a tuning config flag. The default stays the same as today — small deployments see no behavior change.
3. **Tests stay on `InProcess`.** Unit tests don't need a child. Integration tests can opt into `Sidecar` to exercise the IPC layer.
4. **`build_core_memory`'s offthread closures** currently use `try_lock()` (PR #153) to fail-fast. With sidecar, the closures call `AnyGitSensor::get_X()` directly — no `try_lock` needed. The mitigation is replaced by the sidecar's timeout. Watch PR #153's `try_lock` codepath can be deleted once the sidecar is the default.

## Migration plan

| Step | Action | Effect |
|------|--------|--------|
| 1 | Land the prototype binaries (`feat/sidecar-prototype` — this PR) | Operators can opt in via env var on a trial run |
| 2 | `SidecarGitSensor` implementation behind `AnyGitSensor` | Drop-in replacement for the production code path |
| 3 | Federation health surfaces `git_sensor.kind` | Observability of which mode is in use |
| 4 | Default mode flip after one release of soak time | New default is `Sidecar`; old default `InProcess` available as fallback |
| 5 | Delete `try_lock` codepath from PR #153 | Mitigation is now redundant; watchdog (PR #165) stays as belt-and-suspenders |

Each step is its own PR; the prototype in this cycle is step 1.

## Files touched (production implementation, future cycles)

- `src/server/git.rs` — add `AnyGitSensor`, `SidecarGitSensor`, `GitSensorMode`.
- `src/sidecar_proto.rs` — add `PROTOCOL_VERSION: u32 = 1`; add `version` field to first frame.
- `src/sidecar.rs` (new) — child process spawn, handshake, respawn logic.
- `src/server/ingest/ingestion.rs` — `build_core_memory` uses `AnyGitSensor`; remove `try_lock` codepath (step 5).
- `src/server/federation/repo_index.rs` — same.
- `src/server/ingest/handles/ingest.rs` — `start_git_sensor_watchdog` becomes a no-op when sidecar is enabled (the watchdog fires only if the parking_lot mutex is held; with sidecar there's no mutex).
- `src/server/mcp/handler.rs` — `get_health` reports `git_sensor.kind`.
- `src/sidecar_bench.rs` and `src/bin/lain-git-sidecar.rs` — already in place; promoted from "prototype" to "the real thing".

## Risks

- **Respawn storms.** If the child crashes on every call, the respawn loop hammers the kernel. Mitigated by the 3-in-30-s budget; once exceeded, surface `Unavailable` and stop trying.
- **Path canonicalization.** The child reads `repo_path` from argv. If the parent and child disagree about the workspace path, libgit2 won't find the repo. Mitigated by passing the path through `std::fs::canonicalize()` before spawn.
- **File descriptor inheritance.** The child inherits the parent's fds. Standard `Command::spawn` closes them by default via `close-on-exec`, but if `LAIN_TRACE_RUNTIME=true` or similar instrumentation opens persistent fds, the child could keep them. Mitigated by closing them explicitly on the child side via `libc::prctl(PR_SET_PDEATHSIG, SIGTERM)` (Linux) or similar.
- **Schema migration cost.** v1 is locked in for this cycle. v2 (e.g., adding a new method) is a non-breaking additive change if old parents ignore unknown variants. Production schema discipline needs to start now.

## Open questions for the production cycle

1. **Where does the child binary live in CI?** Right now it's a `[[bin]]` in the same Cargo manifest. Cargo builds it; we install alongside `lain`. For deploy, this means shipping two binaries. OK.
2. **Should the sidecar be persistent across `LainServer` restarts?** No — the child is per-process. Restarting `lain` also restarts the child. Avoids stale state.
3. **What about tests that exercise `GitSensor` directly?** They keep using `GitSensor::new` (the in-process variant). New tests for `SidecarGitSensor` need to be integration-style (spawn the child, talk to it, tear down).
4. **What about `cargo bench`?** The `sidecar_bench` binary is a one-shot, not a Criterion benchmark. Converting to Criterion would let us track regressions over time. Defer to a future cycle.

## Status

This is a design doc. The prototype that proves the IPC-overhead assumption lives in this cycle's branch (`feat/sidecar-prototype`). The production implementation is the next 5-iteration cycle, gated on this design being accepted.
