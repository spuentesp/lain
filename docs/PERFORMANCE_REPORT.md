# Performance improvement report

Scope: read every hot path in `src/server/{ingest,nlp,graph,query,tools}` and the
two large modules (`treesitter.rs`, `lsp.rs`) to identify *real*, *low-risk*
parallelism and caching wins. Stability is the explicit constraint: the report
is organised by expected effect, then by the stability cost of each change,
with "do not touch" boundaries marked explicitly at the end.

This is analysis only — no code has been changed.

## TL;DR

Lain already has a substantial parallelism and caching baseline:

- `tokio` for I/O, `JoinSet` for batched scan fan-out
- `offthread()` helper routing sync work to the blocking pool with
  cooperative cancel
- LSP prewarm parallelised per-language with `JoinSet`
- ONNX sessions multi-threaded internally (intra-op = `min(cores, 4)`)
- `NlpEmbedder::embed_batch` already amortises matmul across 16 inputs
- Federation loads repos concurrently; `DashMap` for lock-free index reads
- `EmbeddingCache` already short-circuits repeated ONNX calls in the tool path
- `parking_lot` mutexes throughout the graph path

There are still clear wins, organised in three bands below. None of them
require breaking the cancel-token contract, the atomicity contract around
`replace_nodes_for_paths`, or the federation's "be conservative" rule.

| Band | Effort | Expected effect | Stability risk |
|---|---|---|---|
| **A** — already-correct, just unused | S | 2-8× on the hot path | Very low |
| **B** — small, additive | S-M | 1.5-3× on cold index | Low |
| **C** — structural rework | M-L | Variable; some are wins, some are not | Medium |

---

## Band A: low-hanging, already-correct, just unused

These are changes where the right primitive is already a dependency and a
single function is leaving parallelism on the table.

### A1. NLP prewarm + background loop is serial-by-default — biggest single win

Where: `src/server/ingest/ingestion.rs:656-797`.

The detached NLP task iterates `prewarm` (top-anchors) and then `rest`
*one node at a time*:

```rust
for node in &prewarm {
    ...
    let emb_result = offthread(nlp_cancel.clone(), move || {
        embedder_for_call.embed(&text_for_call)
    }).await;
    ...
}
```

`embedder_for_call.embed` calls `embed_batch(&[text])`, which runs ONE ONNX
forward pass for ONE text. The model itself runs up to 4 threads internally,
but for a single input that's almost entirely per-call overhead. The same
comment in `nlp.rs:246-262` already documents that batching 16 inputs takes
the per-call amortisation from ~5 s for 200 calls down to ~600-800 ms — a
6-8× improvement on the same workload.

**Fix:** restructure the loop so chunks of 16 nodes share one
`embed_batch` call. The current outer loop already chunks by `nlp_batch_size`
(the default is 50) — just route the chunk through `embed_batch` and amortise
the per-call cost across `embed_batch`'s `n`.

Also: the per-node `offthread(embed)` round-trip is per-node. For a chunk of
50, that's 50 blocking-pool round-trips and 50 `Session::run` serialisations
under the `parking_lot::Mutex<Session>`. With a batch of 50 going through one
`embed_batch` call, it becomes one round-trip and one `Session::run`. ONNX
serialises concurrent `Session::run` calls, so concurrent batching would
actually *contend* on the same mutex — the right answer is *fewer larger*
calls, not more parallel ones.

**Effect:** 6-8× on the NLP prewarm pass, ~3-5× on the background queue.
The background queue is detached, so the speedup is invisible during the
indexing pass — but the *first* semantic search on a freshly indexed repo
will hit warm embeddings instead of waiting on the queue.

**Stability cost:** none. The `Session::run` mutex already serialises calls;
we are removing calls, not adding them. The `node.embedding = Some(json)`
write path is unchanged. The "never store a default on serialize failure"
guard already lives at the write site.

### A2. Tree-sitter queries are recompiled per-file

Where: `src/server/treesitter.rs:254-315` (`extract`) and `:415-475`
(`extract_definitions_rust`).

