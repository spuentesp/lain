# OpenShell gRPC Contract Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Lain see OpenShell's gRPC surface as a live contract graph — the `proto/*.proto` providers joined to the Python SDK's client call sites — so the OpenShell workspace shows real RPC contracts and NeMo-Fabric's Rust provider becomes a cross-repo dependent of that surface.

**Architecture:** The provider half already works: `grpc_provider_sensor` parses every `.proto` into `ContractFact::RpcProvider` facts (measured: 161 proto facts, 120 provider nodes for `openshell.v1.OpenShell` and friends). The consumer half does not fire on OpenShell's SDKs, for two independent reasons: the Python SDK's receiver is `self._stub` / `stub` (the sensor requires a receiver ending in `Stub`), and the channel address is a constructor parameter (the sensor requires a literal or an env-get local). Fixing those two seams in one sensor, plus splitting the repo into two services with `services[].paths`, lets the *existing* resolver's package-qualified fallback join `OpenShell` → `openshell.v1.OpenShell`. No new `ContractKey`, no new joiner, no federation-graph schema bump.

**Tech Stack:** Rust (stable toolchain per `rust-toolchain.toml`), `tree-sitter` 0.25, `serde_yaml` 0.9, `inventory` for tool registration, `cargo test` (unit tests in-module + `tests/` integration tests).

**Spec:** `docs/protocol-coverage.md` (§rpc — the resolver, the binding grammar, the coverage matrix), `docs/REPOS_YAML.md` (§services, §bindings), `docs/FEDERATION.md` (federation graph + schema gate).

## Implementation status

Implemented on `feat/openshell-rpc-contract` (7 commits, not pushed as of 2026-10-29). Measured against the real NVIDIA clones with `scripts/eval/openshell-contract-smoke.sh`: **120 RPC contracts, 21 with a bound consumer, both repos `ready`** (Lain `4b6cafad`).

Where the implementation deliberately departs from this plan, the task below carries an **"Amended after implementation"** note:

- **Task 3** — the channel fallback no longer refuses on two or more env reads; it takes the single *address-shaped* name (`GATEWAY|HOST|URL|ADDR|ENDPOINT|SERVER`) and refuses an ambiguous pair. The bind it produces is service-level, not a verified dial address.
- **Task 1** — the per-repo budget is tested through a pure `parse_index_timeout(Option<&str>)` helper (the `OnceLock` makes the env-var form untestable), and the concurrency assertion is an upper bound rather than a timing-dependent lower bound.
- **Task 4** — the test uses the real `add_repo(source, &data_dir)` / `source.fetch()` APIs.

Two items from the plan's Review Focus were not implemented, both for reasons Claude recorded on the branch: **#3** (the same file under two services) cannot occur because overlapping `paths` are rejected at config load, and **#6 as originally written** (refuse on two env reads) was replaced by the rule above.

## Measured baseline

Measured on 2026-10-29 with `lain 0.13.0` (commit `0355a95b`, built from `origin/dev`) against shallow clones of `NVIDIA/OpenShell`, `NVIDIA/NemoClaw`, `NVIDIA/NeMo-Fabric`. Reproduce with `scripts/eval/openshell-contract-smoke.sh` (created by Task 6).

| Measurement | Value | Where it came from |
|---|---|---|
| OpenShell index time (single repo) | 128 s | `lain server` log: `fully indexed in 128.29s` |
| OpenShell sensor counts | `http_routes: 321, proto: 161, websocket: 4, http_clients: 12, dynamic_dispatch: 571, entry_points: 63, sql_tables: 4` | `protocol sensors contributed SensorCounts` |
| `grpc_provider` nodes emitted | 120 | `sensor finished (slow) sensor="grpc_provider" ... emitted=120` |
| NeMo-Fabric index time (single repo) | 29.7 s | `fully indexed in 29.73s` |
| RPC consumer facts in OpenShell | **0** | no `grpc_consumer` sensor line with `emitted>0`; the sensor's language filter is `go|py|java|cc|cpp|h|hpp` and its Python receiver filter is `rcv.ends_with("Stub")` |
| RPC consumer facts in NeMo-Fabric | **0** | `proto: 0` in its `SensorCounts` |
| Cross-repo code dependency | **real** | `NeMo-Fabric/environment-providers/openshell/Cargo.toml`: `openshell-sdk = { git = "https://github.com/NVIDIA/OpenShell", rev = "…" }`; `NeMo-Fabric/environment-providers/openshell/src/lib.rs:17`: `use openshell_sdk::raw::proto;` |
| Manifest dependency gate | `nemo-fabric → openshell` = true | `declared_identifiers` (`src/server/federation/federated_index.rs:1685`) yields `["openshell"]`; the git URL contains the token |
| Manifest dependency gate | `nemoclaw → openshell` = true **by accident** | the match is the word "OpenShell" in `NemoClaw/package.json`'s *description*; the real `"@nvidia/openshell-sdk"` dependency is missed because `contains_token` treats `-` as part of a name (`federated_index.rs:1731`) |
| 3-repo federation index | **does not complete** | the startup index loop (`src/cli/server.rs:336`) is sequential; NeMo-Fabric's rust-analyzer (its Cargo workspace has a git dependency on OpenShell) keeps one tokio worker at 99–123 % CPU on the same runtime, so the loop never reaches the second repo |

### Why the consumer half is dark — the three exact gaps

1. **Receiver shape.** `OpenShell/python/openshell/sandbox.py:730` builds `self._stub = openshell_pb2_grpc.OpenShellStub(_error_mapping_channel(self._channel))` and calls `self._stub.Health(...)` at line 861. `detect_method_calls` (`src/server/sensors/util_tokenize.rs:385`) hands the sensor the identifier run before the dot — `_stub` — and `detect_python_stub_calls` requires `rcv.ends_with("Stub")` (`grpc_consumer_sensor.rs:347`). `_stub` fails. So does the bare `stub` in `OpenShell/e2e/python/test_security_tls.py:157`.
2. **Service name.** Even when the receiver matches, the service name has to come from somewhere. `self._stub` carries none; the name lives in the constructor call (`openshell_pb2_grpc.OpenShellStub` → `OpenShell`). The sensor never looks at the assignment site.
3. **Channel address.** `sandbox.py:710` calls `grpc.insecure_channel(endpoint)` where `endpoint` is a parameter of `SandboxClient.__init__` (`sandbox.py:662`). The sensor's channel detection accepts a string literal (`detect_channel_host`) or a bare identifier bound from an env-get (`detect_channel_env`, `grpc_consumer_sensor.rs:217`). A parameter is neither → `HostPart::None` → `RpcStubUnknown`. The file *does* read the gateway from the environment — `sandbox.py:2557`: `env_gateway = os.environ.get("OPENSHELL_GATEWAY")` — but that local is not the channel argument.

