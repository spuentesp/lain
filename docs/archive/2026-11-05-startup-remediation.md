# Startup Remediation and Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reach full static readiness in at most 10 seconds on the measured 993-file checkout, preserve graph output, and stop MCP clients from silently attaching to stale shared daemons.

**Architecture:** Add an opt-in real-checkout performance and canonical-digest harness first. Optimize `field_access` through transient per-run graph, file-metadata, and scope-analysis reuse, then remove repeated source-string copies and make Rust gRPC indexing lazy. Validate daemon `version`, `build_commit`, and `index_build_id` before bridging; use bounded, measured follow-ups only after these changes.

**Tech Stack:** Rust 2021, tokio, tree-sitter 0.25, `std::sync::Arc`, existing Rayon blocks, `GraphDatabase`, `canonical_digest_hex`, Cargo tests, Unix socket JSON-RPC.

**Spec:** `docs/superpowers/specs/2026-11-05-startup-remediation-design.md`

## Global Constraints

- Every cold run must reach full static readiness in **≤10s**; target **≤4s** for `field_access` and **≤5s** for all other startup work.
- Preserve all analyses, canonical graph output, operation identity, source sites, and readiness semantics. Do not defer `field_access` to meet the target.
- Peak RSS must be **≤110% of the same-host baseline**. All analysis caches are transient and dropped after their sensor pass.
- Add no dependency and no persistent cache. No federation schema migration or release version bump is expected; stop and revise if serialized formats must change.
- Do not revert `ea91ef8c`. Do not add broad sensor parallelism or source-text pre-filters.
- Feature branches and PRs target `dev`, never `main` directly. Every commit message begins `Answer:`.
- Read and follow `AGENTS.md`, `docs/CONTRIBUTING_AGENTS.md`, `src/server/sensors/AGENTS.md`, and `src/server/mcp/AGENTS.md` before editing their respective files.
- Remove benchmark processes and temporary artifacts after every measurement. Do not stop an operator daemon without fresh PID verification and explicit operator approval.

## Review Focus

- Two functions with the same name but different graph IDs must never share scope results; Task 4 pins ID-based cache identity.
- HTTP and payload operations on one sender must use different binding rules; Task 4 proves separate analyses and distinct operation facts.
- A daemon running the same package version but different uncommitted indexing code must be rejected; Task 2 pins `index_build_id` rather than trusting `version` or `build_commit` alone.
- Cycles, unreadable files, nested functions, and module-level reads must retain existing behavior; Tasks 3–5 include focused coverage.
- Missing, non-UTF-8, exactly-2-MiB, and over-2-MiB files must retain cache semantics, and shared content must remain safe across existing Rayon blocks; Tasks 6 and 7 pin these cases.

---

### Task 1: Add the Real-Checkout Baseline Harness

**Files:**
- Create: `tests/startup_remediation.rs`
- Modify: `tests/performance_budgets.rs:163-300,598-638`

**Interfaces:**
- Consumes: public `index_one_repo(IndexRequest { mode: IndexMode::Live, .. })`, `canonical_digest_hex`, and the existing `ServerGuard`/`free_port` helpers.
- Produces: `LAIN_STARTUP_REPO_ROOT`, `LAIN_STARTUP_BUDGET_MS`, `LAIN_STARTUP_BASELINE_RSS_BYTES`, and `LAIN_STARTUP_DIGEST_FILE` benchmark inputs; `boot_and_measure(...) -> (ServerGuard, StartupMetrics, String)` for the real-checkout test.

- [ ] **Step 1: Write the failing budget-parser and digest-baseline tests**

Create `tests/startup_remediation.rs` with:

```rust
use lain::federation::contracts::digest::canonical_digest_hex;
use lain::federation::repo_id::RepoId;
use lain::federation::repo_source::{RepoSource, WorkspaceDirSource};
use lain::git::{AnyGitSensor, GitSensorMode};
use lain::graph::GraphDatabase;
use lain::schema::RepoNamespace;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
fn parse_budget_ms(value: Option<&str>) -> u64 {
    value
        .map(|value| value.parse::<u64>().expect("numeric startup budget"))
        .filter(|budget| *budget > 0)
        .unwrap_or(60_000)
}

#[test]
fn configured_budget_rejects_zero() {
    assert!(std::panic::catch_unwind(|| parse_budget_ms(Some("0"))).is_err());
}

async fn static_index(root: &Path) -> (tempfile::TempDir, GraphDatabase) {
    let data_dir = tempfile::tempdir().unwrap();
    let db = GraphDatabase::new(&data_dir.path().join("graph.bin")).unwrap();
    let source: Arc<dyn RepoSource> = Arc::new(
        WorkspaceDirSource::new(RepoId::new("startup").unwrap(), root.to_path_buf()).unwrap(),
    );
    let local_path = source.local_path().to_path_buf();
    let git = AnyGitSensor::new(&local_path, GitSensorMode::InProcess).unwrap();
    let namespace = RepoNamespace::for_test();
    let cancel = tokio_util::sync::CancellationToken::new();
    lain::server::ingest::ingestion::index_one_repo(
        lain::server::ingest::ingestion::IndexRequest {
            path: &local_path,
            graph: &db,
            lsp_pool: None,
            git: &git,
            overlay: None,
            resolver: None,
            source_repo: None,
            namespace: &namespace,
            force: true,
            cancel: &cancel,
            mode: lain::server::ingest::ingestion::IndexMode::Live,
        },
    )
    .await
    .unwrap();
    (data_dir, db)
}

#[tokio::test]
#[ignore]
async fn real_checkout_digest_matches_baseline() {
    let root = PathBuf::from(std::env::var("LAIN_STARTUP_REPO_ROOT").unwrap());
    let baseline = PathBuf::from(std::env::var("LAIN_STARTUP_DIGEST_FILE").unwrap());
    let (_tmp, db) = static_index(&root).await;
    let fresh = canonical_digest_hex(&db);
    if baseline.exists() {
        assert_eq!(std::fs::read_to_string(&baseline).unwrap().trim(), fresh);
    } else {
        std::fs::write(&baseline, format!("{fresh}\n")).unwrap();
    }
}
```

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test startup_remediation configured_budget_rejects_zero`

Expected: FAIL because `parse_budget_ms` currently accepts zero.

- [ ] **Step 2: Reject zero and add the real-server measurement wrapper**

Change `parse_budget_ms` to this pure function, which needs no environment mutation in tests:

```rust
#[allow(dead_code)]
fn parse_budget_ms(value: Option<&str>) -> u64 {
    value
        .map(|value| {
            let budget = value.parse::<u64>().expect("numeric startup budget");
            assert!(budget > 0, "startup budget must be greater than zero");
            budget
        })
        .unwrap_or(60_000)
}
```

In `performance_budgets.rs`, retain `boot_and_time` as the compatibility wrapper and move its body into:

```rust
#[derive(Debug)]
struct StartupMetrics {
    listening: Duration,
    indexed: Duration,
    peak_rss_bytes: Option<u64>,
}

