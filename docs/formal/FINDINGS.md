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
