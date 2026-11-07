# NVIDIA startup benchmark

The completed cache implementation and its before/after measurements are documented in [STARTUP_CACHE.md](STARTUP_CACHE.md). The results below retain the original baseline.

Build the release executables, then run the benchmark against existing NVIDIA clones:

```bash
cargo build --release --bin lain --bin lain-git-sidecar
python3 scripts/eval/nvidia-startup-bench.py \
  /data/agents/tmp-lain-tests/nvidia-eval \
  --output /tmp/nvidia-startup.json
```

The default performs three cold/warm pairs for each federation:

| Federation | Repositories |
| --- | --- |
| openshell | OpenShell, NeMo-Fabric, NemoClaw |
| nat | NeMo-Agent-Toolkit, NeMo-Agent-Toolkit-UI, NeMo-Agent-Toolkit-Examples |

Every cold run uses an empty temporary Lain data directory. Its warm run restarts with that same directory. The OS page cache remains enabled. Repositories are reused without fetching or changing their source. The JSON records their commits, tracked file counts, and tracked dirty state, plus the executable and Git sidecar SHA-256 hashes, embedded build stamp, checkout commit, and machine information. Git uses the matching sidecar beside the selected executable.

The default disables LSP and uses one repo indexer at a time. Use `--lsp` for a separate measurement with normal language servers, or `--concurrency 3` to measure concurrent indexing. Results from different modes should be compared separately. These configurations include repository discovery and cross-repo linking; they do not include the service declarations used by the protocol-specific smoke tests.

For each process launch, the harness records time to first HTTP index-status response, first successful MCP `tools/list`, and full federation readiness. Readiness requires the exact expected repo roster with every repo `ready`, followed by a healthy response with nonempty nodes and edges. Degraded, unavailable, incomplete, crashed, and timed-out runs fail the benchmark. Median/min/max readiness times summarize successful runs; the raw runs retain failures.

The readiness probe runs approximately every 100 ms, plus HTTP latency, so results have that sampling granularity. Peak RSS is sampled for the server parent and excludes sidecars and LSP processes. Timeout defaults to 900 seconds per process. Graphs, logs, and owned process groups are removed after each pair; failure log tails remain in the JSON evidence.

Run the harness checks without building Rust:

```bash
PYTHONDONTWRITEBYTECODE=1 python3 tests/e2e/test_nvidia_startup_bench.py
```

The measured evidence is stored in [nvidia-startup-2026-11-06.json](benchmarks/nvidia-startup-2026-11-06.json). The embedded build stamp in this run is `dfd10bf7-dirty`, retained by Cargo’s build-script cache, while the rebuilt source checkout is `4b877781fdca8ef6ec1690693b12dabfa729202e`. Use the recorded binary hashes to identify these executables. This is a shared-host measurement, with host load recorded per run.

## Measured results — 2026-11-06

Release build, LSP disabled, one repo indexer, three cold/warm pairs per federation. All 12 process launches passed full-readiness checks. The six harness tests also pass.

| Federation | Phase | Median readiness | Range | Median HTTP availability | Peak sampled parent RSS |
| --- | --- | ---: | ---: | ---: | ---: |
| openshell | cold | 276.2 s | 272.5–285.1 s | 1.44 s | 2139.3 MiB |
| openshell | warm | 165.2 s | 163.5–169.2 s | 8.27 s | 1145.0 MiB |
| nat | cold | 40.8 s | 39.4–44.1 s | 0.51 s | 847.9 MiB |
| nat | warm | 23.9 s | 23.7–28.2 s | 1.11 s | 484.4 MiB |

Graph-count findings: every OpenShell run had 101,118 nodes, but cold-run edge counts varied (318,362; 321,212; 318,027), and each warm run added 157 edges relative to its preceding cold run. NeMo Agent Toolkit retained 25,026 nodes, with 67,842 edges on every cold run and 67,846 on every warm run. These measurements establish readiness latency and nonempty output, not canonical graph equivalence. Edge-count variation needs investigation before using this workload to prove equivalent analyzer output.

The first OpenShell cold run’s logs showed a 62.1-second NemoClaw ORM scan, a 6.3-second field-access scan, and a 99.8-second complete local NemoClaw pass. Its full federation readiness was 276.2 seconds. Warm readiness remained around 165 seconds despite reused per-repo indexes, showing substantial startup work after local cache reuse.