fn boot_and_measure(
    project_dir: &Path,
    repo_dir: &Path,
    data_dir: &Path,
    port: u16,
    disable_lsp: Option<bool>,
) -> (ServerGuard, StartupMetrics, String)
```

Use the existing body, adding this before spawning when `disable_lsp` is `Some`:

```rust
if let Some(disable) = disable_lsp {
    command.env("LAIN_DISABLE_LSP", if disable { "1" } else { "0" });
}
```

Sample RSS in both health-polling loops and retain the maximum:

```rust
#[cfg(target_os = "linux")]
fn rss_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb = status.lines().find_map(|line| {
        line.strip_prefix("VmRSS:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    Some(kb * 1024)
}

#[cfg(not(target_os = "linux"))]
fn rss_bytes(_pid: u32) -> Option<u64> { None }
```

Restore the old wrapper exactly:

```rust
fn boot_and_time(
    project_dir: &Path,
    repo_dir: &Path,
    data_dir: &Path,
    port: u16,
) -> (ServerGuard, Duration, Duration, String) {
    let (guard, metrics, host) =
        boot_and_measure(project_dir, repo_dir, data_dir, port, None);
    (guard, metrics.listening, metrics.indexed, host)
}
```

- [ ] **Step 3: Add the three-run real-checkout performance test**

Add `PathBuf` to the existing `std::path::Path` import, then add to `performance_budgets.rs`:

```rust
#[test]
#[ignore]
fn real_checkout_static_ready_under_configured_budget() {
    let repo = PathBuf::from(std::env::var("LAIN_STARTUP_REPO_ROOT").unwrap());
    let budget = std::env::var("LAIN_STARTUP_BUDGET_MS")
        .unwrap_or_else(|_| "60000".into())
        .parse::<u128>()
        .unwrap();
    let baseline_rss = std::env::var("LAIN_STARTUP_BASELINE_RSS_BYTES")
        .ok()
        .map(|v| v.parse::<u64>().unwrap());

    for run in 0..3 {
        let port = free_port();
        let project = tempfile::tempdir().unwrap();
        let data = project.path().join("data");
        let (mut guard, metrics, host) = boot_and_measure(
            project.path(), &repo, &data, port, Some(true),
        );
        guard.exit_diag().unwrap();
        assert!(
            metrics.indexed.as_millis() <= budget,
            "run {run}: full static readiness took {:?} on {host}",
            metrics.indexed,
        );
        if let Some(baseline) = baseline_rss {
            let peak = metrics.peak_rss_bytes.expect("Linux peak RSS");
            assert!(
                peak <= baseline * 110 / 100,
                "run {run}: peak RSS {peak} exceeds 110% of baseline {baseline}",
            );
        }
        eprintln!(
            "startup run={run} listening={:?} indexed={:?} peak_rss={:?}",
            metrics.listening, metrics.indexed, metrics.peak_rss_bytes,
        );
        drop(guard);
    }
}
```

- [ ] **Step 4: Verify the harness and capture the unoptimized baseline**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test startup_remediation configured_budget_rejects_zero
LAIN_STARTUP_REPO_ROOT="$PWD" \
LAIN_STARTUP_DIGEST_FILE=/tmp/lain-startup-baseline.digest \
  cargo test --release --test startup_remediation real_checkout_digest_matches_baseline -- --ignored --nocapture
LAIN_STARTUP_REPO_ROOT="$PWD" LAIN_STARTUP_BUDGET_MS=60000 \
  cargo test --release --test performance_budgets real_checkout_static_ready_under_configured_budget -- --ignored --nocapture
```

Expected: the parser test passes; the digest file is created; all three startup runs finish below 60s. Record each run's wall time and peak RSS. Set `LAIN_STARTUP_BASELINE_RSS_BYTES` to the largest recorded baseline for later tasks. Preserve `/tmp/lain-startup-baseline.digest` for comparison and remove other generated logs.

- [ ] **Step 5: Commit the baseline harness**

```bash
git add tests/startup_remediation.rs tests/performance_budgets.rs
git commit -m "Answer: Add static startup performance baselines"
```

### Task 2: Reject Incompatible Shared Daemons

**Files:**
- Modify: `src/server/mcp/socket_server.rs:385-403`
- Modify: `src/cli/shared_daemon.rs:62-71,112-140,155-246,393-535`
- Modify: `src/cli/mcp.rs:252-283`
- Modify: `tests/shared_daemon_mcp.rs:346-364`
- Modify: `docs/USER_MANUAL.md:254-276`
- Modify: `CHANGELOG.md:6-20`

**Interfaces:**
- Consumes: `lain::server::build_info::{VERSION, GIT_SHA, INDEX_BUILD_ID}`.
- Produces: `DaemonIdentity { version, build_commit, index_build_id }`, `DaemonInfo` with optional identity fields, and `attach_or_start(...) -> anyhow::Result<Option<UnixStream>>`.

- [ ] **Step 1: Write failing identity and payload tests**

Add `#[cfg(test)] mod identity_tests` to `shared_daemon.rs`. The tests reference production types rather than defining test-only duplicates:

```rust
use super::{compatibility_error, DaemonIdentity, DaemonInfo};

#[test]
fn identity_mismatch_names_the_actual_and_expected_values() {
    let mut actual = DaemonIdentity::current();
    actual.index_build_id = "stale-index".into();
    let error = compatibility_error(&DaemonInfo {
        pid: 1,
        workspaces: vec![],
        open_connections: 0,
        shared: true,
        version: Some(actual.version),
        build_commit: Some(actual.build_commit),
        index_build_id: Some(actual.index_build_id),
    }).unwrap();
    assert!(error.contains("stale-index"));
    assert!(error.contains(crate::server::build_info::INDEX_BUILD_ID));
}

#[test]
fn legacy_missing_identity_is_rejected() {
    let error = compatibility_error(&DaemonInfo {
        pid: 1,
        workspaces: vec![],
        open_connections: 0,
        shared: true,
        version: Some(crate::server::build_info::VERSION.into()),
        build_commit: None,
        index_build_id: None,
    }).unwrap();
    assert!(error.contains("build_commit"));
    assert!(error.contains("index_build_id"));
}
```

Add to `two_clients_same_set_share_one_daemon` after `let info = daemon_info(&sock);`:

```rust
assert_eq!(info.pointer("/result/version").and_then(Value::as_str), Some(lain::server::build_info::VERSION));
assert_eq!(info.pointer("/result/build_commit").and_then(Value::as_str), Some(lain::server::build_info::GIT_SHA));
assert_eq!(info.pointer("/result/index_build_id").and_then(Value::as_str), Some(lain::server::build_info::INDEX_BUILD_ID));
```

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test identity_mismatch_names_the_actual_and_expected_values --lib`

Expected: FAIL because `compatibility_error` and the new fields do not exist.

- [ ] **Step 2: Emit and parse all identity fields**

Change the `lain/daemon_info` response to include:

```rust
"version": crate::server::build_info::VERSION,
"build_commit": crate::server::build_info::GIT_SHA,
"index_build_id": crate::server::build_info::INDEX_BUILD_ID,
```

Add `#[derive(Debug, Clone)]` to `DaemonInfo`, extend it with `version`, `build_commit`, and `index_build_id`, all `Option<String>`, and parse them with `.and_then(Value::as_str).map(str::to_string)`.

Define the production identity and comparison next to `DaemonInfo`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonIdentity {
    version: String,
    build_commit: String,
    index_build_id: String,
}

impl DaemonIdentity {
    fn current() -> Self {
        Self {
            version: crate::server::build_info::VERSION.into(),
            build_commit: crate::server::build_info::GIT_SHA.into(),
            index_build_id: crate::server::build_info::INDEX_BUILD_ID.into(),
        }
    }
}

fn compatibility_error(info: &DaemonInfo) -> Option<String> {
    let expected = DaemonIdentity::current();
    let matches = info.version.as_deref() == Some(expected.version.as_str())
        && info.build_commit.as_deref() == Some(expected.build_commit.as_str())
        && info.index_build_id.as_deref() == Some(expected.index_build_id.as_str());
    (!matches).then(|| format!(
        "incompatible shared daemon: expected version={} build_commit={} index_build_id={}; actual version={:?} build_commit={:?} index_build_id={:?}",
        expected.version, expected.build_commit, expected.index_build_id,
        info.version, info.build_commit, info.index_build_id,
    ))
}
```

- [ ] **Step 3: Add the failing fake-daemon integration test**

Add this fake legacy daemon to `shared_daemon_mcp.rs`. It answers the discovery and bind-race probes without running an indexer:

```rust
fn spawn_stale_info(socket: &Path, workspace: &Path) -> std::thread::JoinHandle<()> {
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    let workspace = workspace.canonicalize().unwrap().display().to_string();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            let response = serde_json::json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {
                    "pid": 999,
                    "version": lain::server::build_info::VERSION,
                    "build_commit": "stale",
                    "index_build_id": "stale",
                    "workspaces": [workspace.clone()],
                    "open_connections": 0,
                    "shared": true,
                }
            });
            writeln!(writer, "{response}").unwrap();
            writer.flush().unwrap();
        }
    })
}