Every call constructs a fresh `tree_sitter::Query` for every pattern, on
every file. Query compilation is non-trivial — it walks the AST patterns and
allocates per-pattern state. For a 5 000-file scan with five patterns, that's
25 000 query compilations per language per pass.

`Query::new` accepts any `'static` string pattern, so the patterns are
trivially cacheable. They live in `const RUST_CALLS_1: &str = "..."` already
— the only missing piece is to compile once and share.

**Fix:** a `OnceLock<Query>` per `(Language, pattern)` pair, or a process-wide
`Lazy<HashMap<(Language, &'static str), Arc<Query>>>`. `Query::new` is
`Send + Sync` once compiled, so a single shared instance is safe to share
across threads.

**Effect:** ~10-20% on the tree-sitter pass. Tree-sitter is already the
fast path (no LSP round-trip), so the per-file time shrinks proportionally.

**Stability cost:** none. `QueryCursor` is the per-thread cursor object; the
query itself is read-only after construction.

### A3. `find_all_nodes_by_name` does a full scan on every indexing pass

Where: `src/server/graph/mod.rs:928-938`, called from
`src/server/ingest/resolve.rs:178-184` inside `resolve_static_edges`.

Every indexing pass calls `db.get_all_nodes()` (full clone!) and walks it to
build a `HashMap<String, Vec<(id, type, path)>>` keyed by name. That is one
copy of every node + one allocation per node — once per indexing pass, on top
of the already-allocating petgraph internal state.

The graph already has `DashMap<String, NodeIndex>` for id lookups; adding a
`DashMap<String, Vec<NodeIndex>>` keyed by name is symmetric. Maintained
under the same write lock that already updates `index_map`/`path_index`
(`insert_nodes_batch`, `replace_nodes_for_paths`, `remove_nodes_by_ids`).

**Fix:** add `name_index: DashMap<String, Vec<NodeIndex>>` next to the
existing `path_index`, update it in the three mutating paths. `resolve_static_edges`
then becomes O(refs) instead of O(nodes + refs).

**Effect:** noticeable on large repos where `get_all_nodes()` is the
profiler-visible cost of the resolve phase. The clone is the issue — the
lookup itself is fast.

**Stability cost:** low. The existing test surface for the graph module is
strong; the new index should land behind the same tests, with the existing
behaviour pinned by property tests.

### A4. `find_node_by_path` does a linear scan despite a `path_index`

Where: `src/server/graph/mod.rs:940-946`.

```rust
pub fn find_node_by_path(&self, path: &str) -> Option<GraphNode> {
    self.graph
        .read()
        .node_weights()
        .find(|n| n.path == path)
        .cloned()
}
```

`has_node_at_path` (lines 953-955) already uses `path_index`. The two
functions differ only in *what they return*, not what they index.

**Fix:** look up via `path_index` and return the first `NodeIndex`'s weight.

**Effect:** small but free — this is on the tool hot path for any
path-based query. The `graph.read()` lock is acquired and released in both
forms, but the iteration drops from O(N nodes) to O(N nodes per path),
which for typical queries (one path, handful of nodes) is O(k).

**Stability cost:** none. Same lookup, same lock semantics.

### A5. Resolve phases are sequential loops over O(N) data

Where: `src/server/ingest/resolve.rs` — three functions, three O(N) loops.

`resolve_call_edges` (76-110) is a per-ref linear scan with a per-ref
`get_node_at_location` (which itself does a path-index lookup — fine).

`resolve_static_edges` (168-258) builds the `name_index` above and then
walks `refs` linearly. Once A3 lands, this becomes O(refs).

`resolve_pattern_edges` (265-355) does the value-clustering + scoring +
edge-emission. The dominant cost is the `db.get_all_nodes()` filter for
`NodeType::File` plus the value→files→dirs cluster walk.

All three are pure with respect to the graph (they only call public
`GraphDatabase` methods that take a read lock). They could run on
rayon's `par_iter` with chunked inputs:

