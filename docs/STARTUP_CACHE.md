# Completed federation startup cache

Lain can reuse a completed live federation on restart. The same data directory must be retained between launches. Completion follows the server's existing readiness contract, with optional analyzer availability reported separately. A cold startup publishes `startup-complete.json` before reporting full readiness; a valid warm startup loads the saved graphs and reconstructs process-local lookup tables without rerunning projection, peer matching, or cross-repo linking. It restores environment bindings and CODEOWNERS attribution alongside those tables. Source watchers remain active after a cache hit.

Build development executables with `cargo build --release --bin lain --bin lain-git-sidecar`; use `./target/release/lain` below when that build is not installed on PATH.

## NVIDIA results — 2026-11-06

Both measurements use the same six repository revisions, release builds, disabled LSP, one repo indexer, and three cold/warm pairs per federation. These are shared-host measurements; the OS page cache remains enabled. The new run also fingerprints the graph before readiness, so its timings include that verification work.

| Federation | Phase | Previous median | New median | Time reduction | New range |
| --- | --- | ---: | ---: | ---: | ---: |
| OpenShell | cold | 276.19 s | 94.93 s | 65.6% | 93.28–95.87 s |
| OpenShell | warm | 165.23 s | 5.35 s | 96.8% | 5.26–5.57 s |
| NeMo Agent Toolkit | cold | 40.85 s | 19.03 s | 53.4% | 18.92–19.52 s |
| NeMo Agent Toolkit | warm | 23.88 s | 1.21 s | 94.9% | 1.21–1.52 s |

All 12 launches passed readiness and MCP checks. All six warm launches confirmed completed-cache hits. Each federation produced an identical normalized graph fingerprint across its six launches: OpenShell retained 101,118 nodes and 318,168 edges; NeMo Agent Toolkit retained 25,026 nodes and 67,845 edges. The baseline's varying edge counts do not establish equivalent old/new graph content; the new fingerprint checks establish repeatability across fresh indexes and cache restores, alongside the correctness regression tests.

Median HTTP availability was 1.01 s cold / 4.95 s warm for OpenShell and 0.40 s / 1.11 s for NeMo Agent Toolkit. Warm loading validates the checkpoint before opening the listener. Maximum sampled parent RSS changed from 1,145 to 759 MiB for warm OpenShell and 484 to 307 MiB for warm NeMo Agent Toolkit. Cold peaks were 2,139 → 2,099 MiB and 848 → 894 MiB respectively. These samples exclude sidecars and language servers.

Raw evidence: [baseline JSON](benchmarks/nvidia-startup-2026-11-06.json) and [cache implementation JSON](benchmarks/nvidia-startup-cache-2026-11-06.json). The reports include executable hashes, repository revisions, per-launch load, counts, and timings; the new report also includes fingerprints and cache-hit evidence.

Further cold-start work should target the remaining measured sensors. In the first new NemoClaw pass, ORM took 13.3 s, frontend values 10.6 s, HTTP clients 9.8 s, and field access 6.2 s. Its total local pass took 55.9 s, compared with 99.8 s in the original first run. Concurrent indexing is available through the existing configuration and benchmark flag, but these measurements keep concurrency at one to preserve the comparison.

To prewarm, start the normal server and wait for `/health/index-status` to return HTTP 200. That endpoint reports readiness only after indexing, linking, the contract join, and checkpoint publication have been attempted. Publication errors are logged; ordinary startup can still serve its completed in-memory graph.

```bash
lain server --config repos.yaml --transport http
```

To require a prepared cache on subsequent launches:

```bash
LAIN_REQUIRE_STARTUP_CACHE=1 lain server --config repos.yaml --transport http
```

This mode refuses a cache miss with a prewarming instruction. It does not silently perform a cold index. Stop the first server before starting another against its mutable data directory; multiple agents should use Lain's existing shared daemon.

## Validation and invalidation

The completion record contains fingerprints rather than another copy of the graph. Reuse validates:

- the Lain index build identity and federation schema version;
- the resolved configuration, effective run parameters, selected repo set, and source locations;
- each repo's HEAD, actual tracked file contents, `.lain/patterns` overrides, and `.lain/tuning.toml`;
- dotenv and deployment files discovered by the environment sensor, including ignored files;
- LSP enable/disable, lean, test-disable, and prewarm settings; relevant language-server availability, executable contents, package metadata, Rust toolchain component metadata, and environment selection;
- persisted federation bytes, per-repo graph files, WALs, and build stamps;
- per-repo call-graph publication stamps and analyzer identity.