#[test]
fn incompatible_daemon_is_not_attached() {
    let iso = Isolation::new();
    let (_ws_tmp, workspace) = make_repo("stale");
    std::fs::create_dir_all(iso.socket_dir()).unwrap();
    let socket = iso.socket_dir().join("lain-mcp-0123456789abcdef.sock");
    let fake = spawn_stale_info(&socket, &workspace);

    let mut child = Command::new(env!("CARGO_BIN_EXE_lain"));
    let child = child
        .arg("mcp")
        .arg("--workspace")
        .arg(&workspace)
        .env("XDG_RUNTIME_DIR", &iso.run)
        .env("HOME", iso.home.path())
        .env("XDG_STATE_HOME", iso.home.path().join("state"))
        .env("XDG_CONFIG_HOME", iso.home.path().join("config"))
        .env_remove("LAIN_WORKSPACE")
        .env_remove("LAIN_MCP_NO_SHARE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    fake.join().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("incompatible shared daemon"), "{stderr}");
    assert!(stderr.contains("stale"), "{stderr}");
    assert!(stderr.contains("--no-share"), "{stderr}");
}
```

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test shared_daemon_mcp incompatible_daemon_is_not_attached`

Expected: FAIL because the client currently bridges to the fake daemon.

- [ ] **Step 4: Propagate compatibility errors without spawning a rival**

Add `Discovery::Incompatible { socket: PathBuf, info: DaemonInfo }`. During discovery, retain the first incompatible shared candidate but continue scanning: a compatible covering or partial daemon always wins. Return `Incompatible` only when no compatible candidate exists.

Change `attach_or_start` to return `anyhow::Result<Option<UnixStream>>`; change the early no-share and path-too-long returns to `Ok(None)`. Handle the new variant with:

```rust
Discovery::Incompatible { socket, info } => Err(anyhow!(
    "{} at {}; restart the daemon or use --no-share",
    compatibility_error(&info).unwrap_or_else(|| "incompatible shared daemon".into()),
    socket.display(),
)),
```

Make `only_shared` return the same `Result` and repeat the check for bind-race winners. Keep `shared: false` as `Ok(None)` and a failed probe as `Ok(Some(stream))`, preserving those existing contracts. Because `start_detached_daemon` still returns `Option<UnixStream>`, replace every `.and_then(|stream| only_shared(...))` with this adapter instead of leaving an `Option`/`Result` type mismatch:

```rust
fn finish_start(start: Option<UnixStream>, socket: &Path) -> anyhow::Result<Option<UnixStream>> {
    match start {
        Some(stream) => only_shared(stream, socket),
        None => Ok(None),
    }
}
```

Call `finish_start(start_detached_daemon(...), &socket_key)` at each detached start site.

In `src/cli/mcp.rs`, change the call to:

```rust
if let Some(stream) = crate::cli::shared_daemon::attach_or_start(
    &workspaces,
    embedding_model,
    reindex_timeout,
    no_share,
)? {
    // existing bridge body unchanged
}
```

- [ ] **Step 5: Document and verify the compatibility behavior**

Add to `docs/USER_MANUAL.md` under multi-agent coordination:

```markdown
A client bridges only when `version`, `build_commit`, and `index_build_id`
match its own. A mismatch or missing identity produces an error naming the
socket and both identities. Restart the daemon, or explicitly select a
private instance with `--no-share` / `LAIN_MCP_NO_SHARE=1`; Lain never
silently attaches to stale code or starts a competing indexer in response.
```

Add the same behavior as one bullet under `CHANGELOG.md` `Unreleased`.

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --lib identity_tests
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test shared_daemon_mcp
```

Expected: all pass, including the existing same-set, handoff, zero-config, and no-share tests.

- [ ] **Step 6: Commit**

```bash
git add src/server/mcp/socket_server.rs src/cli/shared_daemon.rs src/cli/mcp.rs tests/shared_daemon_mcp.rs docs/USER_MANUAL.md CHANGELOG.md
git commit -m "Answer: Reject incompatible shared daemons"
```

### Task 3: Cache Owned File Metadata and Add Stage Metrics

**Files:**
- Modify: `src/server/sensors/field_access_sensor.rs:198-300,4256-4274,4303-4552,4702-4950`
- Create: `src/server/sensors/field_access_sensor/reuse_tests.rs`
- Modify: `src/server/sensors/field_access_sensor.rs` test-module declarations

**Interfaces:**
- Consumes: existing `ensure_file`, `collect_function_defs`, `collect_function_metadata`, and `extend_emissions_with_scope`.
- Produces: `ParsedFiles`, owned `FunctionDef { name, line, range }`, and `FieldAccessMetrics`; no tree-sitter `Node` is stored in the cache.

- [ ] **Step 1: Write the failing metadata-reuse test**

Start `reuse_tests.rs` with `use super::*;`, `use crate::federation::contracts::model::{ConsumerFact, SourceSite};`, and `use crate::schema::StaticSource;`. Then add a Python file containing a sender and a callee and this test:

```rust
#[test]
fn file_metadata_is_built_once_and_owned_ranges_resolve_to_functions() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("sample.py"),
        "def send():\n    return helper()\n\ndef helper():\n    return {'id': 1}\n",
    ).unwrap();
    let mut files = ParsedFiles::default();
    let mut metrics = FieldAccessMetrics::default();

    assert!(files.ensure(tmp.path(), "sample.py", &mut metrics));
    assert!(files.ensure(tmp.path(), "sample.py", &mut metrics));
    assert_eq!(metrics.file_metadata_builds, 1);

    let file = files.get("sample.py").unwrap();
    let send = file.defs.iter().find(|def| def.name == "send").unwrap();
    let node = file.node(send).unwrap();
    assert_eq!(node.kind(), "function_definition");
}
```

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test file_metadata_is_built_once_and_owned_ranges_resolve_to_functions --lib`

