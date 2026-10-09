# B5: Graph WAL — implementation design

Companion to `GraphWal.tla` (the formal spec).

## Goal

The on-disk `graph.bin` is a single bincode snapshot. A torn
write (process crash, full disk, laptop unplugged) makes the
whole file unreadable; recovery is a full reindex from source
(~5 min for a 41k-LOC repo). This change introduces a
write-ahead log: every `insert_node` / `upsert_edge` is
appended to `graph.wal` first, and a periodic checkpoint
atomically writes `graph.bin` and truncates the WAL.

## On-disk layout

```
.lain/
  graph.bin      <- last checkpoint, atomic rename from graph.bin.tmp
  graph.wal      <- append-only log of ops since the last checkpoint
  graph.bin.tmp  <- temp file written during a checkpoint
  graph.wal.tmp  <- temp file used for log rotation (rare)
```

The two temp files are the only ones that exist briefly. They
are renamed atomically (`rename(2)`) on success, removed on
failure. The `.lain/` directory layout does not change.

## Op format

`graph.wal` is a stream of length-prefixed bincode 2.x frames:

```
[ u32 length | bincode(GraphOp) payload ] [ u32 length | ... ]
```

`GraphOp` is a new enum:

```rust
enum GraphOp {
    UpsertNode(GraphNode),
    UpsertEdge(GraphEdge),
    RemoveNodesByIds(Vec<String>),
    RemoveEdges { endpoints: Vec<(String, String, EdgeType)> },
    CommitIndexMap { version: u64 },
}
```

CRC32C is appended to each frame for torn-write detection
within the WAL (a torn frame is rejected; the next good frame
is treated as the end of the WAL).

## Lifecycle

1. **Open (cold start).** Read `graph.bin` if present; if its
   header version matches `FEDERATION_GRAPH_VERSION`, load the
   in-memory state. Then read `graph.wal` frame by frame,
   applying each `GraphOp` to the in-memory state. If a frame
   fails to decode, treat the WAL as truncated at that point
   and stop (the in-memory state is the graph up to that
   frame).
2. **Write.** Every `upsert_node` / `upsert_edge` /
   `remove_*` call first appends the `GraphOp` to
   `graph.wal`, fsyncs, then applies it to the in-memory
   state. The fsync is what gives us write-ahead durability.
3. **Checkpoint.** Every N ops (default: 4096), or every M
   seconds (default: 60), encode the in-memory state to
   `graph.bin.tmp`, rename to `graph.bin`, then truncate
   `graph.wal` to length 0. The rename is atomic; a crash
   between the rename and the truncate leaves a slightly
   longer WAL that replays to the same state.
4. **Recovery.** On startup with a corrupt or missing
   `graph.bin` (e.g. the on-disk graph.bin was truncated),
   find the most recent valid checkpoint in
   `graph.bin.tmp` / `graph.bin` / `graph.wal` and replay
   from there. The user never has to manually move the
   graph aside again.

## State mapping (TLA+ ↔ Rust)

| Spec | Rust |
|---|---|
| `log` | `graph.wal` file contents (the post-checkpoint tail) |
| `checkpoint` | `graph.bin` (the last atomic snapshot) |
| `in_recovery` | the `load_from_disk` codepath on startup |
| `AppendOp` | the per-op `fsync(graph.wal)` + in-memory mutation |
| `Checkpoint` | the per-N-op background task |
| `Crash` | every `?` on disk I/O + the existing `SIGTERM` handler |

The spec's `DurableHistory == checkpoint \o log` is exactly
what the loader produces by replaying `graph.wal` on top of
the snapshot from `graph.bin`. A torn write loses the tail of
the WAL but the snapshot is intact; recovery is a few seconds
of WAL replay, not a 5-minute reindex.

## Concurrency invariants (cross-reference TLA+)

| Spec invariant | Rust enforcement |
|---|---|
| S1: `Len(log) <= MaxLog` | The checkpoint task runs whenever the WAL exceeds the threshold (default 4096 frames) and truncates. |
| Recovery always terminates | The replay loop is bounded by the file size; it cannot loop infinitely. |
| Eventually checkpointed | The checkpoint task runs on a tokio interval, so the WAL cannot grow unbounded. |

## Implementation order (commit-by-commit)

1. `feat(graph): GraphOp enum + bincode codec` with round-trip
   property tests.
2. `feat(graph): WAL writer (append + fsync)` and `WALReader`
   (frame-by-frame, CRC-checked, truncate-tolerant).
3. `feat(graph): WAL-driven load path` —
   `GraphDatabase::load_from_disk` reads `graph.bin` then
   replays `graph.wal`. Tolerant of missing WAL, corrupt WAL,
   and torn frames (treat them as end-of-log).
4. `feat(graph): periodic checkpoint` — a tokio task that
   runs every N ops or every M seconds and writes a fresh
   `graph.bin` from the in-memory state.
5. `feat(doctor): detect torn writes without manual move`.
   Today `lain doctor` says "move .lain/graph.bin to a backup,
   then run `lain mcp` to rebuild." With the WAL, that
   guidance becomes "the loader will recover automatically;
   if it doesn't, file an issue."

## Out of scope (defer)

- Compression of `graph.wal`. For 41k LOC the WAL is a few MB;
  not worth the complexity.
- A real-time replica / log shipping. The WAL is local-only.
- Federation. The federation already has its own envelope
  format (`LNF2`) and its own index-generation consistency
  story (see `IndexGeneration.tla`). The B5 WAL is for the
  per-repo graph.
- Cross-version WAL upgrade. `graph.bin` already has a
  version header; the WAL inherits the same envelope.