- `resolve_call_edges`: `par_iter` over `refs`, each worker takes a
  read lock per ref. The read lock is held only for `get_node_at_location`,
  which is path-index → node-index → clone. Should be <1 µs per lookup,
  but multiplied by N refs it adds up. The right shape is **chunked, with
  one read-lock acquisition per chunk**.
- `resolve_static_edges`: with A3 landed, this is a `par_iter` over
  `refs` resolving each to a node — the read lock is on a per-ref basis
  the same way.
- `resolve_pattern_edges`: the dominant cost is the value-clustering
  pass, which is a hash-build step. `par_iter` over `refs` to build
  per-worker `HashMap<String, Vec<String>>` and merge at the end is the
  textbook rayon pattern.

**Effect:** linear speedup on these phases proportional to core count.
For a 5 000-file repo these phases collectively run for a few seconds;
with 8 cores they would drop to <1 s.

**Stability cost:** low. The graph is read-only during these phases — the
read lock is fine to hold for the per-chunk duration. The merge step at
the end of `resolve_pattern_edges` is a `HashMap::extend` (deterministic
under rayon chunk ordering is *not* required because the output is sorted
by `(score, value, file)`).

### A6. `calculate_anchor_scores` and `calculate_depths` are serial scans

Where: `src/server/graph/mod.rs:1094-1345`.

Both methods take the graph *write* lock and walk all nodes. `calculate_anchor_scores`
is two passes (raw score, then normalise). Each node's raw score depends only
on its own edges, so Pass 1 is embarrassingly parallel. Pass 2 needs the global
`max_raw`, so Pass 1 must complete first; Pass 2 is again embarrassingly
parallel.

`calculate_depths` is BFS from a small set of entry points, which is
fundamentally sequential.

**Fix:** split Pass 1 of `calculate_anchor_scores` into a `compute_raw` step
that takes no lock (the graph is read-only during this phase), then a
`write_normalised` step that takes the write lock once and applies scores.
The compute step can be `par_iter` over node indices.

**Effect:** ~Nx on Pass 1 where N is core count. Pass 2 is O(N) writes
under one lock, which is fast; the speedup comes from Pass 1.

**Stability cost:** medium. The current implementation takes the write
lock for the whole computation; the refactor needs to keep the read-only
invariant during the compute phase. The graph is already not written by
anyone else during the index pass (single-writer pipeline), so the
invariant holds.

---

## Band B: small, additive caches

### B1. Bounded LRU for `EmbeddingCache`

Where: `src/server/tools/registry.rs:38`,
`src/server/query/executor.rs:430-449`.

The current cache is `Arc<Mutex<HashMap<String, Vec<f32>>>>` with no bound.
For a 10 k-node graph with 384-dim vectors, that's ~15 MB per tool executor,
growing with each unique query text.

**Fix:** swap for `lru::LruCache<String, Vec<f32>>` (the `lru` crate has no
new transitive deps and is already vendored-class common). Cap from
`tuning.toml` (e.g., `embedding_cache_capacity: usize` defaulting to 10 000
entries — ~15 MB). Wire the new knob into the existing
`every_tuning_knob_is_read_by_production_code` reachability test.

**Effect:** bounded memory; for cold queries the miss rate is the same;
for repeated workloads the hit rate stays high while memory is capped.

**Stability cost:** none. The cache is purely a read-through optimisation;
the persisted embedding on `node.embedding` (JSON in the graph) is the
durable copy.

### B2. File-content cache for `build_enriched_text`

Where: `src/server/tools/handlers/search.rs`, `metrics.rs`, `query.rs`,
`semantic.rs` — every on-demand embedding path that misses the cache.

When an embedding isn't cached, the tool path calls `build_enriched_text`,
which reads the source file off disk to compose the embedding input. Across
a single query that hits multiple nodes in the same file, this is N reads
of the same file.

**Fix:** a per-tool-executor `Arc<Mutex<HashMap<PathBuf, (SystemTime, String)>>>`
keyed on path, valued on `(mtime, content)`. Read-through with mtime
invalidation: if the mtime changed since the cache entry, re-read.