Expected: FAIL because `ParsedFiles`, `FunctionDef::node`, and the counter do not exist.

- [ ] **Step 2: Implement owned function ranges and the file cache**

Add:

```rust
#[derive(Debug, Default, PartialEq, Eq)]
struct FieldAccessMetrics {
    scope_http: usize,
    scope_payload: usize,
    scope_cache_hits: usize,
    file_metadata_builds: usize,
    path_index_builds: usize,
    graph_snapshots: usize,
}

#[derive(Debug)]
struct FunctionDef {
    name: String,
    line: u32,
    range: tree_sitter::Range,
}

struct ParsedFile {
    lang: Lang,
    content: std::sync::Arc<str>,
    tree: Tree,
    defs: Vec<FunctionDef>,
    metadata: FunctionMetadata,
}

impl ParsedFile {
    fn node(&self, def: &FunctionDef) -> Option<tree_sitter::Node<'_>> {
        self.tree.root_node().descendant_for_byte_range(
            def.range.start_byte,
            def.range.end_byte,
        )
    }
}

#[derive(Default)]
struct ParsedFiles {
    entries: std::collections::BTreeMap<String, ParsedFile>,
}
```

`ParsedFiles::ensure` replaces `ensure_file`: it reads each file once, parses once, collects owned ranges once, and builds `FunctionMetadata` once. In this task, use `let content = std::fs::read_to_string(path).ok()?; let content: std::sync::Arc<str> = std::sync::Arc::from(content.as_str());` so the boolean loader does not use `?` on a `Result`. Task 6 replaces that read with `read_shared_or_fs`. Increment `file_metadata_builds` only on a cache miss. Do not store `Node<'a>` in `ParsedFile`; that would make the tree and its borrowed nodes self-referential.

Use `ParsedFiles` inside `extend_emissions_with_scope`. Replace repeated `collect_function_defs` and `collect_function_metadata` calls with `file.defs` and `file.metadata`. Add `#[derive(Clone)]` to `FunctionMetadata` and change `merge_metadata` to borrow each cached `&FunctionMetadata`, cloning only the entries needed for the scope-specific merged map; never move metadata out of a file cache. Resolve sender/member nodes through `ParsedFile::node` and verify the expected name before walking.

- [ ] **Step 3: Add deterministic stage timing**

Wrap the existing phases in `scan_workspace_field_access_with_report`, capture `Instant::now()` at each boundary, compute each `*_ms` value with `started.elapsed().as_millis()`, and emit:

```rust
tracing::info!(
    graph_ms,
    local_detection_ms,
    scope_ms,
    emission_ms,
    replacement_ms,
    metrics = ?metrics,
    "field_access stage metrics",
);
```

Measure `graph_ms` around graph/call collection, `local_detection_ms` around file detection, `scope_ms` around scope extension, `emission_ms` around node/edge construction and consumer patches, and `replacement_ms` around `replace_sensor_output`.

- [ ] **Step 4: Verify metadata correctness and existing behavior**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test file_metadata --lib
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test payload_field_lineage
```

Expected: all pass, including nested-function, dynamic-key, cycle, and unreadable-file tests. Confirm the metrics log reports one metadata build per loaded path.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/field_access_sensor.rs src/server/sensors/field_access_sensor/reuse_tests.rs
git commit -m "Answer: Cache field access file metadata"
```

### Task 4: Memoize Scope Analysis by Sender and Binding Kind

**Files:**
- Modify: `src/server/sensors/field_access_sensor.rs:4303-4552`
- Modify: `src/server/sensors/field_access_sensor/reuse_tests.rs`
- Modify: `tests/payload_field_lineage.rs`

**Interfaces:**
- Consumes: Task 3's `ParsedFiles` and `FieldAccessMetrics`.
- Produces: `ResponseBinding::{HttpResponse, FirstParameter}`, `ScopeKey { sender_id, binding }`, and `ScopeValue { reads, escapes }` for one sensor pass.

- [ ] **Step 1: Write failing cache-identity tests**

Add these concrete helpers to `reuse_tests.rs`; the returned `TempDir` keeps the graph's WAL path alive:

```rust
struct ScopeFixture {
    _tmp: tempfile::TempDir,
    graph: GraphDatabase,
    sender_id: String,
    call_a: String,
    call_b: String,
}

fn scope_fixture() -> ScopeFixture {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("send.py"), "def send():\n    return client.get(url)['id']\n").unwrap();
    std::fs::write(tmp.path().join("helper.py"), "def helper():\n    return response['id']\n").unwrap();
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    let mut sender = GraphNode::new_in(NodeType::Function, "send", "send.py", &ns);
    sender.line_start = Some(0);
    sender.line_end = Some(1);
    let mut helper = GraphNode::new_in(NodeType::Function, "helper", "helper.py", &ns);
    helper.line_start = Some(0);
    helper.line_end = Some(1);
    let call_a = GraphNode::new_in(NodeType::HttpClientCall, "call-a", "send.py", &ns);
    let call_b = GraphNode::new_in(NodeType::HttpClientCall, "call-b", "send.py", &ns);
    let sender_id = sender.id.clone();
    let helper_id = helper.id.clone();
    let call_a_id = call_a.id.clone();
    let call_b_id = call_b.id.clone();
    graph.insert_nodes_batch(&[sender, helper, call_a, call_b]).unwrap();
    let mut calls = GraphEdge::new(EdgeType::Calls, sender_id.clone(), helper_id);
    calls.provenance = Some(EdgeProvenance::Static { source: StaticSource::TreeSitter });
    graph.insert_edge(&calls).unwrap();
    for target in [&call_a_id, &call_b_id] {
        let mut edge = GraphEdge::new(EdgeType::SendsHttp, sender_id.clone(), target.clone());
        edge.site = Some(SourceSite { path: "send.py".into(), line: 1 });
        graph.insert_edge(&edge).unwrap();
    }
    ScopeFixture { _tmp: tmp, graph, sender_id, call_a: call_a_id, call_b: call_b_id }
}

fn emission(
    sender_id: &str,
    call_id: &str,
    origin: FieldReadOrigin,
    operation: Option<crate::federation::contracts::model::ContractKey>,
) -> FieldAccessEmission {
    FieldAccessEmission {
        path: "send.py".into(),
        call_id: call_id.into(),
        sender_id: sender_id.into(),
        reads: vec![],
        escapes: BTreeSet::new(),
        reads_complete: true,
        origin,
        operation,
    }
}

#[test]
fn http_calls_share_scope_analysis_but_keep_distinct_call_targets() {
    let fixture = scope_fixture();
    let mut emissions = vec![
        emission(&fixture.sender_id, &fixture.call_a, FieldReadOrigin::FieldAccess, None),
        emission(&fixture.sender_id, &fixture.call_b, FieldReadOrigin::FieldAccess, None),
    ];
    let mut analysis = FieldAccessAnalysis::new(fixture._tmp.path(), &fixture.graph);
    analysis.extend_emissions(&mut emissions);
    assert_eq!(analysis.metrics.scope_http, 1);
    assert_eq!(analysis.metrics.scope_cache_hits, 1);
    assert_eq!(emissions[0].call_id, fixture.call_a);
    assert_eq!(emissions[1].call_id, fixture.call_b);
}

#[test]
fn payload_operations_share_analysis_but_keep_operation_identity() {
    let fixture = scope_fixture();
    let topic = crate::federation::contracts::model::ContractKey::Topic {
        broker: "kafka".into(), name: "orders".into(),
    };
    let graphql = crate::federation::contracts::model::ContractKey::Graphql {
        op: crate::federation::contracts::model::GraphqlOp::Query,
        field: "orders".into(),
    };
    let mut emissions = vec![
        emission(&fixture.sender_id, &fixture.call_a, FieldReadOrigin::TopicConsumer, Some(topic.clone())),
        emission(&fixture.sender_id, &fixture.call_b, FieldReadOrigin::GraphqlConsumer, Some(graphql.clone())),
    ];
    let mut analysis = FieldAccessAnalysis::new(fixture._tmp.path(), &fixture.graph);
    analysis.extend_emissions(&mut emissions);
    assert_eq!(analysis.metrics.scope_payload, 1);
    assert_eq!(emissions[0].operation, Some(topic));
    assert_eq!(emissions[1].operation, Some(graphql));
}

#[test]
fn http_and_payload_on_one_sender_do_not_share() {
    let fixture = scope_fixture();
    let topic = crate::federation::contracts::model::ContractKey::Topic {
        broker: "kafka".into(), name: "orders".into(),
    };
    let mut emissions = vec![
        emission(&fixture.sender_id, &fixture.call_a, FieldReadOrigin::FieldAccess, None),
        emission(&fixture.sender_id, &fixture.call_b, FieldReadOrigin::TopicConsumer, Some(topic)),
    ];
    let mut analysis = FieldAccessAnalysis::new(fixture._tmp.path(), &fixture.graph);
    analysis.extend_emissions(&mut emissions);
    assert_eq!(analysis.metrics.scope_http, 1);
    assert_eq!(analysis.metrics.scope_payload, 1);
    assert_eq!(analysis.metrics.scope_cache_hits, 0);
}

#[test]
fn same_named_senders_with_different_ids_do_not_share() {
    let fixture = scope_fixture();
    let ns = RepoNamespace::for_test();
    let mut second = GraphNode::new_in(NodeType::Function, "send", "other.py", &ns);
    second.line_start = Some(0);
    second.line_end = Some(1);
    let second_id = second.id.clone();
    fixture.graph.upsert_node(second).unwrap();
    let mut calls = GraphEdge::new(EdgeType::Calls, second_id.clone(), fixture.sender_id.clone());
    calls.provenance = Some(EdgeProvenance::Static { source: StaticSource::TreeSitter });
    fixture.graph.insert_edge(&calls).unwrap();
    std::fs::write(fixture._tmp.path().join("other.py"), "def send():\n    return response['id']\n").unwrap();
    let mut emissions = vec![
        emission(&fixture.sender_id, &fixture.call_a, FieldReadOrigin::FieldAccess, None),
        emission(&second_id, &fixture.call_b, FieldReadOrigin::FieldAccess, None),
    ];
    let mut analysis = FieldAccessAnalysis::new(fixture._tmp.path(), &fixture.graph);
    analysis.extend_emissions(&mut emissions);
    assert_eq!(analysis.metrics.scope_http, 2);
}
```

In `tests/payload_field_lineage.rs`, retain the assertion that two operations on one consumer produce two emissions with different `ContractKey`s, and add `ReadsFrom` target assertions for both operation IDs.

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test scope_ --lib`

Expected: FAIL because every emission is currently analyzed independently and `FieldAccessAnalysis` does not exist.

- [ ] **Step 2: Extract one scope computation from the emission loop**

Define:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ResponseBinding { HttpResponse, FirstParameter }

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScopeKey {
    sender_id: String,
    binding: ResponseBinding,
}

#[derive(Clone)]
struct ScopeValue {
    reads: Vec<FieldRead>,
    escapes: std::collections::BTreeSet<Escape>,
}
```

Extract the existing per-emission body into `analyze_scope(&mut self, emission: &FieldAccessEmission) -> Option<ScopeValue>`. `None` retains the existing phase-A result for an unresolved sender or an S-only scope. Do not convert that case into a cached empty result.

Increment `scope_http` or `scope_payload` only when this computation actually runs.

- [ ] **Step 3: Memoize without caching operation identity**

Introduce the per-pass owner and store scope results only inside it:

```rust
struct FieldAccessAnalysis<'a> {
    root: &'a std::path::Path,
    graph: &'a GraphDatabase,
    files: ParsedFiles,
    scope_cache: BTreeMap<ScopeKey, ScopeValue>,
    metrics: FieldAccessMetrics,
}

impl<'a> FieldAccessAnalysis<'a> {
    fn new(root: &'a std::path::Path, graph: &'a GraphDatabase) -> Self {
        Self {
            root,
            graph,
            files: ParsedFiles::default(),
            scope_cache: BTreeMap::new(),
            metrics: FieldAccessMetrics::default(),
        }
    }
}
```

Use this lookup for every eligible emission:

```rust
let key = ScopeKey {
    sender_id: emission.sender_id.clone(),
    binding: if emission.origin == FieldReadOrigin::FieldAccess {
        ResponseBinding::HttpResponse
    } else {
        ResponseBinding::FirstParameter
    },
};
if let Some(value) = self.scope_cache.get(&key) {
    self.metrics.scope_cache_hits += 1;
    apply_scope_value(emission, value);
    return;
}
if let Some(value) = self.analyze_scope(emission) {
    apply_scope_value(emission, &value);
    self.scope_cache.insert(key, value);
}
```

`apply_scope_value` clones only `reads` and `escapes`, then sets `reads_complete = escapes.is_empty()`. It never overwrites `call_id`, `origin`, `operation`, or `path`.

- [ ] **Step 4: Verify output equivalence and counters**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test scope_ --lib
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test payload_field_lineage
LAIN_STARTUP_REPO_ROOT="$PWD" \
LAIN_STARTUP_DIGEST_FILE=/tmp/lain-startup-baseline.digest \
  cargo test --release --test startup_remediation real_checkout_digest_matches_baseline -- --ignored --nocapture
```

Expected: all tests and the canonical digest comparison pass. On the real checkout, `scope_http + scope_payload` is lower than the number of eligible emissions and every operation remains distinct.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/field_access_sensor.rs src/server/sensors/field_access_sensor/reuse_tests.rs tests/payload_field_lineage.rs
git commit -m "Answer: Reuse field access scope analysis"
```

### Task 5: Consolidate Graph Views, Path Lookups, and Consumer Patches

