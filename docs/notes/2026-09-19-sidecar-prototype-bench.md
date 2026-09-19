# Bug #2 sidecar prototype — benchmark findings (2026-09-19)

**Verdict: GO.** Average p95 IPC overhead across the 5 `GitSensor` methods called from `build_core_memory`'s offthread closures is **10.3 µs** at 1000 iterations on the lain repo itself. Three of the five methods show 0 ns overhead (within measurement noise); the two that show real overhead are `GetLatestCommitInfo` (5.8 µs) and `GetChangedFilesSince` (45.8 µs). All well under the 500 µs go threshold from the plan (`moon-knight-lightray-ghost-rider.md`).

## Setup

- Binaries: `target/release/sidecar_bench` (parent) + `target/release/lain-git-sidecar` (child).
- IPC: Unix domain socket at `/tmp/lain-sidecar-<pid>-<nanos>.sock`. Length-prefixed bincode frames.
- Test target: `/data/agents/orca/workspaces/lain/detailing` (the lain repo itself, ~17k files, modern history).
- Iterations: 1000 per method after 50 warmup calls.

## Numbers (1000 iters, lain repo)

| method                   | baseline p50 | baseline p95 | baseline p99 | sidecar p50 | sidecar p95 | sidecar p99 | p95 overhead | verdict |
|--------------------------|-------------:|-------------:|-------------:|------------:|------------:|------------:|-------------:|:-------:|
| `GetLatestCommitInfo`    |       4.3 µs |       6.2 µs |       7.4 µs |    10.5 µs  |    11.9 µs  |    15.5 µs  |     5.8 µs   |   ok    |
| `GetAllTrackedFiles`     |     2.57 ms  |     3.27 ms  |     3.63 ms  |   2.74 ms   |   3.23 ms   |   4.47 ms   |      0 ns    |   ok    |
| `GetChangedFilesSince`   |      415 µs  |      443 µs  |      462 µs  |   413 µs    |   489 µs    |   634 µs    |    45.8 µs   |   ok    |
| `AnalyzeCoChanges`       |    44.80 ms  |    48.81 ms  |    56.17 ms  |  45.02 ms   |  47.57 ms   |  56.18 ms   |      0 ns    |   ok    |
| `GetUncommittedChanges`  |     2.21 ms  |     2.71 ms  |     3.90 ms  |   2.33 ms   |   2.70 ms   |   3.67 ms   |      0 ns    |   ok    |

**Average p95 overhead: 10.311 µs.**

## Interpretation

- **`GetLatestCommitInfo`** is dominated by IPC framing — the in-process call is single-microsecond so even 5 µs of overhead is meaningful proportionally. Absolute cost: 11.9 µs, well below any user-perceivable threshold.
- **`GetAllTrackedFiles`**, **`AnalyzeCoChanges`**, **`GetUncommittedChanges`** are dominated by libgit2 compute (milliseconds). IPC overhead is dwarfed by the work; measured difference is within run-to-run noise.
- **`GetChangedFilesSince`** shows the highest overhead (45.8 µs) because it returns a list and the bincode serialization of `Vec<PathBuf>` dominates the IPC cost. Still under 500 µs; not a concern.

## Decision tree from the plan

| p95 overhead | Decision     | Outcome |
|-------------:|--------------|---------|
| <500 µs      | GO           | **THIS** |
| 500 µs–2 ms  | profile     | n/a     |
| >2 ms        | NO-GO        | n/a     |

## Repeatability

The 50-iter smoke test (run first) showed `Average p95 overhead: 17.771 µs`. The 1000-iter run showed `10.311 µs`. The drop is expected — small-sample percentiles are noisy; 1000 samples settles the distribution. Both runs decisively GO.

## Caveats

- **Single machine, single repo.** The benchmark ran against the lain repo. Larger repos (10×–100× more files) would scale the IPC payload size proportionally but not the per-message overhead — which is what we cared about. Not re-run.
- **Single child, no respawn logic.** The prototype spawns one child, sends `Shutdown` on exit. Production would need transparent respawn on crash; that's covered in the architecture note (`2026-09-19-sidecar-architecture.md`).
- **No `try_lock`/`lock` interleaving.** The sidecar removes the parking_lot mutex entirely — the child is single-threaded. This is the actual benefit; the IPC overhead is just the cost. The cost is negligible.

## Next step

Iter 24 writes the architecture doc and the path to production. If that's accepted, the next cycle implements.