**Effect:** removes redundant disk I/O from the on-demand embedding path.
For `semantic_search` over a cluster of nodes in the same file this is the
difference between N reads and 1.

**Stability cost:** none. mtime is already used by `freshness()` for a
different purpose; the cache is a direct read-through.

### B3. blake3 content hash for `process_change`

Where: `src/server/ingest/ingestion.rs:973-1078` (the watcher path).

Every modify event calls `process_change` which calls LSP. The watcher fires
on every write, including the common `editor save` that doesn't actually
change the bytes (e.g., `touch`). A blake3 hash check on the file content
short-circuits no-op writes.

**Fix:** at the top of `process_change`, hash the file (blake3 is already
a dependency, `Cargo.toml:63`). If the hash matches a per-path `OnceCell`
or a small `Mutex<HashMap<PathBuf, [u8;32]>>`, return early.

**Effect:** removes the LSP round-trip on no-op writes. Common for
editor saves that touch the file but don't change it.

**Stability cost:** very low. The hash check is the only new step; it
cannot mutate the graph.

### B4. Short-lived (per-scan) LSP response cache

Where: `src/server/ingest/scan.rs:56-328` (`scan_file_structure`).

During a single indexing pass, two files in the same module can both ask
the LSP for `get_document_symbols_hierarchical` on the same path (e.g.,
header files referenced by both `.c` and `.h`). The LSP result is
identical for identical inputs in a single pass.

**Fix:** a per-scan `Arc<Mutex<HashMap<(PathBuf, [u8;32]), HierarchicalSymbol>>>`
keyed on `(path, content_hash)`. Cleared between passes.

**Effect:** marginal on most codebases (the LSP usually isn't called twice
on the same path) but a real win in mixed-language repos with shared
header files.

**Stability cost:** low. The cache is local to a single scan; clearing it
between passes is a single drop.

### B5. Compiled tree-sitter query cache (replaces A2 if you prefer one big PR)

Same as A2, just labelled here to make the trade-off explicit: A2 is the
"while you're there" change; B5 is the "in a dedicated cache module"
change. Both are correct; A2 lives in `treesitter.rs`, B5 would be a new
small module. Either is fine.

---

## Band C: structural rework — proceed with care

### C1. Tree-sitter parser per rayon worker

Where: `src/server/treesitter.rs:12-14`.

```rust
thread_local! {
    static PARSER: Mutex<Parser> = Mutex::new(Parser::new());
}
```

The thread-local is correct for the current code path (single-threaded
extraction per file). It also means: if you put tree-sitter extraction on
a rayon thread pool with N workers, each worker gets its own `Parser`, and
the mutex serialises them anyway because they all touch `set_language`.

The fix is structural: when extracting concurrently across files, give each
worker its own pre-configured `Parser` so `set_language` is set once per
worker and not on every call. The pattern is:

```rust
thread_local! {
    static RUST_PARSER: RefCell<Parser> = {
        let mut p = Parser::new();
        p.set_language(&tree_sitter_rust::language()).unwrap();
        RefCell::new(p)
    };
}
```

…one thread-local per language, configured once at first use. This is
*not* a `Lazy<Mutex<HashMap<Language, Parser>>>` — it has to be
thread-local because `Parser` is `!Send`.

This pairs with the rayon integration in A2 (query compilation) and any
future parallelism in `scan_file_batch`.

**Effect:** enables real tree-sitter parallelism in any rayon-backed
extraction. Today, extraction is serial within a batch anyway, so this
is a *prerequisite* for any parallelism gain in the tree-sitter path —
not a win on its own.

**Stability cost:** medium. `tree_sitter::Parser` is `!Send`; getting
this wrong is a `Send` error. Tests must cover concurrent extraction
on a real rayon pool.

### C2. Per-file parallelism inside `scan_file_batch`

Where: `src/server/ingest/scan.rs:330-358`.