**Files:**
- Modify: `src/server/sensors/field_access_sensor.rs:198-300,428-526,4146-4254,4303-4552`
- Modify: `src/server/sensors/field_access_sensor/reuse_tests.rs`

**Interfaces:**
- Consumes: Task 4's `FieldAccessAnalysis` and `canonical_digest_hex`.
- Produces: `FieldAccessGraphView { nodes, edges, fn_index, callers_of, calls_by_function, path_index }` and batched incomplete-consumer updates through `upsert_nodes_batch`.

- [ ] **Step 1: Write failing path-index and batching tests**

Add self-contained fixtures rather than referring to nonexistent helper functions:

```rust
#[test]
fn path_index_uses_innermost_function_and_file_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    let file = GraphNode::new_in(NodeType::File, "sample.py", "sample.py", &ns);
    let mut outer = GraphNode::new_in(NodeType::Function, "outer", "sample.py", &ns);
    outer.line_start = Some(0);
    outer.line_end = Some(9);
    let mut nested = GraphNode::new_in(NodeType::Function, "nested", "sample.py", &ns);
    nested.line_start = Some(2);
    nested.line_end = Some(4);
    let (file_id, outer_id, nested_id) = (file.id.clone(), outer.id.clone(), nested.id.clone());
    graph.insert_nodes_batch(&[file, outer, nested]).unwrap();

    let view = FieldAccessGraphView::new(&graph);
    assert_eq!(view.enclosing_symbol_id("sample.py", 3), Some(nested_id));
    assert_eq!(view.enclosing_symbol_id("sample.py", 1), Some(outer_id));
    assert_eq!(view.file_node_id("sample.py"), Some(file_id));
    assert_eq!(view.enclosing_symbol_id("missing.py", 1), None);
}

#[test]
fn incomplete_consumers_are_collected_for_one_batch_update() {
    use crate::federation::contracts::model::{CallVia, HttpMethod, HostPart, MethodSpec, NormalizedUrl};
    let tmp = tempfile::tempdir().unwrap();
    let graph = GraphDatabase::new(&tmp.path().join("graph.bin")).unwrap();
    let ns = RepoNamespace::for_test();
    let mut ids = Vec::new();
    for name in ["call-a", "call-b"] {
        let mut node = GraphNode::new_in(NodeType::HttpClientCall, name, "send.py", &ns);
        node.contract = vec![ContractFact::Consumer(ConsumerFact {
            method: MethodSpec::Known(HttpMethod::Get),
            url: NormalizedUrl { host: HostPart::Literal("orders".into()), template: Some("/api".into()) },
            via: CallVia::Library { name: "requests".into() },
            url_expr: "\"http://orders/api\"".into(),
            reads_complete: true,
        })];
        ids.push(node.id.clone());
        graph.upsert_node(node).unwrap();
    }
    let patches = collect_consumer_patches(&graph, &ids).unwrap();
    assert_eq!(patches.len(), 2);
    assert!(patches.iter().all(|node| {
        matches!(node.contract.first(), Some(ContractFact::Consumer(fact)) if !fact.reads_complete)
    }));
}
```

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test path_index_uses_innermost_function_and_file_fallback --lib`

Expected: FAIL because the graph view and patch collector do not exist.

- [ ] **Step 2: Build one graph view per sensor pass**

Define:

```rust
struct FieldAccessGraphView {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    fn_index: BTreeMap<String, usize>,
    callers_of: BTreeMap<String, BTreeSet<String>>,
    calls_by_function: BTreeMap<String, BTreeSet<String>>,
    path_index: BTreeMap<String, Vec<usize>>,
}

impl FieldAccessGraphView {
    fn new(graph: &GraphDatabase) -> Self {
        let nodes = graph.get_all_nodes();
        let edges = graph.all_edges();
        Self::from_snapshots(nodes, edges)
    }
}
```

`from_snapshots` builds every index once. `path_index` stores node indices, not references into `nodes`, so the view is not self-referential. `enclosing_symbol_id` retains the existing smallest-range/later-start rule and NodeType filtering.

Add `view: FieldAccessGraphView` to `FieldAccessAnalysis`, initialize it once in `new`, and pass `&self.view` through call collection and scope analysis. Keep public test wrappers such as `detect_emissions` source-compatible by constructing the view once inside the wrapper. Set `FieldAccessMetrics.graph_snapshots = 1` when the sensor-level `FieldAccessAnalysis` creates this view; no other stage changes that counter.

- [ ] **Step 3: Reuse the path index and batch consumer updates**

Change `build_emission` to resolve `reads_field_source` through `FieldAccessGraphView::enclosing_symbol_id(...).or_else(|| view.file_node_id(...))`; it must no longer call `enclosing_symbol(graph, ...)` for each read.

Replace `patch_reads_complete` with `collect_consumer_patches(graph: &GraphDatabase, call_ids: &[String]) -> Result<Vec<GraphNode>, LainError>`. It clones each matching node, flips only `ConsumerFact.reads_complete` from `true` to `false`, and does not mutate the graph. Build the ID list with the same predicate as the old loop and apply all updates once, before `replace_sensor_output`:

```rust
let incomplete_call_ids: Vec<String> = emissions
    .iter()
    .filter(|emission| !emission.reads_complete && !emission.call_id.is_empty())
    .map(|emission| emission.call_id.clone())
    .collect();
let patches = collect_consumer_patches(graph, &incomplete_call_ids)?;
graph.upsert_nodes_batch(patches)?;
```

Do not deduplicate FieldRef nodes or edges.

- [ ] **Step 4: Verify graph output and metrics**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test path_index --lib
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test payload_field_lineage
LAIN_STARTUP_REPO_ROOT="$PWD" \
LAIN_STARTUP_DIGEST_FILE=/tmp/lain-startup-baseline.digest \
  cargo test --release --test startup_remediation real_checkout_digest_matches_baseline -- --ignored --nocapture
```

Expected: all pass. `FieldAccessMetrics.graph_snapshots == 1`, and path-index builds occur once per referenced path.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/field_access_sensor.rs src/server/sensors/field_access_sensor/reuse_tests.rs
git commit -m "Answer: Index field access graph lookups"
```

### Task 6: Add Shared-Content Scanning and Migrate `field_access`

**Files:**
- Modify: `src/server/sensors/util.rs:204-302,1387-1485`
- Modify: `src/server/sensors/field_access_sensor.rs`
- Modify: `src/server/sensors/dynamic_dispatch_sensor.rs:248-280`

**Interfaces:**
- Consumes: the existing per-pass `WorkspaceScanCache` and `Arc<str>` entries.
- Produces: `read_shared_or_fs(root, path) -> Option<Arc<str>>` and `scan_files_shared(...) -> impl Iterator<Item = (PathBuf, Arc<str>, T)>`; legacy `scan_files` remains available.

- [ ] **Step 1: Write failing shared-allocation and boundary tests**

Add to `util.rs` tests:

```rust
#[test]
fn shared_scan_reuses_the_same_source_allocation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("x.py"), "x = 1\n").unwrap();
    with_workspace_scan_cache(dir.path(), || {
        let first = read_shared_or_fs(dir.path(), &dir.path().join("x.py")).unwrap();
        let second = read_shared_or_fs(dir.path(), &dir.path().join("x.py")).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(&*first, "x = 1\n");
    });
}