Each repo also records its analysis context. A context mismatch forces local analysis even when HEAD is unchanged; an interrupted or dirty pass invalidates that stamp.

Only clean worktrees are eligible for completed-federation reuse. Dirty, staged, or untracked changes cause a cache miss; dirty startup runs the indexing path instead of trusting an unchanged HEAD. Ignored pattern and tuning overrides are explicitly fingerprinted. Changes observed between initial validation and indexing completion prevent publication. A valid read does not consume or rewrite the completion record. Invalid records are removed before fallback work; malformed or missing records cannot certify readiness.

The record is versioned JSON written through a temporary file and rename. Existing graph serialization is unchanged, so no federation schema migration is introduced. Normal analyzer build changes still invalidate prior analysis.

## Sharing extracted references

`<data_dir>/static-ref-cache/` stores immutable reference-extraction artifacts keyed by analyzer identity, relative source paths, source bytes, overrides, and environment analysis files. Identical source trees can share an artifact across repos in that federation, even when their repo names and namespaces differ. Each artifact has a payload checksum. Resolved graph nodes and cross-repo edges retain their own repo identities.

This cache has a 64 MiB disk budget and evicts least recently used entries. Missing, evicted, oversized, or corrupt entries trigger normal extraction. It keeps no process-wide collection of parsed trees. Independent data directories have independent caches; completed mutable graphs are not shared across concurrently running servers. Lain's revision-pinned snapshot cache and shared daemon remain separate reuse mechanisms.

## Cold-start changes

Initial repo passes finish before federation projection and cross-repo resolution begin. This removes dependence on task completion order. Startup local passes leave cross-repo resolution to the barrier pass; persisted source references let an unchanged repo reuse the same extraction when another part of the federation changes.

Relinking replaces syntax-derived cross-repo `Calls` and `Uses` without repeating peer matching. Obsolete derived references are retracted; confirmed and runtime evidence retain their separate ownership. Changed repo membership prunes unselected nodes from the serving graph while keeping reusable per-repo disk indexes. Live refresh projects changed symbols before resolving references from other repos.

Within each repo projection, identical signatures share their peer-match calculation. The TypeScript ORM scan compiles Drizzle and TypeORM queries once per scan and parses each source file once for both detectors, preserving per-repo query overrides.

Service-reference extraction counts line numbers incrementally as URLs appear. The former implementation rescanned the whole prefix for every URL. NVIDIA's large trace artifacts caused about 205 GB of repeated prefix scanning in the OpenShell federation; the new pass scans each prefix once and preserves reference locations.

## Concurrent indexing and warm-cache profile — 2026-11-06

The follow-up experiment uses the same instrumented release binary for both settings, with LSP disabled and the same repository revisions. It alternates whole-federation cold/warm batches in the order 1, 2, 2, 1, 1, 2 indexers, giving three pairs per federation per setting. Each pair owns a fresh temporary data directory. The reports record invocation order and host load. Use this experiment's serial measurements as its comparison baseline.

| Federation | Cold, 1 indexer | Cold, 2 indexers | Cold time reduction | Warm, 1 indexer | Warm, 2 indexers |
| --- | ---: | ---: | ---: | ---: | ---: |
| OpenShell | 89.84 s | 81.91 s | 8.8% | 3.72 s | 3.62 s |
| NeMo Agent Toolkit | 17.15 s | 15.97 s | 6.9% | 1.11 s | 1.11 s |

All 24 launches passed readiness, MCP, timing-output, and graph-fingerprint checks. All 12 warm launches hit the completed cache. Every graph also matched the previously validated serial graph fingerprint. OpenShell cold ranges were 89.75–94.10 s with one indexer and 81.31–82.08 s with two; Toolkit ranges were 17.12–17.97 s and 15.79–16.22 s. Warm OpenShell ranges overlap (3.72–3.73 s and 3.62–3.93 s), so these samples do not establish a useful warm-start improvement.

| Federation | Cold peak parent RSS, 1 → 2 | Warm peak parent RSS, 1 → 2 |
| --- | ---: | ---: |
| OpenShell | 2,097 → 2,175 MiB | 757 → 773 MiB |
| NeMo Agent Toolkit | 917 → 1,067 MiB | 288 → 326 MiB |

Two indexers trade higher peak memory for a modest cold-start reduction. The current configuration default is eight; this experiment explicitly covers one and two and leaves the default unchanged. A three-repo federation can have at most three active repo passes, so evaluating the default would require another measurement at that effective concurrency.