## Global Constraints

- No new `ContractKey` variant. The exhaustive `match` over `ContractKey` in `src/server/federation/contracts/coverage_matrix.rs` must keep compiling unchanged; if a task seems to need a new kind, that is a design change and out of scope for this plan.
- No federation-graph schema bump. `FEDERATION_GRAPH_VERSION` in `src/server/federation/graph_backend.rs` stays as it is. If a task needs a new on-disk field, stop and follow the schema-bump procedure in `AGENTS.md`.
- Ship on `dev`; the PR targets `dev`. Never target `main`. Do not bump any version in this plan.
- `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` must pass on the branch.
- The contract tool schemas under `src/server/mcp/contract_tools/schemas/` are `include_str!`-ed into the binary; after changing any schema, run `make schema` and commit the regenerated `docs/tool-schema.json`.
- Sensor detection changes must keep the existing detection order deterministic: the same input file must produce the same node set, byte-for-byte, across runs.

## Review Focus

The failure modes below are implied by the design but not exercised by any task's tests. Each one gets a test in the task that owns the code.

1. **A Python file with a stub-shaped receiver but no `*Stub(...)` constructor in the file.** The consumer fact must not carry an empty service name — a nameless consumer would enter the unresolved ledger as a phantom. Expected: no consumer fact at all.
2. **The channel resolves to a service that does not own the proto.** The candidate-service filter in `resolve_rpc_consumer` (`src/server/federation/contracts/joiner/consumer_protocol.rs:309-372`) must hold: a consumer must never bind to a provider in a service its channel did not name.
3. **The same SDK file is indexed under two services.** The consumer must bind to exactly one provider, not one per service.
4. **A receiver that is a stub handle but whose method name matches a provider in a non-candidate service.** It must stay unresolved (`RpcStubUnknown`), never bind.
5. **A service whose `paths` prefix matches no file in the repo.** The service must not silently claim the whole repo — a service with `paths: ["proto/"]` must not be treated as owning `python/openshell/sandbox.py`.
6. **A file that reads more than one environment variable.** The channel fallback (Task 3) takes the single env read when the file has one. With several, it takes the only *address-shaped* one (`GATEWAY`, `HOST`, `URL`, `ADDR`, `ENDPOINT`, `SERVER`) and refuses — leaving the consumer `HostPart::None` — when zero or two-or-more are address-shaped. It must never pick between two address-shaped candidates.

   > **Amended after implementation (2026-10-29).** The original rule here was "refuse on two or more env reads". That left the real repository dark: `OpenShell/python/openshell/sandbox.py` reads `XDG_CONFIG_HOME` *and* `OPENSHELL_GATEWAY`, so the consumer never resolved. The shipped rule prefers the single address-shaped name and still refuses an ambiguous pair. The bind it produces is **service-level** — `OPENSHELL_GATEWAY` names a cluster, and the dial address comes from a metadata file — so the edge says "this SDK talks to the service that owns the gateway", not "this SDK dials this address". The code documents that caveat.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src/server/sensors/grpc_consumer_sensor.rs` | modify | Detect Python stub calls through member/lowercase receivers; recover the service name from the stub constructor; resolve the channel from a file-level env read |
| `src/server/sensors/grpc_consumer_sensor.rs` (tests) | modify | Unit tests for each new detection rule |
| `src/server/federation/repo_index.rs` | modify | Split the per-repo index budget from the startup re-index budget |
| `src/cli/server.rs` | modify | Index repos concurrently under `max_concurrent_indexers`; run the cross-repo "glue" pass after the barrier |
| `src/server/federation/federated_index.rs` | modify | `declared_identifiers` also honours declared published package names |
| `src/server/federation/config.rs` | modify | `RepoConfig.packages: Vec<String>` — the package names a repo publishes |
| `docs/REPOS_YAML.md` | modify | Document `packages:` and the two-service split |
| `tests/federation_index_budget.rs` | create | Integration tests for the index budget and the concurrency limit |
| `tests/federation_rpc_e2e.rs` | create | End-to-end contract test: proto provider + SDK consumer across two services |
| `scripts/eval/openshell-contract-smoke.sh` | create | Reproducible smoke run over the real NVIDIA workspaces |

---

### Task 1: Indexing foundation — an independent per-repo budget, concurrent repos, a barrier before the glue pass

The measurement above shows the 3-repo federation never finishes indexing: the startup loop is sequential, and one repo's rust-analyzer work monopolises the tokio runtime the loop runs on. This task makes the index pass bounded, concurrent, and observable. It is the prerequisite for every other task in this plan (and for Plan B).

**Files:**
- Modify: `src/server/federation/repo_index.rs:35-49` (`index_timeout`)
- Modify: `src/server/federation/federated_index.rs` (add `index_all`, the concurrency counters, and the stored limit)
- Modify: `src/server/federation/loader.rs:56` (store `max_concurrent_indexers` on the index)
- Modify: `src/cli/server.rs:312-375` (`index_federation` delegates to `index_all`)
- Test: `tests/federation_index_budget.rs` (create)

**Interfaces:**
- Consumes: `FederatedIndex::set_contract_config`, `RepoIndex::index()` (`src/server/federation/repo_index.rs:561`), `max_concurrent_indexers` from `FederationConfig` (`src/server/federation/config.rs:16`).
- Produces:
  - `pub fn index_timeout() -> Duration` — now reads `LAIN_INDEX_TIMEOUT` only (default `INDEX_TIMEOUT`, 60 s). `LAIN_REINDEX_TIMEOUT` stops affecting it.
  - `pub async fn index_all(self: &Arc<Self>)` on `FederatedIndex` — indexes every repo concurrently, bounded, and returns at the barrier.
  - `pub fn observed_index_concurrency(&self) -> usize` — the peak number of repo index passes that ran at the same time.
  - `pub fn enter_index_pass(self: &Arc<Self>) -> IndexPassGuard` — the guard that records the peak.
  - `pub async fn index_federation(fed: Arc<FederatedIndex>)` — unchanged signature; now delegates the index pass to `index_all` and keeps the glue pass.

- [ ] **Step 1: Write the failing test for the independent budget**

Create `tests/federation_index_budget.rs`:

```rust
//! The per-repo index budget must be independent of the startup
//! re-index budget, and a repo that blows its budget must degrade
//! instead of blocking the federation.

use lain::federation::config::SourceConfig;
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use std::path::Path;
use std::sync::Arc;

fn git_init_committed(dir: &Path) {
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .status()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c", "user.email=t@example.com", "-c", "user.name=t",
            "commit", "-q", "-m", "init",
        ])
        .current_dir(dir)
        .status()
        .unwrap();
}

#[test]
fn index_timeout_defaults_to_the_per_repo_budget() {
    assert_eq!(
        lain::server::federation::repo_index::parse_index_timeout(None),
        std::time::Duration::from_secs(60)
    );
}

