# Startup Remediation and Optimization Design

## Intent and success criteria

Reduce cold startup on the measured 993-file Lain checkout to full static readiness in at most 10 seconds without dropping analyses, changing graph results, or hiding work behind a different readiness definition. Prevent MCP clients from silently attaching to stale shared daemons, and reduce startup CPU and allocation costs without creating a memory problem.

Full static readiness means the repository reports `ready` after all currently registered static sensors complete. LSP readiness is measured separately. The target does not permit moving `field_access` or another existing static analysis out of the readiness contract.

Success means:

- three release-mode cold runs, each with a fresh temporary data directory and no competing indexers, all reach full static readiness in at most 10 seconds;
- `field_access` takes at most 4 seconds and the remaining startup work takes at most 5 seconds, leaving 1 second of margin;
- canonical graph output and per-operation lineage are unchanged;
- peak RSS is no more than 10% above the measured pre-change baseline, and analysis caches are released after their sensor pass;
- a client never silently bridges to a daemon whose binary or indexing identity differs from its own;
- no benchmark processes, temporary graphs, logs, or worktrees remain after verification.

## Measured findings and boundaries

All measurements used release binaries, fresh temporary state, and `LAIN_DISABLE_LSP=1` to isolate static indexing.

| Revision or stage | Result |
|---|---:|
| Current `ea91ef8c`, two cold runs | 30.45s and 30.54s to full readiness |
| Current HTTP availability | 1.22s |
| Current `grpc_consumer` | 1.93s |
| Current `field_access` | 22.26s |
| Parent `8121b716` | Not ready within 180s; stalled in `grpc_consumer` |
| Pre-branch `c7e79c07` `field_access`, with gRPC neutralized | 21.897s |
| Parent `8121b716` `field_access`, with gRPC neutralized | 21.909s |

The newest gRPC change is therefore an improvement, not the regression. Its per-file indexes replace repeated whole-AST receiver scans. It must not be reverted.

The dominant remaining cost predates this branch. The newer per-call lineage behavior increased emissions from 2,518 to 2,753 in the isolated comparison without materially changing `field_access` runtime. A regression theory based only on the new per-call behavior is not supported by the measurements.

The active daemons inspected during the investigation were running cached Lain 0.12.0 (`bdcaecc8`), not the checkout's 0.13.0 (`ea91ef8c`). They are an operational source of stale behavior and competing CPU load, not evidence that the newest source is slow.

## Approaches considered

### Reuse first — selected

Preserve readiness and graph semantics while removing repeated analysis, graph lookups, string copies, and batch-update overhead. Use parallelism only as a separately reviewed fallback. This gives the best expected improvement with bounded memory and correctness risk.

### Defer heavy lineage — rejected for this target

Marking core graph tools ready before `field_access` completes would improve perceived startup but would violate the requirement for full static readiness within 10 seconds. It is not an acceptable way to meet this design's target.

### Parallelize immediately — deferred

Parallel sensor or emission analysis could reduce wall time but would duplicate caches under the current thread-local workspace cache, increase RSS, and complicate deterministic graph mutation. It is justified only if the selected reuse work misses the target, and then under a separate bounded design with indexed output collection and memory limits.

## Operational remediation

Revalidate the identities and PIDs of the old clients and daemons before stopping anything. Perform a controlled restart onto the remediated binary, then start one shared daemon for the intended workspace set. Do not kill processes based only on an earlier PID listing.

Verify the running daemon through `lain/daemon_info` and the fresh binary through `lain doctor --json`. Record `version`, `build_commit`, and `index_build_id`. Confirm that no old indexer remains active before taking benchmark measurements.

This operational restart is separate from the code changes. It must not be performed implicitly by a feature PR or by an investigation agent.

## Daemon identity and compatibility

`lain/daemon_info` already emits `version`. Extend its result and the client's `DaemonInfo` with:

- `build_commit`, using the running binary's `GIT_SHA`;
- `index_build_id`, using the running binary's `INDEX_BUILD_ID`.

Before bridging, the client compares all three identities with its own. Matching behavior remains unchanged. Missing or mismatched fields produce an actionable error containing the expected and actual values and the daemon socket path.