The opt-in timings identify the remaining warm work. Values below are medians across the three serial warm launches; parent spans include their children.

| Warm phase | OpenShell | NeMo Agent Toolkit |
| --- | ---: | ---: |
| Federation graph open, including validation and load | 0.426 s | 0.097 s |
| Per-repo loading | 0.374 s | 0.138 s |
| Cache input validation | 0.247 s | 0.087 s |
| Persisted output fingerprints | 0.049 s | 0.012 s |
| Metadata restoration, including contract reconstruction | 2.154 s | 0.608 s |
| Final input recheck | 0.237 s | 0.084 s |

Within metadata restoration, contract-index reconstruction took 1.823 s for OpenShell and 0.049 s for Toolkit; service-reference scanning took 0.290 s and 0.551 s. These are the next candidates for metadata checkpoint reuse. Contract reconstruction includes semantic resolution and client/environment input construction, so further subdivision would distinguish its internal costs. Federation payload validation alone took only 0.128 s and 0.026 s. Those phase durations identify work available to optimize; they do not predict the net savings after adding metadata persistence and validation.

Raw evidence: [one indexer](benchmarks/nvidia-startup-concurrency-1-2026-11-06.json), [two indexers](benchmarks/nvidia-startup-concurrency-2-2026-11-06.json). Temporary graphs, logs, and owned process groups were removed after each pair.

## Functional NVIDIA checks

Both existing real-repo contract smoke tests passed against the same release binary and source revisions, with static analysis and explicit service/provider declarations. Every repo was ready in both federations. These checks exercise query answers beyond the startup benchmark's health, tool discovery, and graph-repeatability checks.

- OpenShell exposed 120 RPC contracts with 21 bound consumers. The cross-repo impact query for `parse_sandbox_policy` included NeMo-Fabric. Source inspection confirmed two calls to that OpenShell function in NeMo-Fabric's OpenShell environment provider. Impact results include indirect paths; their counts do not measure exact direct-call accuracy.
- NeMo Agent Toolkit exposed 12 HTTP endpoints with one bound consumer. Both declared chat endpoints appeared, the Next.js `/api/mcp/clients` route appeared, no malformed `ANY` routes appeared, and the UI proxy was attributed to the `nat` service with an explicit unresolved route.

Toolkit reported 43 unresolved consumers: 30 `no_match`, nine `no_route_in_service`, two `graphql_no_op`, and two `env_unmapped`. Those gaps need individual review before claiming complete coverage. These targeted scenarios do not establish corpus-wide precision or recall, and the real-repo checks did not enable LSP.

The subsequent [source correctness audit](NAT_CORRECTNESS_AUDIT.md) reviewed all 43 baseline entries. It found that the sole HTTP binding was test-only and six of twelve HTTP providers came from tests. It also confirmed SQL-to-GraphQL false positives, a bogus WebSocket provider, and missed literal FastAPI route definitions. The original smoke established its narrow assertions and no production HTTP binding in that configured Toolkit view.

Analyzer revision 9 fixes those three detector defects. The final Toolkit run discovered 22 HTTP endpoints, including both MCP keyword registrations; the false GraphQL consumers and `/async` provider disappeared. Formatted and escaped paths are conservatively excluded from the new keyword query. All three known MySQL consumer identities were preserved. HTTP, WebSocket, and unresolved results matched after strict warm-cache restoration. [Correctness-fix evidence](benchmarks/nvidia-correctness-fixes-2026-11-07.json) records the final binary, assertions, source scopes, and the intermediate formatted-path issue caught during verification.

There are still 41 unresolved consumers and zero demonstrated non-test-source HTTP caller bindings in this scenario. The smoke now reports source scope explicitly. Set `NAT_REQUIRE_NON_TEST_HTTP_BINDINGS=1` to make it fail when only fixture traffic binds; non-test source alone still does not establish runtime deployment. URL-expression recovery and candidate-protocol filtering remain open in the audit.

The first Toolkit run exposed command substitution in comments inside the test's generated YAML heredoc. Removing the backticks fixed the harness; its clean rerun passed the same functional expectations. [Functional evidence](benchmarks/nvidia-functional-correctness-2026-11-06.json) records the current runs, initial noisy attempt, script hashes, and repository revisions. Temporary graphs and isolated state were removed after the tests.

## Verification