```rust
pub async fn scan_file_batch(
    paths: Vec<PathBuf>, ...
) -> Vec<Result<FileScanResult, LainError>> {
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let result = scan_file_structure(...).await;
        results.push(result);
    }
    results
}
```

The cross-batch fanout is parallel via `JoinSet` in `build_core_memory`
(default `files_per_batch = 50`). Inside a batch of 50, processing is
strictly serial — each file acquires the LSP `AsyncMutex`, awaits the
LSP round-trip, etc.

For the tree-sitter-only fallback path (no LSP), the batch is pure CPU
and could go through rayon. For the LSP path, the mutex already serialises
inside a multiplexer; per-multiplexer parallelism is real, but adding
more multiplexers is a tuning change (already exists — `lsp_pool_size`).

**Effect:** a real win for repos where LSP is unavailable (CI,
no rust-analyzer). Marginal for repos with LSP available — the LSP
bottleneck dominates.

**Stability cost:** medium. The graph mutations from per-file results
happen *after* the batch completes (the `Reduce` phase in
`build_core_memory`), so there's no write-lock contention to worry about.
The risk is in error propagation: a partial-failure in a parallel batch
needs the same "scanned/failed" accounting as the serial version.

### C3. Read-side query indexes (type, label)

Where: `src/server/graph/mod.rs:957-1003` (`query_nodes`).

The query executor's main filter walks `node_weights()` once and applies
type/name/label/path filters. For a corpus of 100 k nodes, this is 100 k
closures per query.

A type index (`DashMap<NodeType, Vec<NodeIndex>>`) and a label index
(`DashMap<String, Vec<NodeIndex>>`) maintained at the same write-lock
sites as `path_index`/`name_index` would short-circuit the type and
label filters. Name and path filters would still need a scan, but the
type/label short-circuit alone cuts the scan to a fraction.

**Effect:** depends on selectivity. For `query_nodes(type=Function)`,
the speedup is roughly `1 / P(Function)` where P is the proportion of
Functions in the graph. For typical repos that is 2-5×.

**Stability cost:** medium. Same invariant story as A3, but with more
mutation sites. The existing `replace_nodes_for_paths` tests give a good
base; new tests must cover all four indexes (id, path, name, type).

### C4. File-watcher parallelism

Where: `src/server/watcher.rs:233-237`.

```rust
for path in &batch {
    if let Err(e) = process_file(&server, path).await {
        warn!("FileWatcher: failed to process {:?}: {}", path, e);
    }
}
```

The batch is processed serially. `process_file` calls `process_change`
which makes an LSP round-trip. With multiple files in a batch (typical
for a multi-file save), they could be processed concurrently via
`futures::future::join_all` or a small `JoinSet`.

**Effect:** marginal. The LSP `AsyncMutex` is a multiplexer — concurrent
attempts on the same multiplexer serialise. The win is for batches that
span multiple multiplexers, which the pool already round-robins across.

**Stability cost:** low to medium. The overlay mutation pattern is
already concurrent-safe (it uses `Arc<RwLock<...>>` and `parking_lot`),
so concurrent overlay inserts are fine. The risk is in error ordering:
the loop currently fails-fast-and-logs; concurrent processing needs to
aggregate errors.

---

## Caching patterns that are NOT worth it

- **Caching the `get_all_nodes()` Vec.** This is called from the resolve
  phase and is genuinely O(N) — but the right answer is the indexes in
  A3 / C3, not a memoised Vec. A memoised Vec would have to be
  invalidated on every insert; the indexes do that work for free.
- **Caching git operations.** `git2` already caches the repo handle
  internally; the only repeated work is the libgit2 walk itself, which
  is sequential by nature.
- **Persisting the embedding cache to disk.** The on-disk graph already
  carries `node.embedding` (JSON). The in-memory cache is just an
  accelerator for `serde_json::from_str` on that field — persisting it
  would duplicate state and risk drift.
- **Caching `tree_sitter::Parser::parse` results.** The parser is fast;
  the dominant cost is the query walk that follows. A parse cache would
  only help if the same content were parsed twice in one pass, which the
  watcher hash check (B3) already prevents.