#[test]
fn shared_content_keeps_the_two_mib_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.txt");
    std::fs::write(&path, "a".repeat(2 * 1024 * 1024)).unwrap();
    with_workspace_scan_cache(dir.path(), || {
        let a = read_shared_or_fs(dir.path(), &path).unwrap();
        let b = read_shared_or_fs(dir.path(), &path).unwrap();
        assert!(std::sync::Arc::ptr_eq(&a, &b));
    });
}
```

Retain the existing over-limit, missing-file, and changed-file tests. Add a non-UTF-8 fixture and assert `read_shared_or_fs` returns `None` and remembers the miss within the pass.

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test shared_scan_reuses_the_same_source_allocation --lib`

Expected: FAIL because `read_shared_or_fs` does not exist.

- [ ] **Step 2: Implement the shared API while preserving legacy callers**

Add:

```rust
pub fn read_shared_or_fs(root: &Path, path: &Path) -> Option<std::sync::Arc<str>> {
    const MAX_CACHEABLE_BYTES: usize = 2 * 1024 * 1024;
    let cached = WORKSPACE_SCAN_CACHE.with(|cell| {
        let borrow = cell.borrow();
        let cache = borrow.as_ref().filter(|cache| cache.root == root)?;
        cache.file_contents.get(path).cloned()
    });
    if let Some(entry) = cached {
        return entry;
    }
    let content = std::fs::read_to_string(path)
        .ok()
        .map(|text| std::sync::Arc::<str>::from(text.as_str()));
    WORKSPACE_SCAN_CACHE.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if let Some(cache) = borrow.as_mut().filter(|cache| cache.root == root) {
            match &content {
                Some(text) if text.len() <= MAX_CACHEABLE_BYTES => {
                    cache.file_contents.insert(path.to_path_buf(), content.clone());
                }
                None => {
                    cache.file_contents.insert(path.to_path_buf(), None);
                }
                _ => {}
            }
        }
    });
    content
}

pub fn scan_files_shared<'a, T: 'a>(
    root: &'a Path,
    select: impl Fn(&Path) -> Option<T> + 'a,
) -> impl Iterator<Item = (std::path::PathBuf, std::sync::Arc<str>, T)> + 'a {
    walk_workspace(root).filter_map(move |entry| {
        let path = entry.path();
        let tag = select(path)?;
        let content = read_shared_or_fs(root, path)?;
        Some((path.to_path_buf(), content, tag))
    })
}
```

Implement legacy `scan_files` by mapping `scan_files_shared` and calling `content.to_string()` so unmigrated behavior is unchanged. Implement `read_cached_or_fs` through the same shared helper to keep one cache implementation.

- [ ] **Step 3: Migrate `field_access` and dynamic dispatch**

Use `scan_files_shared` in `field_access_sensor.rs` and `dynamic_dispatch_sensor.rs`. Deref coercion supplies `&str`/`&[u8]` to existing regex and parser calls; explicitly use `content.as_bytes()` where the receiver type is ambiguous.

Change `ParsedFiles::ensure` to call `super::util::read_shared_or_fs` and store its `Arc<str>`; no new unqualified import is needed. Keep each parsed `Tree` local to `field_access`; only source text is shared.

- [ ] **Step 4: Verify allocation behavior and graph output**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test shared_ --lib
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test payload_field_lineage
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test dynamic_dispatch --lib
LAIN_STARTUP_REPO_ROOT="$PWD" \
LAIN_STARTUP_DIGEST_FILE=/tmp/lain-startup-baseline.digest \
  cargo test --release --test startup_remediation real_checkout_digest_matches_baseline -- --ignored --nocapture
```

Expected: all pass, `Arc::ptr_eq` is true within the pass, and caches are gone afterward.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/util.rs src/server/sensors/field_access_sensor.rs src/server/sensors/dynamic_dispatch_sensor.rs
git commit -m "Answer: Share field access source contents"
```

### Task 7: Migrate Hot Sensors and Rayon Blocks to Shared Content

**Files:**
- Modify: `src/server/sensors/http_sensor.rs:900-923`
- Modify: `src/server/sensors/http_client_sensor.rs:145-166`
- Modify: `src/server/sensors/grpc_consumer_sensor.rs:108-120,163-170`
- Modify: `src/server/sensors/grpc_consumer_sensor/rust.rs:161-176`
- Modify: `src/server/sensors/util_verification.rs`

**Interfaces:**
- Consumes: Task 6's `scan_files_shared`.
- Produces: identical route/client/candidate results with `Arc<str>` passed safely through existing Rayon `par_iter` blocks.

- [ ] **Step 1: Add the Rayon shared-content integration test**

Add to `util_verification.rs`, importing `rayon::prelude::*` alongside the existing `super::*` import:

```rust
#[test]
fn shared_blocks_are_send_and_preserve_file_order() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.py", "b.py", "c.py"] {
        std::fs::write(dir.path().join(name), format!("name = '{name}'\n")).unwrap();
    }
    with_workspace_scan_cache(dir.path(), || {
        let mut files: Vec<_> = scan_files_shared(dir.path(), |p| {
            p.extension()?.to_str()?.eq("py").then_some(())
        }).collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let bodies: Vec<String> = files
            .par_iter()
            .map(|(_, content, _)| content.trim().to_string())
            .collect();
        assert_eq!(bodies, vec!["name = 'a.py'", "name = 'b.py'", "name = 'c.py'"]);
    });
}
```

Run this baseline test after Task 6 and again after the migrations. It must pass both times; the migration itself is a no-output-change refactor covered by the sensor suites below.

- [ ] **Step 2: Migrate HTTP and HTTP-client blocks**

Change both sensors to `scan_files_shared`. Keep their 256-file blocks, query construction, `par_iter` ordering, and graph mutation exactly as written. Rely on deref coercion at `detect_calls_with_queries(path, &content, ...)` and `scan_file_for_routes_with_queries(path, &content, ...)`.

- [ ] **Step 3: Migrate Rust and non-Rust gRPC scans**

Change `discover_rust_grpc_candidates`, `GrpcConsumerSensor::scan_with_report`'s Rust-file enumeration, and the non-Rust `detect_stub_calls` loop to `scan_files_shared`. Keep path normalization, candidate sorting, and compiled-config reuse unchanged.

- [ ] **Step 4: Verify all migrated sensors**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test shared_blocks_are_send_and_preserve_file_order --lib
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test rust_grpc_candidates --test http_client_sensor --test http_sensor
LAIN_STARTUP_REPO_ROOT="$PWD" \
LAIN_STARTUP_DIGEST_FILE=/tmp/lain-startup-baseline.digest \
  cargo test --release --test startup_remediation real_checkout_digest_matches_baseline -- --ignored --nocapture
