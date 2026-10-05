# What is verified, with what, and what is not

"Every part of the app" cannot be proven formally: most of Lain is parsing,
I/O and glue where the right tool is tests and fuzzing, not a model checker.
This map says which tool checks which part, and, as importantly, where
nothing does. Findings per row are in [`FINDINGS.md`](FINDINGS.md).

Legend: ● checked · ◐ partly · ○ not checked.

## State machines and concurrency protocols

| Part | TLA+ | State-machine / property tests | loom | Kani | Mutation |
|------|:----:|:------------------------------:|:----:|:----:|:--------:|
| Readiness lifecycle (`readiness.rs`) | ● | ● refines the model | ● | ● publication rule | ● |
| Reload bus (`reload.rs`, rebuild loop) | ● | ● status machine | ● | ○ | ● |
| Presence claims (`OccupancyMap`) | ○ | ● refines the claim spec | ○ | ○ | ○ |
| Filesystem lease (`presence_lock.rs`) | ● | ● stress + properties | ○ | ○ | ● |
| Background jobs (`job_store.rs`) | ● | ● refines a cap model + threaded | ○ | ○ | ○ |
| Snapshot residency (`manager.rs`) | ● (4 specs) | ● | ● install/evict/hold | ○ | ○ |
| Contract rejoin flag (`DirtyFlag`) | ● | ● | ● | ○ | ○ |
| Repo health + startup hold (`HealthGate`) | ● | ● | ● | ○ | ● |
| Graph WAL / checkpoint (`graph/wal.rs`) | ● (2 specs) | ● crash, order, checkpoint races | ○ | ○ | ● |
| Shared oneshot server (B1) | ◐ spec as merged | ● socket properties, real-binary e2e | ○ | ○ | ○ |
| LSP circuit breaker / restart budget | ○ | ● refines the documented spec | ○ | ○ | ○ |
| Federation repo add / project / remove | ● | ● concurrent stress | ○ | ○ | ○ |
| Auth token bucket | ○ | ● rate bound, `Retry-After`, cap | ○ | ● `constant_time_eq` | ○ |
| Sidecar respawn budget | ○ | ◐ targeted test | ○ | ○ | ○ |
| LSP pool (size, round-robin) | ○ | ● zero size, balance under racing clones | ○ | ○ | ○ |
| File-watcher batching (`take_batch`) | ○ | ● partition | ○ | ○ | ○ |

## Pure functions and unsafe code

| Part | Tool |
|------|------|
| Civil-date arithmetic (1970–9999) | Kani |
| CODEOWNERS glob | proptest against the original recursive definition |
| Annotation list filter ∘ limit contract | proptest model |
| `format_duration` | proptest |
| Raw-pointer thread-local (`sensors/util.rs`) | Miri |
| `unsafe impl Sync for GitSensor` | removed (not verified, deleted) |

## Not covered by any of the above

* **Parsing and extraction**: tree-sitter symbol/edge extraction, the sensors
  (HTTP, gRPC, GraphQL, SQL, OpenAPI, entry points), query-language parsing.
  Example-based tests and the existing `fuzz/` harness only.
* **Protocol framing and servers**: JSON-RPC dispatch and every safe tool are
  fuzzed in-process (no panic, hang or out-of-workspace read). HTTP framing,
  Command Center and SSE are integration tests only; process-spawning tools
  (`run_build`, `run_tests`, ...) are excluded from the fuzz.
* **Federation indexing pipeline** end to end (ordering across repos,
  cross-repo resolution) beyond the specific protocols above.
* **NLP / embeddings**, LSP process management, git semantics.
* **Persistence formats** other than the WAL: schema-version refusal is tested
  by examples, not modelled.
* `OccupancyMap`, `job_store`, `manager.rs`, `DirtyFlag` and the socket server
  have property tests but have not been through mutation testing.

## Caveats that apply to everything above

* TLA+ models are hand-written. They are tied to the code by naming, by the
  regression test beside each fix, and (loom) by running the real code, not by
  a refinement proof. A model can be wrong in the same way the code is; several
  of the specs that arrived passing (`GraphWal.tla`) checked nothing useful.
* loom models extracted protocol cores (`DirtyFlag`, `HealthGate`,
  `ReadinessHandle`, the residency lock), not whole subsystems.
* Kani proofs are bounded (e.g. dates to year 9999, byte slices to 8 bytes).
* Mutation survivors are listed in `.cargo/mutants.toml` only when genuinely
  unobservable, each with its reason.
* Windows and macOS are exercised by the `ci-probe/*` workflow, not locally.
  None of this tooling runs in CI by default yet.