The final correctness-fix tree passed the full workspace test run: 4,122 tests across 113 suites, with 16 ignored and no failures. The Python benchmark harness passed nine tests, and the new source-scope harness passed three tests. Regression coverage includes unchanged restarts, dirty inputs, ignored environment and pattern overrides, configuration changes, corrupt certificates and graph outputs, analyzer identity, shared extraction with isolated repo namespaces, stale derived-edge removal, confirmed-edge preservation, repo removal, and restoration of environment and ownership metadata. Detector regressions also cover SQL/GraphQL isolation, callback-provider retraction, FastAPI keyword ordering and multiple methods, incomplete registrations, and formatted-path rejection. Architecture checks, formatting, and `git diff --check` passed.

The earlier targeted `cargo clippy --lib --bins --test federation_startup_cache` pass reported 26 existing warnings in production code. The final CI-configured `cargo clippy --workspace --all-targets` passed with `-D warnings` and the workflow's existing style, complexity, performance, pedantic, unused, and dead-code allowances.

The broad Linux follow-up also passed:

- `scripts/demo.sh --quick`: 171 capability checks, zero failures, two semantic-search checks skipped because the isolated environment had no embedding model. It exercised 96 of 97 advertised tools, exempting the machine-mutating language-server installer. [Final capability evidence](benchmarks/startup-capability-c9-2026-11-07.json) records every assertion.
- Live schema comparison: all 100 tools exactly matched `docs/tool-schema.json`.
- All four CI architecture guard scripts, npm installer and launcher tests, and three Windows archive assembly contract tests.

The capability demo used temporary fixtures and isolated XDG state, configuration, and caches, then removed its owned process group and data. These local checks ran on Linux; native macOS and Windows execution was outside this run.

```bash
cargo test --test federation_startup_cache --test orm_models
python3 tests/e2e/test_nvidia_startup_bench.py
python3 scripts/eval/nvidia-startup-bench.py \
  /path/to/nvidia-eval --digest \
  --output /tmp/nvidia-startup-cache.json
```

`--digest` requires the instrumented binary. It includes a graph fingerprint pass in measured startup and verifies that normalized node and complete edge content agree across every cold/warm run in each federation. Sync timestamps, hydration flags, and embeddings are excluded. Fingerprinting sorts compact per-record hashes instead of cloning the entire graph. This strengthens the original benchmark's readiness and nonempty-graph checks.

Use `--timings` alongside `--digest` to capture startup phase durations in each run's `startup_timings_ms`. Outside the benchmark, set `LAIN_STARTUP_TIMING=1` to log those phases. Timings cover federation payload validation/write/load, global ID indexing, repo loading, source and graph fingerprints, metadata restoration, and the final input recheck. Repeated per-repo spans remain separate samples. Parent durations include their child spans, so do not add every reported value together. A benchmark requested with timings fails if the binary does not report its loader timing.

To measure two concurrent repo indexers with those diagnostics:

```bash
python3 scripts/eval/nvidia-startup-bench.py \
  /path/to/nvidia-eval --concurrency 2 --digest --timings \
  --output /tmp/nvidia-startup-concurrency-2.json
```

With startup timing enabled, the instrumentation additionally passed 804 federation unit tests (three ignored) and all 14 startup-cache tests.

The original measurements and workload details are in [NVIDIA_STARTUP_BENCHMARK.md](NVIDIA_STARTUP_BENCHMARK.md).

## Verification rerun — 2026-11-07

The current working tree passed `cargo test --locked --workspace -j 2`: 4,133 tests passed across 113 suites, 16 ignored, and none failed. This run included the real Zoanthrope coordination tests and the unresolved-origin alias regression. The CI-configured workspace Clippy check, formatting, live 100-tool schema comparison, mutation-score floor, health-badge tests, all four architecture guard scripts and self-tests, and `git diff --check` also passed.

The focused mutation gate for `src/server/federation/startup_cache.rs` tested all 72 generated mutants against the library suite: 69 were caught and three were unviable; none were missed or timed out. This was a path-filtered batch, not a mutation run over every changed Rust file. The JS suite passed 78 tests; npm-shim installer and launcher tests, three Windows archive contract tests, nine startup benchmark harness tests, and three contract-smoke scope tests passed. The npm-shim install hook emitted a recoverable model-install warning from the older globally cached binary; the package install and its tests exited successfully.

These checks ran on Linux. Native macOS and Windows execution require CI runners. The release coverage command was attempted locally but stopped before completion when its separate target grew to 26 GB and filled the 280 GB volume; that generated target and its isolated temporary files were removed, restoring the prior 26 GB free. Coverage remains outstanding for CI. Existing same-day NVIDIA correctness and capability evidence above remains separate from this test run.