```

If the named HTTP integration targets do not exist, run `cargo test http_client --lib && cargo test http_sensor --lib` instead; do not create duplicate test targets. Expected: all pass with deterministic output.

- [ ] **Step 5: Commit**

```bash
git add src/server/sensors/http_sensor.rs src/server/sensors/http_client_sensor.rs src/server/sensors/grpc_consumer_sensor.rs src/server/sensors/grpc_consumer_sensor/rust.rs src/server/sensors/util_verification.rs
git commit -m "Answer: Share source contents across hot sensors"
```

### Task 8: Build Rust gRPC Indexes Only on a Usable Match

**Files:**
- Modify: `src/server/sensors/grpc_consumer_sensor/rust.rs:187-255,275-290` and its private test module

**Interfaces:**
- Consumes: existing `CompiledConfig`, `QueryCursor`, deny-list, and `Context::index`.
- Produces: private `RustGrpcMetrics { files_indexed }` and lazy index construction on the first usable match.

- [ ] **Step 1: Write the failing lazy-index test**

Add `#[cfg(test)] mod lazy_index_tests` inside `rust.rs` so the test can use the private detector directly:

```rust
use super::*;

#[test]
fn files_without_a_query_match_skip_identity_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let config = RustGrpcConfig::load(dir.path()).unwrap();
    let mut metrics = RustGrpcMetrics::default();
    let calls = detect_with_metrics(
        "fn plain() { let value = 1; }",
        "plain.rs",
        &config,
        &mut metrics,
    ).unwrap();
    assert!(calls.is_empty());
    assert_eq!(metrics.files_indexed, 0);

    let calls = detect_with_metrics(
        "fn f() { client.get(req); }",
        "call.rs",
        &config,
        &mut metrics,
    ).unwrap();
    assert!(calls.is_empty());
    assert_eq!(metrics.files_indexed, 1);
}
```

Run: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test files_without_a_query_match_skip_identity_indexing --lib`

Expected: FAIL because indexing currently happens before the query loop.

- [ ] **Step 2: Move index construction behind capture and deny checks**

Add:

```rust
#[derive(Debug, Default)]
pub(crate) struct RustGrpcMetrics {
    pub(crate) files_indexed: usize,
}
```

Thread `&mut RustGrpcMetrics` through an internal `detect_with_metrics` wrapper. Keep the public detector as a default-metrics wrapper. Replace eager `ctx.index(...)` with:

```rust
let mut indexed = false;
// inside the match loop, after call/receiver/method capture and deny check:
if !indexed {
    ctx.index(tree.root_node(), SyntaxContext::default());
    metrics.files_indexed += 1;
    indexed = true;
}
```

Do not index for a query miss, an incomplete capture, or a denied method. Evaluate `ctx.expression_type` only after this block.

- [ ] **Step 3: Verify candidate equivalence and the no-match override**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test rust_grpc_candidates
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test grpc_resolution
```

Expected: all pass, including large-file scoping, evidence preservation, constructor/accessor resolution, and the lazy-index counter test.

- [ ] **Step 4: Commit**

```bash
git add src/server/sensors/grpc_consumer_sensor/rust.rs
git commit -m "Answer: Index Rust gRPC files only on candidates"
```

### Task 9: Run the Full Gates and Perform the Controlled Rollout

**Files:**
- Modify: `docs/USER_MANUAL.md` only if Task 2's compatibility text needs clarification discovered during verification.
- Modify: `CHANGELOG.md` with one final performance bullet, if not already covered.

**Interfaces:**
- Consumes: all earlier tasks, the Task 1 digest and RSS baseline, and `LAIN_STARTUP_*` benchmark inputs.
- Produces: a verified release candidate and an operator-approved daemon restart; no additional production redesign.

- [ ] **Step 1: Run focused correctness and architecture checks**

Run:

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --lib field_access
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --test payload_field_lineage --test rust_grpc_candidates --test grpc_resolution --test shared_daemon_mcp --test startup_remediation
python3 scripts/check-no-duplicate-sensors.py
git diff --check
```

Expected: all pass. Confirm no test-only metrics or benchmark helper is included in production graph output.

- [ ] **Step 2: Run the 10-second gate three times and compare the graph**

Run:

```bash
LAIN_STARTUP_REPO_ROOT="$PWD" \
LAIN_STARTUP_DIGEST_FILE=/tmp/lain-startup-baseline.digest \
  cargo test --release --test startup_remediation real_checkout_digest_matches_baseline -- --ignored --nocapture

LAIN_STARTUP_REPO_ROOT="$PWD" \
LAIN_STARTUP_BUDGET_MS=10000 \
LAIN_STARTUP_BASELINE_RSS_BYTES=<largest baseline recorded in Task 1> \
  cargo test --release --test performance_budgets real_checkout_static_ready_under_configured_budget -- --ignored --nocapture
```

Expected: the digest matches exactly. Every one of the three fresh-data runs reaches full static readiness in ≤10s, `field_access` logs ≤4s, all other work totals ≤5s, and peak RSS is ≤110% of baseline. If any gate fails, stop: use the stage metrics to select a new bounded optimization and do not perform the rollout.

- [ ] **Step 3: Verify warm restart, incremental edit, and default LSP separately**

Run the real server once with a persisted temporary graph, record warm readiness, then edit one tracked source file in a disposable worktree and confirm the next index changes only the expected graph facts. Run once more without `LAIN_DISABLE_LSP`; record LSP readiness separately and assert MCP `initialize` and `tools/list` answer while indexing continues.

Do not claim the static 10s gate covers LSP. Delete the disposable worktree and all temporary graphs afterward.

- [ ] **Step 4: Run the project verification battery**

Run:

```bash
cargo fmt --all -- --check
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo clippy --all-targets -- -D warnings
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo test --release
scripts/demo.sh
```

Expected: all pass. If the full suite exposes an unrelated pre-existing failure, reproduce it on the base revision and report it rather than expanding this plan.

- [ ] **Step 5: Document the final measured result and commit documentation**

Add one `Unreleased` bullet to `CHANGELOG.md`:

```markdown
- Reduce repeated field-lineage, graph-lookup, and source-content work during static indexing, and build Rust gRPC identity indexes only for candidate files; preserve canonical graph output and keep full static readiness within the 10-second target on the reference checkout.
```

Commit only the intentional documentation changes:

```bash
git add CHANGELOG.md docs/USER_MANUAL.md
git commit -m "Answer: Document startup remediation and compatibility"
```

- [ ] **Step 6: Restart the stale daemon with operator approval**

Revalidate current processes:

```bash
ps -C lain -o pid,ppid,lstart,etime,pcpu,rss,args
git rev-parse --short HEAD
target/release/lain doctor --json --workspace "$PWD"
```

Confirm the fresh binary's `build_commit` matches `HEAD`, then present the exact daemon PIDs and install path to the operator for approval. Stop MCP clients gracefully, send `TERM` only to the revalidated stale daemon PIDs, and start one daemon through the operator's existing install/configuration flow. Do not kill by name or use the PIDs from the earlier investigation.

Verify the new `lain/daemon_info` identity matches the client, `get_capabilities` becomes ready, and no old daemon remains:

```bash
ps -C lain -o pid,ppid,lstart,etime,pcpu,rss,args
```

Remove `/tmp/lain-startup-baseline.digest` only after recording the final comparison in the PR evidence. Remove all other temporary logs, graphs, and worktrees. Confirm `git status --short` shows only intentional source, test, and documentation changes.