#[test]
fn index_timeout_honors_its_own_value() {
    assert_eq!(
        lain::server::federation::repo_index::parse_index_timeout(Some("7")),
        std::time::Duration::from_secs(7)
    );
}

#[test]
fn index_timeout_falls_back_on_garbage() {
    assert_eq!(
        lain::server::federation::repo_index::parse_index_timeout(Some("soon")),
        std::time::Duration::from_secs(60)
    );
}
```

> **Amended after implementation (2026-10-29).** The test above originally set
> `LAIN_INDEX_TIMEOUT` in the environment and read `index_timeout()`. That cannot
> work: `index_timeout()` caches its answer in a `OnceLock`, so the second
> assertion in one process reads the first one's value — and the env var is
> process-global, so parallel tests would race. The shipped shape splits the
> parsing into a pure function and tests that:
>
> ```rust
> /// The per-repo budget for a raw `LAIN_INDEX_TIMEOUT` value. Unset or
> /// unparseable values fall back to [`INDEX_TIMEOUT`].
> fn parse_index_timeout(raw: Option<&str>) -> Duration {
>     match raw {
>         None => INDEX_TIMEOUT,
>         Some(s) => match s.parse::<u64>() {
>             Ok(secs) => Duration::from_secs(secs),
>             Err(_) => {
>                 eprintln!(
>                     "LAIN_INDEX_TIMEOUT={s:?} is not a valid integer; using default 60s for per-repo pipeline"
>                 );
>                 INDEX_TIMEOUT
>             }
>         },
>     }
> }
> ```
>
> with `index_timeout()` reading the env var once and delegating to it.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test federation_index_budget`
Expected: FAIL — `LAIN_REINDEX_TIMEOUT=3000` currently makes `index_timeout()` return 3000 s, not 60 s.

- [ ] **Step 3: Split the knobs**

In `src/server/federation/repo_index.rs`, replace the body of `index_timeout`:

```rust
/// Resolved per-repo pipeline timeout. Honors `LAIN_INDEX_TIMEOUT`
/// (this knob is per repo) and never `LAIN_REINDEX_TIMEOUT` (that
/// one is the *startup* budget in `crate::server::refresh`).
/// Raising the startup budget must not give a single pathological
/// repo an unbounded window — the two budgets answer different
/// questions.
pub fn index_timeout() -> Duration {
    static OVERRIDE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| match std::env::var("LAIN_INDEX_TIMEOUT") {
        Ok(s) => match s.parse::<u64>() {
            Ok(secs) => Duration::from_secs(secs),
            Err(_) => {
                tracing::warn!(
                    "LAIN_INDEX_TIMEOUT={s:?} is not a valid integer; using default 60s for per-repo pipeline"
                );
                INDEX_TIMEOUT
            }
        },
        Err(_) => INDEX_TIMEOUT,
    })
}
```

In `src/server/reload.rs` / `src/server/refresh/mod.rs`, `parse_reindex_timeout` keeps reading `LAIN_REINDEX_TIMEOUT` — no change needed there. Update the doc comment on `parse_reindex_timeout` to say it is the *startup* budget only.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --test federation_index_budget`
Expected: PASS.

- [ ] **Step 5: Write the failing test for concurrent indexing**

Add to `tests/federation_index_budget.rs`:

```rust
/// The peak number of index passes running at the same time never
/// exceeds the configured limit. This is an *upper* bound, so it is
/// deterministic: two repos may or may not overlap in time, but the
/// semaphore must never let more than `limit` through.
#[tokio::test]
async fn index_all_never_runs_more_passes_than_the_limit() {
    let root = tempfile::tempdir().unwrap();
    let fed = federation_of(root.path(), &["a", "b"]).await;
    fed.index_all().await;
    for repo in fed.list_repos() {
        let repo = fed.get_repo(&repo.0).unwrap();
        assert_ne!(repo.health(), RepoHealth::Degraded, "every repo should index");
    }
    assert!(
        fed.observed_index_concurrency() <= fed.max_concurrent_indexers(),
        "peak {} exceeded the limit {}",
        fed.observed_index_concurrency(),
        fed.max_concurrent_indexers()
    );
}

/// The pass guard records the peak and releases on drop.
#[tokio::test]
async fn the_pass_guard_records_the_peak_and_releases_on_drop() {
    let root = tempfile::tempdir().unwrap();
    let fed = federation_of(root.path(), &["a"]).await;
    {
        let _guard = fed.enter_index_pass();
        let _second = fed.enter_index_pass();
        assert_eq!(fed.observed_index_concurrency(), 2);
    }
    assert_eq!(fed.current_index_concurrency(), 0);
}
```

> **Amended after implementation (2026-10-29).** The original assertion here was
> "the peak *reaches* 2" — a lower bound that needs both repos to overlap in
> time. That is a timing assertion and it would flake on a loaded machine. The
> shipped test asserts the deterministic direction instead: the peak never
> *exceeds* the limit, every repo still indexes, and the pass guard records the
> peak and releases on drop. The lower bound ("the repos really do overlap")
> is covered by the real-workspace smoke run in Task 6, where two repos index
> concurrently and both reach `ready`.

- [ ] **Step 6: Run the test to verify it fails**

Run: `cargo test --test federation_index_budget repos_index_concurrently`
Expected: FAIL — `observed_index_concurrency` and `index_all` do not exist, and the loop is sequential.

- [ ] **Step 7: Make the index loop concurrent with a barrier before the glue pass**

In `src/server/federation/federated_index.rs`, add the counter:

```rust
/// Peak number of repo index passes observed running at the same
/// time. Observable so the concurrency limit is testable without
/// timing assertions.
index_concurrency_peak: AtomicUsize,
index_concurrency_now: AtomicUsize,
```

with `pub fn observed_index_concurrency(&self) -> usize { self.index_concurrency_peak.load(Ordering::SeqCst) }` and a private helper that bumps the "now" counter, records the peak, and decrements on drop.

Move the index pass into the federation as a public method, so it is callable
from a test and from the server alike. In
`src/server/federation/federated_index.rs`:

```rust
    /// Index every repo concurrently, bounded by
    /// `max_concurrent_indexers`, then project each one. The
    /// sequential version let one pathological repo (a Rust workspace
    /// whose language server monopolises the runtime) starve every
    /// other repo behind it.
    ///
    /// The caller runs the cross-repo "glue" pass afterwards — the
    /// barrier is here, at the end of this method.
    pub async fn index_all(self: &Arc<Self>) {
        let limit = self.max_concurrent_indexers().max(1);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(limit));
        let mut handles = Vec::new();
        for (id, _) in self.list_repos() {
            let Some(repo) = self.get_repo(&id) else { continue };
            let fed = self.clone();
            let sem = semaphore.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore");
                tracing::info!("lain server: indexing repo '{}'", id.as_str());
                let guard = fed.enter_index_pass();
                if let Err(e) = repo.index().await {
                    tracing::warn!(
                        "lain server: indexing repo '{}' failed: {e} (marking Degraded)",
                        id.as_str()
                    );
                    repo.set_health(RepoHealth::Degraded);
                    return;
                }
                drop(guard);
                if let Err(e) = fed.project_repo(&id).await {
                    tracing::warn!(
                        "lain server: project_repo for '{}' after indexing failed: {e}",
                        id.as_str()
                    );
                }
                if let Err(e) = repo.start_watcher().await {
                    tracing::warn!(
                        "lain server: could not watch repo '{}' for re-index: {e}",
                        id.as_str()
                    );
                }
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        // Barrier: every repo is indexed (or degraded) before the glue
        // pass resolves calls that cross a repo boundary.
    }