An incompatible daemon must not be silently attached, and the client must not automatically start another resource-heavy indexer beside it. The operator may retry after restarting the daemon or explicitly request a private instance with `--no-share` / `LAIN_MCP_NO_SHARE=1`. Older clients can ignore the additional response fields; older daemons missing them fail the new client's compatibility check rather than being trusted implicitly.

Tests cover exact matches, each mismatch, missing legacy fields, unavailable discovery, and the explicit no-share path. Documentation explains the failure and recovery behavior without introducing a version bump in a feature PR.

## `FieldAccessAnalysis` architecture

Introduce one transient analysis context for a `field_access` pass. It owns all reuse for that pass and is dropped before the sensor returns.

### Single graph view

Take one node snapshot and one edge snapshot. Build function-by-id, caller/callee, operation-call, and path indexes from those snapshots instead of repeatedly calling `get_all_nodes()` and `all_edges()`.

Build an enclosing-symbol index per referenced path. `build_emission` currently calls `enclosing_symbol` for every read, which clones every node in that path on each lookup. Reusing the path's function spans removes those repeated clones while retaining the existing innermost-function and file-fallback rules.

### File-level analysis cache

Retain the existing parsed-file cache, keyed by repository-relative path, and extend each entry with:

- source language and shared source content;
- parsed tree;
- function definitions;
- per-file `FunctionMetadata`;
- function lookup by graph identity and source location.

`ensure_file` remains responsible for lazy loading and the existing unsupported/unreadable behavior. Per-file definitions and metadata are computed once. Scope-specific metadata merges preserve the current sender-first and deterministic member ordering.

### Scope-analysis cache

Cache completed interprocedural analysis by `(sender_id, response-binding kind)`. The binding kinds are:

- HTTP response binding, currently used for `FieldReadOrigin::FieldAccess`;
- first-parameter payload binding, currently used for topic and GraphQL consumers.

The specific `ContractKey`, `call_id`, and `FieldReadOrigin` do not otherwise affect the scope walk, so they are deliberately excluded from the cache key. HTTP and payload analyses on the same sender are never shared with each other. Function names are never used as cache identities.

The cached value contains only `reads`, `escapes`, and the derived `reads_complete`. Each emission retains its own `call_id`, `origin`, `operation`, and consumer path. Attaching the shared reads to an emission still creates operation-specific `FieldRead` facts and `ReadsFrom` edges.

Cache scope membership and sender lookup where they are independent of the emission. Preserve S-only behavior, missing/unloadable members, deterministic member order, the 16-pass ceiling, and the existing cycle fallback. If readiness scheduling is optimized separately, use precomputed caller counts and a deterministic queue rather than repeatedly scanning all scope IDs; this change requires the same output tests and is not assumed to be the primary hotspot.

The cache is valid only within the current sensor pass. It is not persisted and is never shared across graph mutations, repositories, or index builds.

### Emission and mutation

Keep the existing single `replace_sensor_output` call. Collect incomplete-consumer node updates and apply them through `upsert_nodes_batch` instead of one `upsert_node` per emission.

Do not deduplicate `FieldRef` nodes or edges as part of this work. Operation identity is attached during emission, and any apparent duplication must be handled by the existing graph semantics rather than a new performance-only rule.

Add stage timings and counters before changing behavior: graph collection, local detection, scope analysis, metadata construction, emission, consumer patching, and output replacement. Counters include scope analyses by binding kind, cache hits, file-metadata builds, path-index builds, and graph snapshots.

## Secondary optimizations

### Shared source contents

The workspace cache stores `Arc<str>` but `read_cached_or_fs` converts it back to a new `String` for every sensor. Add a shared-content scan API that yields `Arc<str>` while retaining the current API for unmigrated callers. Migrate measured hot paths incrementally, beginning with `field_access`, Rust gRPC, HTTP client, HTTP, and dynamic dispatch.

The existing 2 MiB cacheability limit and missing-file semantics remain unchanged. Cached paths and content remain scoped to one `run_all_with_reports` pass. `field_access::ensure_file` should reuse shared source content rather than adding another full-file string copy.

### Lazy Rust gRPC indexes

