# Verification findings

What the formal and deterministic tooling found in Lain, how, and what it
does **not** cover. Every row below was reproduced by a failing model or test
*before* the fix, and the same check passes after it.

## Defects found and fixed

| # | Defect | Found by | Pre-fix evidence | Fix |
|---|--------|----------|------------------|-----|
| 1 | A late `ready()` from a finished indexing pass opened the readiness gate while another pass was still mutating the graph; a racing pass could also un-publish `index_cancelled` | TLA+ `ReadinessLifecycle` | 5-step trace | `PassGuard`, terminal cancel (`readiness.rs`) |
| 2 | A reload request sent before the rebuild loop subscribed was dropped; `ReloadBus::status()` returned a made-up `Idle` under contention | TLA+ `ReloadBus` | 3-step trace | subscribe before spawn; blocking sync mutex |
| 3 | A file-level `Read` hid the same agent's symbol-level `Edit`, so another agent was granted a file-level `Edit` over it | proptest state machine (`OccupancyMap` vs. spec) | shrunk to 3 operations | `FileOccupancy::strongest_intent` |
| 4 | Lock file names exceeded `NAME_MAX` for deep paths, so acquire failed with a bogus conflict and an empty holder | proptest | 140-byte path → 256-byte name | hash the tail past 200 bytes |
| 5 | `unsafe impl Sync for GitSensor` on the false premise that libgit2 repositories are thread-safe, while the sensor is shared across threads; `current_patterns()` returned `&'static` from a scoped raw pointer | review, Miri target | — | `Mutex<Repository>`; closure-scoped accessor |
| 6 | Snapshot installed with `held == 0`, lock dropped, hold taken later: an install at capacity could evict it in between | TLA+ `SnapshotInstallHold`, loom | 4-step trace | `install_resident_held`, `hold_if_resident` |
| 7 | An `open`/`stale` annotation query returned nothing when newer resolved rows filled the limit | proptest (filter ∘ limit contract) | shrunk case | limit applied after reclassification |
| 8 | Exponential-time glob matching: `*a*a*a…b` against a long segment (a hostile `CODEOWNERS` could hang indexing) | review, proptest vs. original definition | — | iterative single-star backtracking |
| 9 | `format_duration` printed `-30s ago` despite its documented absolute-value contract | review, proptest | — | work on the unsigned value |
| 10 | Filesystem lock acquire was scan-then-create: concurrent acquirers all won (8 of 8 in the first stress round) | TLA+ `FsLeaseGuard`, stress test | 5-step trace | `O_EXCL` guard file around scan+create |
| 11 | Background-job registry: cap counted and inserted under separate locks; a panicking job stayed `Running` forever (leaking a slot); jobs persisted as `Running` reloaded as `Running` after a restart (ten crash-orphans locked out all background work); completed jobs never evicted; snapshots could roll `jobs.json` back | TLA+ `JobRegistry` (one switch per defect), threaded test | 4 separate counterexamples | `job_store.rs`, panic-observing task wrapper |
| 12 | Sidecar respawn budget counted only *successful* respawns, so a sidecar that dies on startup was retried without limit | targeted test | budget never tripped | record the attempt first |
| 13 | `mark_ready` vs `hold_ready(false)`: a release racing the end of indexing left a repo stuck `Indexing` | TLA+ `HoldGate`, loom | 5-step trace | `HealthGate` (one lock) |

### Cross-platform CI (the lanes that never ran on `dev`)

The `ci-probe/*` workflow runs macOS, Windows and coverage. Plain `dev` fails
macOS, Windows and the `lain-version` check; each round of fixes exposed the
next pre-existing failure:

* macOS: `socket_path_for` hashed an unresolved path before a config existed
  and the canonical one after, so behind a symlinked temp dir (`/var` →
  `/private/var`) the reload socket moved and the signal went nowhere;
  `contracts-fixture.sh` used GNU-only `date -d`.
* Windows: index-cache reads failed with `ACCESS_DENIED` while another thread
  replaced the file (now retried); `.sh` fixtures were executed directly and,
  once routed through `bash`, picked up the WSL launcher instead of Git Bash.
* coverage: a daemon process still running (or killed) when profile data was
  merged corrupted it; the e2e test now requires the process itself to exit.
* `lain-version default tracks latest release` still fails: the action pins
  v0.7.4 and v0.8.0 is released. That is a release-PR change (AGENTS.md), left alone.

### Review of the merged B1 (shared oneshot server) and B5 (graph WAL) work

Both arrived with TLA+ specs that passed TLC and unit tests that passed, and
both were unsafe or non-functional. The specs were too weak to notice:
`GraphWal.tla`'s only real invariant was a length bound (`S2` was `TRUE`), and
the "end-to-end recovery" test never reloaded anything.