```

`FederatedIndex` does not retain `max_concurrent_indexers` today — the
`FederationConfig` value is read at load time (`src/server/federation/loader.rs:56`).
Store it on the index (a `max_concurrent_indexers: usize` field, set in the
loader next to `set_contract_config`) and add:

```rust
    /// The configured concurrency limit for repo index passes.
    pub fn max_concurrent_indexers(&self) -> usize {
        self.max_concurrent_indexers
    }
```

Then in `src/cli/server.rs`, replace the sequential body of `index_federation`
with a call to `fed.index_all().await`, keeping the `hold_ready` handling and
the glue pass exactly where they are:

```rust
    // Index every repo (concurrently, bounded), then link calls
    // across repos.
    fed.index_all().await;
```

`enter_index_pass` is the guard that bumps the "now" counter and records the
peak; it returns a guard that decrements on drop:

```rust
    /// Mark the start of a repo index pass. Returns a guard that
    /// decrements the live counter on drop, so the peak is observed
    /// without a timing assertion.
    pub fn enter_index_pass(self: &Arc<Self>) -> IndexPassGuard {
        let now = self.index_concurrency_now.fetch_add(1, Ordering::SeqCst) + 1;
        self.index_concurrency_peak.fetch_max(now, Ordering::SeqCst);
        IndexPassGuard { fed: self.clone() }
    }
```

with

```rust
/// Decrements the live index-pass counter when dropped.
pub struct IndexPassGuard {
    fed: Arc<FederatedIndex>,
}

impl Drop for IndexPassGuard {
    fn drop(&mut self) {
        self.fed.index_concurrency_now.fetch_sub(1, Ordering::SeqCst);
    }
}
```

- [ ] **Step 8: Run the test to verify it passes**

Run: `cargo test --test federation_index_budget`
Expected: PASS.

- [ ] **Step 9: Run the full suite and the linter**

Run: `cargo test --lib && cargo test --test federation_contracts_e2e && cargo clippy --all-targets -- -D warnings`
Expected: all green.

- [ ] **Step 10: Commit**

```bash
git add src/server/federation/repo_index.rs src/server/reload.rs src/server/refresh/mod.rs \
        src/server/federation/federated_index.rs src/cli/server.rs tests/federation_index_budget.rs