Keep `ea91ef8c`'s compiled-config reuse. Build the per-file syntax/type/binding indexes only after the query yields a usable call/receiver/method match that passes the deny-list. Files without a candidate avoid the indexing cost while producing the same candidates.

Do not add source-text pre-filters that could exclude valid generated-client evidence.

### Measurement-guided small changes

After the larger changes, use the per-stage data to decide whether the remaining costs justify:

- deterministic topological scheduling for scope members;
- fewer path-buffer and node-name allocations;
- other sensors that still exceed their share of the 5-second non-`field_access` budget.

Each is a separate measured change. No dependency is added for these optimizations.

## Error handling and compatibility

Graph schemas, sensor registration names, operation facts, source sites, and MCP readiness semantics remain unchanged. Dynamic payloads, ambiguous bindings, unreadable files, and incomplete lineage retain their current unresolved or escape behavior.

Daemon identity failures are explicit and actionable. They never fall back to stale data or silently create a competing indexer. Explicit no-share behavior is unchanged.

No release version is bumped, no federation schema migration is introduced, and no persistent analysis format is added. If an implementation change unexpectedly alters serialized graph data, stop and apply the federation schema policy rather than silently migrating.

## Verification

### Correctness

Write failing focused tests before each change:

- several HTTP calls on one sender reuse one analysis but retain distinct `ReadsFrom` targets;
- several payload operations on one sender reuse one analysis but retain distinct `operation` facts and unresolved records;
- HTTP and payload operations on one sender use separate analyses;
- different sender IDs with the same function name never share results;
- sender-only scopes, missing files, dynamic keys, escapes, shadowing, cycles, and the 16-pass fallback preserve current behavior;
- metadata merge order and innermost enclosing-symbol selection are unchanged;
- canonical sorted fixture nodes and edges match before and after optimization;
- a canonical real-checkout graph hash and operation counts remain unchanged;
- daemon identity match, mismatch, missing fields, and no-share behavior are covered;
- normal and no-match gRPC scans return the same candidates as before lazy indexing.

Run the existing field-lineage, gRPC, shared-daemon, graph, sensor-idempotency, schema, and performance-budget suites. Finish with formatting, clippy, the full relevant Rust suite, `scripts/demo.sh`, and the normal `dev` CI checks.

### Performance

Use a release binary on the measured checkout. Each run gets a fresh temporary data directory, no embedding model, and `LAIN_DISABLE_LSP=1`. Stop unrelated indexers before measuring. Record wall time, process CPU time, every sensor's elapsed time, cache counters, and peak RSS.

Run three cold starts and require every run to reach full static readiness in at most 10 seconds. `field_access` must take at most 4 seconds and the other work at most 5 seconds. Peak RSS must remain within 110% of the baseline measured on the same host.

Also measure a warm restart and one incremental edit to detect stale-cache behavior. Record default-LSP startup separately and verify that MCP initialize and tools/list remain responsive while background work runs.

Benchmark after every implementation step. Keep a change only when it improves the target without output or memory regressions. If the target remains unmet, stop and review bounded parallelism or persistent content-addressed caching as new designs rather than expanding this implementation silently.

## Delivery sequence

1. Add benchmark instrumentation and correctness baselines.
2. Land daemon identity validation independently.
3. Add scope and file-metadata reuse with focused tests.
4. Consolidate graph snapshots, path indexes, and consumer batching.
5. Add shared-content scanning and lazy gRPC indexes as separate measured changes.
6. Re-run the complete correctness and performance gates and perform the controlled daemon rollout.

Use small feature branches and PRs targeting `dev`. Do not target `main` directly or bump release metadata in feature PRs. Update user-facing daemon documentation and the changelog when the compatibility behavior lands.

## Assumptions and non-goals

- The approved target is full static readiness in 10 seconds, not merely a responsive MCP handshake.
- All current analyses and graph semantics are preserved.
- Reuse before parallelism is the selected approach.
- No persistent cache, new dependency, broad sensor refactor, or parallel scheduler is included in the initial implementation.
- Default-LSP latency is a separate measurement and is not hidden by the static benchmark.
- Implementation waits for review of this written spec and the subsequent implementation plan.