| # | Defect | Found by | Fix |
|---|--------|----------|-----|
| 14 | **WAL replay never terminated**: replay re-logged each op into the file it was reading, so any restart with a non-empty WAL hung and filled the disk | reload test under a watchdog | replay with logging suppressed; compact after recovery |
| 15 | A torn WAL tail was ignored but left in place, so every frame appended after recovery was unreachable at the next crash | test | `replay_and_repair` truncates to the last intact frame |
| 16 | WAL order differed from in-memory order (4 of 6 mutators logged before taking the graph write lock): recovery diverged from the pre-crash graph | TLA+ `GraphWalOrder`, concurrent test | log under the write lock |
| 17 | Checkpoint serialised, wrote, then truncated the live WAL, dropping ops acknowledged meanwhile (16 of 400 lost) | TLA+ `GraphWalCheckpoint`, concurrent test | rotate under the lock, discard after the snapshot |
| 18 | `insert_edges_batch` logged edges it then dropped, and replay aborted on the first "not found", failing startup; one existing test asserted exactly that bug | test | log only applied edges; tolerate stale ops |
| 19 | The "CRC-32C" table was the IEEE polynomial; one open+fsync per node in batch inserts | test vector, review | real Castagnoli; one write+fsync per batch |
| 20 | **The "shared" oneshot server died with the first call** (owned by `StdioSession`, killed on drop): nothing was ever shared, every call cold-started | real-binary probe | detached daemon mode with idle exit; 2nd call 12.1s → 0.00s |
| 21 | Socket `tools/call` skipped the readiness gate, and could not reach presence/audit/status tools | review, test | same gate → dispatch → envelope pipeline as stdio/HTTP |
| 22 | Liveness by PID file and `/proc`: wrong after PID reuse, always "dead" on macOS (deleting a live server's socket); two daemons starting together could delete each other's socket | review, concurrent test | connect probe; `O_EXCL` start guard |
| 23 | Socket mode left to umask although `tools/call` runs builds/tests; unbounded request lines | review, test | `0600`/`0700`; 16 MiB frame cap |
| 24 | `std::os::unix` / `UnixListener` unconditional: the Windows build would break | review | `cfg(unix)` with a private-stdio fallback (**not compiled here**; needs the CI Windows lane) |
| 25 | `find_anchors` ranked tied scores by `HashMap` order: a second call listed different symbols | e2e test | total order (score, name, path, id) |
| 27 | The WAL paid one open + `fsync` per mutation: harmless on Linux (0.3 s for the graph benchmarks) but it pushed the Windows lane past its 30-minute cap, and bulk indexing would pay a milder version of it | CI probe (Windows job cancelled in `graph_benchmark`) | `WalWriter`: one open handle, `fsync` per `LAIN_WAL_SYNC` (`always`, `never`, or ms; default 100 ms). A process crash loses nothing (frames are in the OS cache after `write`); only a power loss can lose the last window |
| 26 | Uncommitted test-suite fallout: core is 20 tools, manual table stale (the previous session died on a billing error mid-fix) | full test run | counts derived from the registry |

### Parsing, protocol surface and configuration

| # | Defect | Found by | Fix |
|---|--------|----------|-----|
| 28 | **A symlink in a repository exfiltrated files outside it.** A committed symlink to a file outside the workspace was indexed (`is_file()` follows links), and `get_context` / `explain_symbol` returned the target's contents. A hostile repo could read any file the user can through an agent's tool call | containment test over every safe tool (with a positive control so it cannot pass vacuously) | `path_util::resolves_inside` guards `GitSensor` listings, `process_change` (retracts a file swapped for an escaping link) and the node-keyed readers |
| 29 | A zero in `tuning.toml` (`lsp_pool_size`, `files_per_batch`, `nlp_batch_size`, `ingest_batch_size`) hit `% 0` / `chunks(0)` and crashed indexing | targeted test | `IngestionConfig::sanitized` in the loader (warns); the pool never builds empty |
| 30 | Selector glob, tokenizer (doubled newlines, byte-vector stripping, tail handling) and `cosine_similarity` (NaN / out-of-range results) defects | proptest against reference definitions | see the commits on this branch |

Also added without a defect found: arbitrary JSON-RPC requests always get one
well-formed response; 600 arbitrary calls across every safe tool never panic,
hang or leak; `GitSensor` differential-tested against the git CLI;
federation repo add/project/remove (TLA+ and a concurrent stress test) never
resurrects a removed repo.

WAL recovery stays a best-effort accelerator, not a complete log: mutators that
rebuild derived state (`replace_nodes`, `set_embedding`, `reset`, ...) are not
logged, and replay never advances `last_commit`, so the next index pass
reconciles. Recovery also cannot repair a corrupt `graph.bin` (the WAL holds
only post-checkpoint ops).

## Checked and found sound

Token-bucket rate limiter (rate bound, `Retry-After` honesty, map cap);
LSP circuit breaker and restart budget (absorbing `unavailable`, thresholds,
sliding window); `take_batch`; `constant_time_eq` ≡ `==`; civil-date arithmetic
for every day 1970–9999; readiness publication rule; federation graph schema
version refusal (already well tested).

## Known residuals (documented, not fixed)

* **Filesystem lease without fencing tokens.** `FsLeaseGuard_Stall` and
  `FsLease_ExclExpiry` show that a guard holder frozen past `GUARD_TTL`, or an
  expiry-plus-steal sequence, can still produce two believers. Closing it needs
  a fencing token checked by the consumer. The in-memory `OccupancyMap` stays
  authoritative when a server runs.
* `take_batch` iterates a `HashSet`, so under sustained overload an old path
  is not guaranteed to be served before newer ones (no loss, just no FIFO).
* Kani proofs of the date arithmetic cover 1970–9999, not all of `u64`
  (unbounded 64-bit division is intractable for the SAT backend).
* Windows is not compiled locally (no cross C compiler here); the B1 `cfg(unix)`
  gating and every platform-sensitive change needs the CI Windows/macOS lanes.
* None of this runs in CI yet. `make verify` runs TLC + property tests + loom;
  `make kani`, `make miri` and `make mutants` are separate.

## How to reproduce

```
make formal      # TLC: every spec in docs/formal/MANIFEST matches its expected outcome
make proptest    # state machines and property tests
make loom        # exhaustive interleavings of the real code (--cfg lain_loom)
make kani        # bounded model checking
make miri        # the raw-pointer thread-local
make mutants     # do these tests catch injected bugs?
```