git commit -m "fix(index): per-repo index budget independent of startup budget; index repos concurrently"
```

---

### Task 2: Detect Python gRPC stub calls through a member receiver

**Files:**
- Modify: `src/server/sensors/grpc_consumer_sensor.rs:339-367` (`detect_python_stub_calls`)
- Modify: `src/server/sensors/grpc_consumer_sensor.rs` (new helpers)
- Test: `src/server/sensors/grpc_consumer_sensor.rs` (tests module)

**Interfaces:**
- Consumes: `detect_method_calls` (`src/server/sensors/util_tokenize.rs:385`), `strip_stub_suffix` (`grpc_consumer_sensor.rs:472`), `GrpcStubCall` (`grpc_consumer_sensor.rs:57`).
- Produces: `fn python_stub_services(content: &str) -> BTreeMap<String, String>` — receiver variable → service name; `fn is_python_stub_receiver(rcv: &str) -> bool`.

- [ ] **Step 1: Write the failing test**

Add to the tests module in `src/server/sensors/grpc_consumer_sensor.rs`:

```rust
    /// OpenShell's Python SDK keeps the stub on the instance
    /// (`self._stub = openshell_pb2_grpc.OpenShellStub(...)`) and
    /// calls through it (`self._stub.Health(...)`). The service
    /// name lives in the constructor, not the receiver.
    #[test]
    fn python_member_receiver_stub_call_is_detected() {
        let src = "\
import grpc
import openshell_pb2_grpc

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)

    def health(self):
        return self._stub.Health(openshell_pb2.HealthRequest())
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].service, "OpenShell");
        assert_eq!(calls[0].method, "Health");
    }

    /// The lowercase receiver shape used by OpenShell's e2e tests.
    #[test]
    fn python_lowercase_stub_receiver_is_detected() {
        let src = "\
import grpc
import openshell_pb2_grpc

channel = grpc.insecure_channel(\"orders:50051\")
stub = openshell_pb2_grpc.OpenShellStub(channel)
resp = stub.GetCurrentUser(request)
";
        let calls = detect_stub_calls(src, "py", "client.py");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].service, "OpenShell");
        assert_eq!(calls[0].method, "GetCurrentUser");
        assert_eq!(calls[0].channel_target.as_deref(), Some("orders"));
    }

    /// A stub-shaped receiver with no constructor in the file has no
    /// service name; emitting a nameless consumer would put a
    /// phantom into the unresolved ledger.
    #[test]
    fn python_stub_receiver_without_constructor_emits_nothing() {
        let src = "\
class Client:
    def health(self):
        return self._stub.Health(request)
";
        let calls = detect_stub_calls(src, "py", "client.py");
        assert!(calls.is_empty());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib grpc_consumer_sensor`
Expected: FAIL — `calls.len()` is 0 (the receiver `_stub` / `stub` does not end with `Stub`).

- [ ] **Step 3: Implement the receiver rules**

In `src/server/sensors/grpc_consumer_sensor.rs`, add:

```rust
/// True when the receiver names a gRPC stub handle.
///
/// `detect_method_calls` hands us the identifier run immediately
/// before the dot, so the three Python shapes arrive as:
///   - `ordersStub`  → generated client, ends with `Stub`
///   - `stub`        → lowercase local handle
///   - `_stub`       → an attribute (`self._stub`); the `self.`
///     part is not part of the identifier run
fn is_python_stub_receiver(rcv: &str) -> bool {
    let last = rcv.rsplit('.').next().unwrap_or(rcv).trim_start_matches('_');
    last.eq_ignore_ascii_case("stub") || last.to_ascii_lowercase().ends_with("stub")
}

/// The service name a stub handle was built from, keyed by the
/// variable that holds it.
///
/// `self._stub = openshell_pb2_grpc.OpenShellStub(channel)` →
///   `_stub` → `OpenShell`
/// `stub = openshell_pb2_grpc.OpenShellStub(channel)` →
///   `stub` → `OpenShell`
///
/// The constructor is the only place the service name appears: the
/// receiver is a handle, not a name.
fn python_stub_services(content: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in content.lines() {
        let Some(eq) = line.find('=') else { continue };
        let lhs = line[..eq].trim();
        let rhs = line[eq + 1..].trim();
        // Skip `==`, `!=`, `>=`, `<=`.
        if rhs.starts_with('=') {
            continue;
        }
        let Some(ctor) = python_stub_constructor_service(rhs) else {
            continue;
        };
        // `self._stub` → `_stub`; `stub` → `stub`.
        let name = lhs.rsplit('.').next().unwrap_or(lhs).trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        out.insert(name.to_string(), ctor);
    }
    out
}

/// `openshell_pb2_grpc.OpenShellStub(channel)` → `OpenShell`.
fn python_stub_constructor_service(rhs: &str) -> Option<String> {
    let open = rhs.find('(')?;
    let head = rhs[..open].trim();
    let last = head.rsplit('.').next()?.trim();
    if !last.to_ascii_lowercase().ends_with("stub") {
        return None;
    }
    let service = last
        .trim_end_matches("Stub")
        .trim_end_matches("stub")
        .trim_end_matches("STUB")
        .to_string();
    if service.is_empty() {
        None
    } else {
        Some(service)
    }
}
```

- [ ] **Step 4: Wire the rules into `detect_python_stub_calls`**

Replace the body of `detect_python_stub_calls`:

```rust
fn detect_python_stub_calls(
    content: &str,
    graph_path: &str,
    channel: &Option<ChannelTarget>,
    out: &mut Vec<GrpcStubCall>,
) {
    let services = python_stub_services(content);
    for det in crate::server::sensors::util_tokenize::detect_method_calls(
        content,
        |rcv| is_python_stub_receiver(rcv),
        |meth| !meth.is_empty(),
    ) {
        // The service name comes from the stub constructor when the
        // file has one; otherwise fall back to the receiver's own
        // suffix (`ordersStub` → `orders` — `strip_stub_suffix`
        // strips the exact `Stub` suffix and returns a `String`).
        let service = services
            .get(det.receiver)
            .cloned()
            .unwrap_or_else(|| strip_stub_suffix(det.receiver));
        if service.is_empty() {
            continue;
        }
        let (channel_target, host_part) = host_parts_from_channel(channel);
        out.push(GrpcStubCall {
            service,
            package: String::new(),
            method: det.method.to_string(),
            channel_target,
            channel_host_part: host_part,
            site: SourceSite {
                path: graph_path.to_string(),
                line: det.line_no,
            },
        });
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib grpc_consumer_sensor`
Expected: PASS (all three new tests plus the existing ones).

- [ ] **Step 6: Commit**

```bash
git add src/server/sensors/grpc_consumer_sensor.rs
git commit -m "feat(sensors): detect python gRPC stub calls through member and lowercase receivers"
```

---

### Task 3: Resolve the gRPC channel when the address arrives as a parameter

**Files:**
- Modify: `src/server/sensors/grpc_consumer_sensor.rs:217-246` (`detect_channel_env`), `:181-212` (`collect_env_locals`)
- Test: `src/server/sensors/grpc_consumer_sensor.rs` (tests module)

**Interfaces:**
- Consumes: `crate::server::sensors::util::host_env_name` (`src/server/sensors/util.rs:459`), `ChannelTarget`, `HostPart`.
- Produces: `fn detect_channel_env(content: &str, ext: &str) -> Option<String>` — now also returns the file's single env read when the channel argument is a parameter that is not itself an env-get local.

- [ ] **Step 1: Write the failing test**

```rust
    /// OpenShell's Python SDK takes the gateway address as a
    /// constructor parameter and reads the gateway name from the
    /// environment elsewhere in the file. The channel must resolve
    /// to that env var so `services[].env` can dispatch it.
    #[test]
    fn python_stub_call_resolves_channel_from_file_env() {
        let src = "\
import os
import grpc
import openshell_pb2_grpc

def _resolve_active_cluster() -> str:
    env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")
    return env_gateway

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert_eq!(calls.len(), 1);
        assert!(matches!(&calls[0].channel_host_part,
            HostPart::Env(ref vars) if vars == &["OPENSHELL_GATEWAY".to_string()]));
    }

    /// Two env reads in one file: the channel must not guess.
    #[test]
    fn python_stub_call_with_two_env_reads_stays_unknown() {
        let src = "\
import os
import grpc
import openshell_pb2_grpc

a = os.environ.get(\"OPENSHELL_GATEWAY\")
b = os.environ.get(\"XDG_CONFIG_HOME\")

class SandboxClient:
    def __init__(self, endpoint):
        self._channel = grpc.insecure_channel(endpoint)
        self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)
";
        let calls = detect_stub_calls(src, "py", "sandbox.py");
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0].channel_host_part, HostPart::None));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib grpc_consumer_sensor python_stub_call_resolves_channel_from_file_env`
Expected: FAIL — `channel_host_part` is `HostPart::None`.

- [ ] **Step 3: Implement the fallback**

In `src/server/sensors/grpc_consumer_sensor.rs`, extend `detect_channel_env`:

```rust
fn detect_channel_env(content: &str, ext: &str) -> Option<String> {
    let ctor = match ext {
        "go" => "grpc.NewClient(",
        "py" => "grpc.insecure_channel(",
        "java" => "ManagedChannelBuilder.forAddress(",
        _ => return None,
    };
    let env_locals = collect_env_locals(content);
    for line in content.lines() {
        let trimmed = line.trim();
        let Some(pos) = trimmed.find(ctor) else {
            continue;
        };
        let rest = &trimmed[pos + ctor.len()..];
        if let Some(var) = crate::server::sensors::util::host_env_name(rest) {
            return Some(var.to_string());
        }
        let arg = rest.split([',', ')']).next().unwrap_or("").trim();
        if !arg.is_empty() && arg.chars().all(|c| c.is_alphanumeric() || c == '_') {
            if let Some(var) = env_locals.get(arg) {
                return Some(var.clone());
            }
            // The channel argument is a parameter, not an env-get
            // local (an SDK that takes the gateway address as a
            // constructor argument). Fall back to the file's
            // gateway-shaped env read.
            return sole_file_env_var(&env_locals);
        }
    }
    None
}

/// The env var a file most plausibly uses as its gateway address, for a
/// channel whose address arrives as a constructor parameter. One distinct
/// env var is taken as is. With several, only address-shaped names
/// (`GATEWAY`, `HOST`, `URL`, `ADDR`, `ENDPOINT`, `SERVER`) count, and the
/// one address-shaped name is taken. Two or more address-shaped names
/// stay unresolved: the var names a gateway, which may resolve to an
/// address elsewhere, so the binding is service-level, not a verified
/// dial address.
fn sole_file_env_var(env_locals: &BTreeMap<String, String>) -> Option<String> {
    const ADDRESS_SHAPED: [&str; 6] = ["GATEWAY", "HOST", "URL", "ADDR", "ENDPOINT", "SERVER"];
    let vars: BTreeSet<&String> = env_locals.values().collect();
    if vars.len() == 1 {
        return vars.into_iter().next().cloned();
    }
    let mut address = vars.into_iter().filter(|v| {
        ADDRESS_SHAPED
            .iter()
            .any(|shape| v.to_ascii_uppercase().contains(shape))
    });
    match (address.next(), address.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}
```

> **Amended after implementation (2026-10-29).** The original rule was "the
> file's *only* env read, otherwise refuse". The real SDK reads two
> (`XDG_CONFIG_HOME` and `OPENSHELL_GATEWAY`), so it never resolved. The
> shipped rule takes the single *address-shaped* name and refuses an ambiguous
> pair; the boundary tests are
> `python_stub_call_with_two_address_env_reads_stays_unknown` and
> `python_stub_call_with_two_non_address_env_reads_stays_unknown`.
>
> `collect_env_locals` records only assignment-form env reads (`name = os.getenv("X")`).
> `OpenShell/python/openshell/sandbox.py` uses `env_gateway = os.environ.get("OPENSHELL_GATEWAY")`
> — the same assignment form, so it is already recorded. If `os.environ.get(...)`
> is not matched by `host_env_name` in an assignment context, widen
> `host_env_name`'s grammar rather than special-casing Python.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib grpc_consumer_sensor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/grpc_consumer_sensor.rs
git commit -m "feat(sensors): resolve a gRPC channel passed as a parameter from the file's env read"
```

---

### Task 4: Two services in one repo, and the contract join across them

The proto providers and the SDK consumers live in the *same* repo. `resolve_by_key` (`src/server/federation/contracts/joiner/consumer_protocol.rs:93-110`) skips providers in the consumer's own service, so a single-service OpenShell collapses the edge to `Internal`. `services[].paths` (`docs/REPOS_YAML.md` §services) splits one repo into several services.

**Files:**
- Modify: `src/server/federation/contracts/config.rs` (validation of overlapping `paths` — confirm it exists)
- Test: `tests/federation_rpc_e2e.rs` (create)
- Modify: `docs/REPOS_YAML.md` (add the OpenShell split as a worked example)

**Interfaces:**
- Consumes: `ServiceDecl { name, repo, paths, hosts, env, … }` (`src/server/federation/contracts/config.rs:60`), `build_endpoints` (`joiner/endpoints.rs:57`), `resolve_rpc_consumer` (`joiner/consumer_protocol.rs:300`).
- Produces: a federation-level regression test that pins the RPC join across two services in one repo.

- [ ] **Step 1: Write the failing end-to-end test**

Create `tests/federation_rpc_e2e.rs`:

```rust
//! A gRPC provider (proto) and a gRPC consumer (python SDK) in the
//! same repo, split into two services by `services[].paths`, must
//! produce one `Binds` edge — and the provider's package-qualified
//! fallback must join a bare `OpenShell` to `openshell.v1.OpenShell`.

use lain::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
use lain::federation::contracts::index::ConsumerTarget;
use lain::federation::federated_index::FederatedIndex;
use lain::federation::graph_backend::{GraphBackend, PetgraphBackend};
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use std::path::Path;
use std::sync::Arc;

fn git_init_committed(dir: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["add", "-A"],
        vec!["-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-q", "-m", "init"],
    ] {
        std::process::Command::new("git")
            .args(&args)
            .current_dir(dir)
            .status()
            .unwrap();
    }
}

#[tokio::test]
async fn proto_provider_binds_the_python_sdk_consumer_across_two_services() {
    let root = tempfile::tempdir().unwrap();
    let repo_dir = root.path().join("openshell");
    std::fs::create_dir_all(repo_dir.join("proto")).unwrap();
    std::fs::create_dir_all(repo_dir.join("python/openshell")).unwrap();

    std::fs::write(
        repo_dir.join("proto/openshell.proto"),
        "syntax = \"proto3\";\n\
         package openshell.v1;\n\
         service OpenShell {\n\
         \x20 rpc Health(HealthRequest) returns (HealthResponse);\n\
         }\n\
         message HealthRequest {}\n\
         message HealthResponse {}\n",
    )
    .unwrap();

    std::fs::write(
        repo_dir.join("python/openshell/sandbox.py"),
        "import os\n\
         import grpc\n\
         import openshell_pb2_grpc\n\
         \n\
         env_gateway = os.environ.get(\"OPENSHELL_GATEWAY\")\n\
         \n\
         class SandboxClient:\n\
         \x20   def __init__(self, endpoint):\n\
         \x20       self._channel = grpc.insecure_channel(endpoint)\n\
         \x20       self._stub = openshell_pb2_grpc.OpenShellStub(self._channel)\n\
         \n\
         \x20   def health(self):\n\
         \x20       return self._stub.Health(request)\n",
    )
    .unwrap();

    git_init_committed(&repo_dir);

    let data_dir = root.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let backend: Arc<dyn GraphBackend> =
        Arc::new(PetgraphBackend::new(&data_dir).expect("backend"));
    let fed = Arc::new(FederatedIndex::new(backend));

    let repo_id = RepoId::new("openshell").unwrap();
    let source: Box<dyn RepoSource> = Box::new(
        WorkspaceDirSource::with_config(
            repo_id.clone(),
            repo_dir.clone(),
            lain::federation::config::SourceConfig::WorkspaceDir { path: repo_dir.clone() },
        )
        .unwrap(),
    );
    source.fetch().await.unwrap();
    fed.add_repo(source, &data_dir).await.unwrap();

    fed.set_contract_config(ContractFederationConfig {
        services: vec![
            ServiceDecl {
                name: "openshell-gateway".into(),
                repo: "openshell".into(),
                paths: vec!["proto".into()],
                hosts: vec!["localhost".into()],
                env: vec!["OPENSHELL_GATEWAY".into()],
                base_path: None,
                route_prefixes: vec![],
                provides_graphql: true,
            },
            ServiceDecl {
                name: "openshell-sdk".into(),
                repo: "openshell".into(),
                paths: vec!["python".into()],
                hosts: vec![],
                env: vec![],
                base_path: None,
                route_prefixes: vec![],
                provides_graphql: true,
            },
        ],
        http_clients: vec![],
        generic_keys: vec![],
        schemas: vec![],
        bindings: vec![],
        databases: vec![],
    });

    // Index, then join.
    let repo = fed.get_repo(&repo_id).unwrap();
    repo.index().await.unwrap();
    fed.project_repo(&repo_id).await.unwrap();
    fed.rejoin_contracts_if_dirty().unwrap();

    let index = fed.contract_index().expect("contract index");
    assert!(!index.endpoints.is_empty(), "the contract index has no endpoints");
    // The provider is the proto surface. `build_endpoints` keys an
    // RPC provider under `ContractKey::Rpc { system, service,
    // method }` and records the *method* as the endpoint template
    // (`joiner/endpoints.rs:124-140`), so the endpoint's template is
    // the rpc method name.
    let rpc_endpoints = index
        .endpoints
        .values()
        .filter(|e| e.template == "Health")
        .count();
    assert_eq!(rpc_endpoints, 1, "expected the proto's Health method as an endpoint");

    // The consumer binds to it (not `Internal`, not unresolved).
    // `ContractIndex.consumers` maps a call site to *one resolution
    // per fact* the node carries, so flatten it.
    let resolutions: Vec<&ConsumerResolution> =
        index.consumers.values().flatten().collect();
    assert_eq!(resolutions.len(), 1);
    assert!(
        matches!(resolutions[0].target, Some(ConsumerTarget::Binds { .. })),
        "expected a bind, got {:?}",
        resolutions[0].target
    );
}
```

> `fed.get_repo(&repo_id)` returns `Option<Arc<RepoIndex>>` and `RepoIndex::index`
> takes `&Arc<Self>`, so `repo.index().await` on the `Arc` is correct.
> `fed.rejoin_contracts_if_dirty()` returns `Result<(), LainError>`; use
> `.expect("rejoin")` in the test.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test federation_rpc_e2e`
Expected: FAIL — no consumer fact is emitted (Task 2/3 not implemented yet), so `consumers.len()` is 0.

- [ ] **Step 3: Run the test again after Tasks 2 and 3 are in**

Run: `cargo test --test federation_rpc_e2e`
Expected: PASS — one RPC endpoint, one consumer, `Binds`.

> If the consumer binds to `Internal` instead of `Binds`, the two services are not being applied: check that `paths: ["proto"]` and `paths: ["python"]` are disjoint prefixes of the same repo and that `resolve_service_decl` (`joiner/endpoints.rs:263`) routes the provider and the consumer to different services.

- [ ] **Step 4: Document the split**

Add to `docs/REPOS_YAML.md` (§services) a worked example:

```yaml
services:
  # The gRPC gateway: the .proto surface plus the server crates.
  - name: openshell-gateway
    repo: openshell
    paths: [proto, crates]
    hosts: [localhost, 127.0.0.1]
    env: [OPENSHELL_GATEWAY]
  # The SDKs that call it. A consumer in the gateway's own service
  # never binds (I5) — the SDK needs its own service for the join
  # to happen at all.
  - name: openshell-sdk
    repo: openshell
    paths: [python, sdk]
```

- [ ] **Step 5: Commit**

```bash
git add tests/federation_rpc_e2e.rs docs/REPOS_YAML.md
git commit -m "test(contracts): proto provider binds the python SDK consumer across two services"
```

---

### Task 5: Declare the package names a repo publishes

`repo_depends_on` (`src/server/federation/federated_index.rs:1602`) decides whether two repos may be linked by comparing the target repo's identifiers against the source's manifests. Today the identifiers are the repo id, the directory name, and the `name` field of the *root* `package.json` / `Cargo.toml` / `pyproject.toml`. Two consequences:

- `NeMo-Fabric → OpenShell` matches by luck (the git URL contains the token `openshell`).
- `NemoClaw → OpenShell` matches by accident (the word "OpenShell" in the package.json *description*), while the actual dependency `"@nvidia/openshell-sdk"` is missed: `contains_token` treats `-` as part of a name, so the needle `openshell` does not match inside `openshell-sdk`.

A repo that publishes a package under a name that is not its directory name cannot be linked on purpose. This task makes the link explicit.

**Files:**
- Modify: `src/server/federation/config.rs:145-148` (`RepoConfig`)
- Modify: `src/server/federation/federated_index.rs:1685-1724` (`declared_identifiers`)
- Modify: `docs/REPOS_YAML.md` (§RepoConfig)
- Test: `src/server/federation/federated_index_tests.rs` (create or extend)

**Interfaces:**
- Consumes: `RepoConfig { id, source }` (`src/server/federation/config.rs:145`), `declared_identifiers(repo_id, root)`.
- Produces: `RepoConfig.packages: Vec<String>` — the package names the repo publishes; `declared_identifiers` gains a `packages` parameter.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn declared_package_names_participate_in_the_dependency_gate() {
    // A repo that publishes "@nvidia/openshell-sdk" must be
    // linkable by that name even though its directory is "openshell"
    // and the hyphen defeats the token match.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sdk/typescript")).unwrap();
    std::fs::write(
        dir.path().join("sdk/typescript/package.json"),
        r#"{"name": "@nvidia/openshell-sdk"}"#,
    )
    .unwrap();
    let ids = declared_identifiers_with_packages(
        &RepoId::new("openshell").unwrap(),
        dir.path(),
        &["@nvidia/openshell-sdk".to_string()],
    );
    assert!(ids.iter().any(|i| i == "@nvidia/openshell-sdk"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib declared_package_names_participate_in_the_dependency_gate`
Expected: FAIL — `declared_identifiers_with_packages` does not exist.

- [ ] **Step 3: Implement the field and the plumbing**

In `src/server/federation/config.rs`:

```rust
pub struct RepoConfig {
    pub id: String,
    pub source: SourceConfig,
    /// Package names this repo publishes (npm, PyPI, crates.io, Go
    /// module paths). A consumer repo that depends on one of them is
    /// linked to this repo even when the name does not contain the
    /// repo id — the npm scope and the hyphen in
    /// `@nvidia/openshell-sdk` both defeat the token match.
    #[serde(default)]
    pub packages: Vec<String>,
}
```

In `src/server/federation/federated_index.rs`:

```rust
/// The names another repo would use to depend on `repo`: its id, its
/// directory, the package names it declares, and the module / package
/// name it declares for itself.
fn declared_identifiers_with_packages(
    repo: &RepoId,
    root: &Path,
    packages: &[String],
) -> Vec<String> {
    let mut ids = vec![repo.as_str().to_lowercase()];
    if let Some(dir) = root.file_name() {
        ids.push(dir.to_string_lossy().to_lowercase());
    }
    // Declared package names: the identifiers a consumer's manifest
    // actually mentions. The npm scope and the hyphen in
    // `@nvidia/openshell-sdk` both defeat the token match against the
    // repo id, so the declaration is what makes the link intentional.
    for p in packages {
        let p = p.trim().to_lowercase();
        if !p.is_empty() {
            ids.push(p);
        }
    }
    // … existing go.mod / package.json / Cargo.toml / pyproject.toml reads …
    ids.retain(|i| i.len() >= 2);
    ids.sort();
    ids.dedup();
    ids
}
```

Keep the existing `declared_identifiers(repo, root)` as a wrapper that passes an
empty package list, so the current callers keep compiling, and update the
caller in `repo_depends_on` to pass the target repo's configured `packages`:

```rust
        let packages = self
            .get_repo(target)
            .map(|r| r.config_packages().to_vec())
            .unwrap_or_default();
        let answer = if manifests.is_empty() {
            true
        } else {
            let text = manifests.to_lowercase();
            declared_identifiers_with_packages(target, tgt.source().local_path(), &packages)
                .iter()
                .any(|id| contains_token(&text, id))
        };
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib declared_package_names_participate_in_the_dependency_gate && cargo test --lib federation`
Expected: PASS.

- [ ] **Step 5: Document the field**

Add to `docs/REPOS_YAML.md` (§RepoConfig) a `packages` row and a paragraph:

```yaml
repos:
  - id: openshell
    packages: ["@nvidia/openshell-sdk", "openshell"]
    source:
      type: shallow_clone
      url: https://github.com/NVIDIA/OpenShell.git
```

> The package name is what a consumer's manifest mentions. Declaring it makes the cross-repo link intentional instead of accidental.

- [ ] **Step 6: Commit**

```bash
git add src/server/federation/config.rs src/server/federation/federated_index.rs docs/REPOS_YAML.md
git commit -m "feat(federation): repos declare the package names they publish"
```

---

### Task 6: Reproducible smoke run over the real NVIDIA workspace

The plan is only worth shipping if it produces a visible contract graph on the real repositories. This task ships the script that measures it and records the numbers.

**Files:**
- Create: `scripts/eval/openshell-contract-smoke.sh`
- Modify: `docs/protocol-coverage.md` (a "measured on real repositories" note under §rpc)

**Interfaces:**
- Consumes: `lain server --config <repos.yaml> --transport http --port <port>`, the `contracts` MCP tools over `POST /mcp`.
- Produces: a script that prints a pass/fail line per expectation and exits non-zero on failure.

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# Smoke: OpenShell's proto surface and its Python SDK must show a
# live RPC contract in the federation view.
#
# Usage: scripts/eval/openshell-contract-smoke.sh <workspace-dir> [port]
# <workspace-dir> must contain the NVIDIA clones:
#   OpenShell/ NemoClaw/ NeMo-Fabric/
set -euo pipefail

LAIN_BIN="${LAIN_BIN:-./target/release/lain}"
WS="${1:?usage: $0 <workspace-dir> [port]}"
PORT="${2:-9971}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/repos.yaml" <<EOF
data_dir: $WORK/.lain/federation
max_concurrent_indexers: 3
repos:
  - id: openshell
    packages: ["@nvidia/openshell-sdk", "openshell"]
    source:
      type: workspace_dir
      path: $WS/OpenShell
  - id: nemo-fabric
    packages: ["nemo-fabric"]
    source:
      type: workspace_dir
      path: $WS/NeMo-Fabric
services:
  - name: openshell-gateway
    repo: openshell
    paths: [proto, crates]
    hosts: [localhost, 127.0.0.1]
    env: [OPENSHELL_GATEWAY]
  - name: openshell-sdk
    repo: openshell
    paths: [python, sdk]
  - name: nemo-fabric
    repo: nemo-fabric
EOF

"$LAIN_BIN" server --config "$WORK/repos.yaml" --transport http --port "$PORT" \
  > "$WORK/server.log" 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true; rm -rf "$WORK"' EXIT

call() {
  curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' -H 'Accept: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}

for _ in $(seq 1 180); do
  curl -sf -o /dev/null "http://127.0.0.1:$PORT/health" && break
  sleep 1
done

RPC=$(call list_contracts '{"snapshot":"live","kind":"rpc","limit":200}')
BOUND=$(printf '%s' "$RPC" | python3 -c '
import json,sys
d=json.load(sys.stdin)
items=d.get("data",{}).get("items",[])
print(sum(i.get("bound_consumers",0) for i in items))')
echo "rpc contracts: $(printf '%s' "$RPC" | python3 -c '
import json,sys
print(len(json.load(sys.stdin).get("data",{}).get("items",[])))')"
echo "bound consumers: $BOUND"

if [ "$BOUND" -lt 1 ]; then
  echo "FAIL: no RPC contract has a bound consumer" >&2
  exit 1
fi
echo "PASS: at least one RPC contract binds a consumer"
```

- [ ] **Step 2: Run it against the real workspace**

Run: `bash scripts/eval/openshell-contract-smoke.sh /data/agents/tmp-lain-tests/nvidia-eval 9971`
Expected: `rpc contracts: > 0`, `bound consumers: >= 1`, `PASS`.

> If `bound consumers` is 0, the consumer fact is present but did not bind: check `list_unresolved` for the consumer's verdict. `Internal` means the two services were not split; `RpcStubUnknown` means the channel did not resolve (Task 3).

- [ ] **Step 3: Record the numbers in the docs**

Add a "Measured" paragraph to `docs/protocol-coverage.md` §rpc with the counts the script printed, the Lain commit, and the date. A number in the docs that nobody can reproduce is worse than no number.

- [ ] **Step 4: Commit**

```bash
git add scripts/eval/openshell-contract-smoke.sh docs/protocol-coverage.md
git commit -m "test(eval): openshell rpc contract smoke run over the real NVIDIA workspace"
```

---

## Out of scope

- **A TypeScript / ConnectRPC consumer sensor.** OpenShell's TypeScript SDK (`sdk/typescript/src/transport.ts`) uses `@connectrpc/connect-node`, not raw gRPC stubs; NemoClaw calls `OpenShellClient.connect(...)` (`NemoClaw/src/lib/adapters/openshell/sdk.ts:158`), a plain method call on an imported package. Detecting that needs a new client shape *and* a policy for the npm-package indirection. The honest scope of this plan is the Python SDK; the TypeScript surface stays visible as a cross-repo *call-graph* edge (Task 5's `packages:` declaration makes the link intentional), not as an RPC contract.
- **A Rust / tonic consumer sensor.** The Rust SDK exposes `openshell_core::proto::open_shell_client::OpenShellClient as GrpcClient` (`OpenShell/crates/openshell-sdk/src/raw.rs:22`); the gRPC consumer sensor covers `go|py|java|cc|cpp|h|hpp` only.
- **Runtime provider declaration.** That is Plan B (`docs/superpowers/plans/2026-10-29-nat-http-contract.md`), which adds the `repos.yaml#provides` section and the agent-facing write path.