---

## What I would not touch (stability boundary)

These areas were deliberately not included in any band:

- **`presence.rs` / `presence_lock.rs` / `state_lock.rs`** — recent
  commits (#199, #200, #202) explicitly hardened these against
  cross-process drift. The AGENTS.md note in `federation/` says "be
  conservative: prefer reading through a lock to taking a snapshot."
  Adding a cache here is exactly the kind of change that introduces the
  drift those fixes were written to prevent.
- **`GraphDatabase` write-side lock semantics.** `replace_nodes_for_paths`
  already has extensive comments documenting why index updates must happen
  *inside* the graph write lock. Any parallelism win that touches the
  write path has to preserve those invariants.
- **The cancel-token contract.** Every long-running phase observes a
  `CancellationToken`. Any new parallel structure must thread the token
  through and abort promptly on cancel — `offthread`'s `JoinHandle::abort`
  path is the pattern.
- **`tuning.toml` knobs without production readers.** The
  `every_tuning_knob_is_read_by_production_code` test rejects knobs
  that aren't read; new knobs from B1 must land with readers in the
  same change.
- **The federation's `CrossRepoResolver` path.** Federation is "the only
  place in Lain where per-process and cross-process state can disagree."
  Caching across processes is the wrong layer; caching within a single
  process is fine but should mirror what the owner does.
- **The ONNX session itself.** `nlp.rs` already documents that 4
  intra-op threads is the sweet spot for bge-small/bge-base; more
  threads doesn't help and burns CPU. Don't change `with_intra_threads`.

---

## Suggested ordering for implementation

If we proceed, the natural order — each step independently shippable and
testable, each preserving stability:

1. **A2** (tree-sitter query `OnceLock`) — zero-risk, mechanical, ~10% on
   tree-sitter path.
2. **A4** (`find_node_by_path` via `path_index`) — trivial, zero-risk.
3. **B1** (bounded LRU for `EmbeddingCache`) — bounded memory, requires
   wiring a new `tuning.toml` knob.
4. **A3** (`name_index` on `GraphDatabase`) — biggest single correctness
   win for the resolve phase. Touches the mutation paths; needs the
   existing graph test surface behind it.
5. **A1** (NLP prewarm batches) — biggest single NLP speedup; touches
   the background task only.
6. **A5** (parallel resolve phases) — rayon `par_iter` over the three
   resolve functions. Pairs naturally with A3.
7. **A6** (split `calculate_anchor_scores` into compute + write) —
   passes the write lock briefly; the read-only compute phase is the
   parallel one.
8. **C1** (per-worker tree-sitter parser) — prerequisite for any future
   per-file parallelism in `scan_file_batch`.
9. **B2 / B3 / B4** (file content / hash / LSP caches) — additive,
   independent of each other.
10. **C2** (parallel `scan_file_batch` for tree-sitter fallback).
11. **C3** (read-side query indexes).

Steps 1-7 are all "Band A" — small, additive, with strong test coverage
already in place. They are the ones to do first.

---

## Stability evidence

A short audit of how existing changes preserved stability, to anchor the
ordering:

- **PR #199, #200, #202 (presence / federation):** all recent stability
  work routes through cooperative cancel and fail-closed defaults. None
  of the Band A changes touch that surface.
- **`offthread` migration (PR B + E of the M4 design):** every sync work
  path now lives on the blocking pool. The Band A changes stay within
  that contract — A1 reduces blocking-pool calls, doesn't add new ones.
- **The tuning-knob reachability test:** new tuning knobs from B1 must
  land with production readers in the same change. Easy to honour.
- **The federation AGENTS.md note:** "be conservative; prefer reading
  through a lock to taking a snapshot." No Band A change takes a
  snapshot anywhere.

In short: the parallelism and caching wins identified here do not require
weakening any of the stability contracts the recent hardening work put
in place. They strengthen them — fewer per-call round-trips (A1), fewer
duplicate disk reads (B2), fewer duplicate LSP calls (B4).
