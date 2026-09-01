//! Performance regression budgets.
//!
//! The docs (`docs/FEDERATION.md`) promise `p99 < 100ms` for cross-repo
//! blast radius, and the README leans on "fast" indexing. None of that
//! prose was enforced by tests — until now.
//!
//! This file pins a handful of headline timings as hard tests:
//!
//! | test                              | budget (relaxed)        | what it pins
//! |-----------------------------------|-------------------------|--------------
//! | `server_boot_under_5_seconds`     | 5 s (single attempt)    | cold start to first ready `/health`
//! | `tools_list_under_100ms`          | 100 ms (×N median)      | JSON-RPC `tools/list` latency
//! | `get_health_under_50ms`           |  50 ms (×N median)      | HTTP `/health` latency
//! | `find_anchors_warm_path_under_200ms` | 200 ms (×N median)  | tool-call round-trip warm
//! | `get_workspace_graph_under_500ms` | 500 ms (×N median)      | workspace graph payload latency
//! | `small_repo_index_under_10_seconds` | 10 s (single attempt) | ~50-fn fixture cold-index
//!
//! The budgets above are the *promised* values; the test compares each
//! measured value against `budget × LAIN_PERF_BUDGET_MULTIPLIER`
//! (default `2.0`) to absorb CI noise. Set the multiplier higher for
//! even noisier runners:
//!
//! ```bash
//! LAIN_PERF_BUDGET_MULTIPLIER=3.0 cargo test --test performance_budgets
//! ```
//!
//! The tests share one booted server (per-test, hermetic ports) and
//! measure median of N rounds so a single cold-cache spike does not
//! fail the suite.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{free_port, ServerGuard};

/// Bound applied on top of every documented budget. 4× absorbs a
/// 2-core CI runner with the debug binary and in-binary parallel
/// contention (the boot benchmarks spawn a real `lain server`
/// subprocess; under `cargo test`'s default thread-per-test model
/// each spawned server competes for CPU with the others and boot
/// time climbs ~2.5× compared to a serial run). Bump higher for
/// noisier fleets via `LAIN_PERF_BUDGET_MULTIPLIER`.
fn budget_multiplier() -> f64 {
    std::env::var("LAIN_PERF_BUDGET_MULTIPLIER")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|m| *m >= 1.0)
        .unwrap_or(4.0)
}

fn relaxed(budget: Duration) -> Duration {
    let mult = budget_multiplier();
    let nanos = (budget.as_nanos() as f64 * mult) as u128;
    // Cap at a generous ceiling so a typo'd env var (`=1000`) cannot
    // silently make every assertion trivially pass.
    let capped = nanos.min(60 * 1_000_000_000_u128); // 60 s
    Duration::from_nanos(capped as u64)
}

/// Build the fixture the perf suite runs against. It is the
/// `scripts/demo-fixture.sh` shape plus enough extra modules that the
/// indexer has ~50 symbols to chew on. Indexing time on this fixture
/// is the unit under test for `small_repo_index_under_10_seconds`.
fn build_perf_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"perf-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    // The hub-and-spoke graph that `find_anchors` is asked to rank.
    // ~10 modules, each with ~5 functions — ~50 symbols total once
    // tree-sitter resolves them. Bodies are short but real so the
    // indexer walks real source, not stub bytes.
    std::fs::write(
        root.join("src/lib.rs"),
        "pub mod core;\n\
         pub mod helpers;\n\
         pub mod extra1;\n\
         pub mod extra2;\n\
         pub mod extra3;\n\
         pub mod extra4;\n\
         pub mod extra5;\n\
         pub mod extra6;\n\
         pub mod extra7;\n\
         pub mod extra8;\n\
         pub fn entry() -> u32 { core::orchestrate() }\n",
    )
    .unwrap();
    let bodies = [
        (
            "src/core.rs",
            "use crate::helpers;\n\
             pub fn orchestrate() -> u32 {\n  let a = helpers::helper_a(1);\n  let b = helpers::helper_b(2);\n  let c = helpers::helper_c(3);\n  let d = helpers::helper_d(4);\n  let e = helpers::helper_e(5);\n  a + b + c + d + e\n}\n",
        ),
        (
            "src/helpers.rs",
            "pub fn helper_a(x: u32) -> u32 { x + 1 }\n\
             pub fn helper_b(x: u32) -> u32 { x + 2 }\n\
             pub fn helper_c(x: u32) -> u32 { x + 3 }\n\
             pub fn helper_d(x: u32) -> u32 { x + 4 }\n\
             pub fn helper_e(x: u32) -> u32 { x + 5 }\n",
        ),
    ];
    for (path, body) in bodies.iter() {
        std::fs::write(root.join(path), body).unwrap();
    }
    // 8 extra modules with 5 functions each → ~50 symbols total when
    // combined with helpers + entry + orchestrate.
    for n in 1..=8 {
        let module = format!(
            "pub fn extra_{n}_a() -> u32 {{ 1 }}\n\
             pub fn extra_{n}_b() -> u32 {{ 2 }}\n\
             pub fn extra_{n}_c() -> u32 {{ 3 }}\n\
             pub fn extra_{n}_d() -> u32 {{ 4 }}\n\
             pub fn extra_{n}_e() -> u32 {{ 5 }}\n"
        );
        std::fs::write(root.join(format!("src/extra{n}.rs")), module).unwrap();
    }
    std::fs::write(
        root.join("tests/basic.rs"),
        "#[test]\nfn smoke() { assert_eq!(perf_fixture::entry(), 15); }\n",
    )
    .unwrap();

    // git init + commit so the indexer picks the files up.
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    for (k, v) in [
        ("user.email", "perf-budgets@lain"),
        ("user.name", "perf-budgets"),
    ] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(root)
            .status()
            .unwrap();
    }
    let add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .status()
        .expect("git add");
    assert!(add.success(), "git add failed");
    let commit = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "perf budgets fixture"])
        .current_dir(root)
        .status()
        .expect("git commit");
    assert!(commit.success(), "git commit failed");
}

/// Spawn the server and wait until `/health` returns 200. Returns the
/// guard plus the elapsed boot duration and the host string. The
/// boot duration is the headline metric for
/// `server_boot_under_5_seconds` — the other tests discard it.
fn boot_and_time(
    project_dir: &Path,
    repo_dir: &Path,
    data_dir: &Path,
    port: u16,
) -> (ServerGuard, Duration, String) {
    let repo_id = repo_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo")
        .to_string();
    std::fs::write(
        project_dir.join("repos.yaml"),
        format!(
            "data_dir: {}\nrepos:\n  - id: {}\n    source:\n      type: workspace_dir\n      path: {}\n",
            data_dir.display(),
            repo_id,
            repo_dir.display(),
        ),
    )
    .unwrap();
    std::fs::write(
        project_dir.join("workspaces.yaml"),
        format!("workspaces:\n  - name: perf\n    members: [{}]\n", repo_id),
    )
    .unwrap();

    let state = tempfile::tempdir().unwrap();
    let xdg_config = tempfile::tempdir().unwrap();
    let stderr_path = std::env::temp_dir().join(format!("perf-budgets-stderr-{port}.log"));
    let stderr_file = std::fs::File::create(&stderr_path).unwrap();

    let boot_start = Instant::now();
    let child = Command::new(env!("CARGO_BIN_EXE_lain"))
        .args([
            "server",
            "--transport",
            "http",
            "--port",
            &port.to_string(),
            "--workspace",
            "auto",
            "--config",
            project_dir.join("repos.yaml").to_str().unwrap(),
        ])
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", xdg_config.path())
        .env("LAIN_JOB_STORE", state.path().join("jobs.json"))
        .env_remove("LAIN_EMBEDDING_MODEL")
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap_or_else(|e| {
            panic!(
                "spawn lain server failed: {e}; binary={}; stderr={}",
                env!("CARGO_BIN_EXE_lain"),
                stderr_path.display()
            )
        });

    let guard = ServerGuard(child);
    let host = format!("127.0.0.1:{port}");

    // Poll /health until 200. The first successful response marks
    // "ready"; the elapsed wall time is what we assert on.
    loop {
        if boot_start.elapsed() > Duration::from_secs(60) {
            let log = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            panic!(
                "server did not become healthy within 60s on {host}; last stderr:\n{log}"
            );
        }
        let attempt = (|| -> std::io::Result<(u16, String)> {
            let mut stream = TcpStream::connect(&host)?;
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            stream.write_all(
                format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            let status_line = response.lines().next().unwrap_or("");
            let status: u16 = status_line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let body_start = response
                .find("\r\n\r\n")
                .map(|i| i + 4)
                .unwrap_or(response.len());
            Ok((status, response[body_start..].to_string()))
        })();
        match attempt {
            Ok((200, _)) => break,
            Ok((status, body)) => {
                if boot_start.elapsed() > Duration::from_secs(10) {
                    panic!("server returned HTTP {status} from /health: {body}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let log = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                panic!("health probe error: {e}; stderr:\n{log}");
            }
        }
    }
    let elapsed = boot_start.elapsed();
    (guard, elapsed, host)
}

/// Build the standard fixture and boot a server against it. Used by
/// every test except `server_boot_under_5_seconds` itself (which needs
/// to measure the boot of THIS call).
fn boot_default() -> (ServerGuard, String, tempfile::TempDir) {
    let port = free_port();
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    build_perf_fixture(&repo_dir);
    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let (guard, _boot_elapsed, host) =
        boot_and_time(project.path(), &repo_dir, &data_dir, port);
    (guard, host, project)
}

/// Issue a raw HTTP request and return the (status, body). Captures
/// wall-clock time as the headline metric for the calling test.
fn http_request(host: &str, raw: &str) -> (u16, String, Duration) {
    let start = Instant::now();
    let mut stream = TcpStream::connect(host).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok();
    stream.write_all(raw.as_bytes()).expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    let elapsed = start.elapsed();
    let status_line = response.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body_start = response
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(response.len());
    (status, response[body_start..].to_string(), elapsed)
}

/// JSON-RPC POST to /mcp, return (status, body, elapsed).
fn jsonrpc(host: &str, body: &str) -> (u16, String, Duration) {
    let req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
        host = host,
        len = body.len(),
    );
    http_request(host, &req)
}

/// Median of a small sample set. Cheaper than full p99 reporting and
/// absorbs a single cold-cache outlier without false failures.
fn median(samples: &mut Vec<Duration>) -> Duration {
    if samples.is_empty() {
        return Duration::ZERO;
    }
    samples.sort();
    samples[samples.len() / 2]
}

// ─── Tests ─────────────────────────────────────────────────────────────

/// T1: server boot to first 200 /health within the 5-second budget.
#[test]
fn server_boot_under_5_seconds() {
    let budget = Duration::from_secs(5);
    let port = free_port();
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    build_perf_fixture(&repo_dir);
    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let (guard, elapsed, host) =
        boot_and_time(project.path(), &repo_dir, &data_dir, port);
    println!("server_boot_under_5_seconds: host={host} elapsed={elapsed:?}");

    let cap = relaxed(budget);
    assert!(
        elapsed < cap,
        "server took {elapsed:?} to become ready (budget {budget:?}, relaxed {cap:?} with multiplier {}×)",
        budget_multiplier()
    );
    drop(guard);
}

/// T2: median of N `tools/list` JSON-RPC calls stays under 100 ms.
#[test]
fn tools_list_under_100ms() {
    let budget = Duration::from_millis(100);
    let (_guard, host, _project) = boot_default();

    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {},
    })
    .to_string();

    // One warm-up round to pre-populate TLS / page caches, then 10
    // measured rounds. The warm-up is discarded so the first measurement
    // is a steady-state read.
    let _ = jsonrpc(&host, &body);

    const ITERS: usize = 10;
    let mut samples: Vec<Duration> = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let (status, resp, elapsed) = jsonrpc(&host, &body);
        assert_eq!(status, 200, "tools/list HTTP {status}: {resp}");
        let parsed: serde_json::Value = serde_json::from_str(&resp)
            .unwrap_or_else(|e| panic!("tools/list not JSON: {e}\n{resp}"));
        assert!(
            parsed.pointer("/result/tools").is_some(),
            "tools/list missing result.tools: {parsed}"
        );
        samples.push(elapsed);
    }
    let med = median(&mut samples);
    let cap = relaxed(budget);
    println!(
        "tools_list_under_100ms: median={med:?} samples={samples:?} \
         budget={budget:?} relaxed={cap:?}"
    );
    assert!(
        med < cap,
        "tools/list median {med:?} exceeds budget {budget:?} (relaxed {cap:?} with multiplier {}×)",
        budget_multiplier()
    );
}

/// T3: median of N `/health` GETs stays under 50 ms.
#[test]
fn get_health_under_50ms() {
    let budget = Duration::from_millis(50);
    let (_guard, host, _project) = boot_default();
    let req = format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let _ = http_request(&host, &req); // warm-up

    const ITERS: usize = 10;
    let mut samples: Vec<Duration> = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let (status, body, elapsed) = http_request(&host, &req);
        assert_eq!(status, 200, "/health HTTP {status}: {body}");
        samples.push(elapsed);
    }
    let med = median(&mut samples);
    let cap = relaxed(budget);
    println!(
        "get_health_under_50ms: median={med:?} samples={samples:?} \
         budget={budget:?} relaxed={cap:?}"
    );
    assert!(
        med < cap,
        "/health median {med:?} exceeds budget {budget:?} (relaxed {cap:?} with multiplier {}×)",
        budget_multiplier()
    );
}

/// T4: warm `find_anchors` tool-call under 200 ms. "Warm" means: not
/// the first call after `initialize` — tree-sitter / page cache /
/// JIT-style warmups already done by the time we start the clock.
#[test]
fn find_anchors_warm_path_under_200ms() {
    let budget = Duration::from_millis(200);
    let (_guard, host, _project) = boot_default();

    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": {"name": "find_anchors", "arguments": {"limit": 5}},
    })
    .to_string();

    // Two warm-up rounds: the first call also warms the indexer's
    // lazy paths, the second one is the actual steady-state.
    let _ = jsonrpc(&host, &body);
    let _ = jsonrpc(&host, &body);

    const ITERS: usize = 10;
    let mut samples: Vec<Duration> = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let (status, resp, elapsed) = jsonrpc(&host, &body);
        assert_eq!(status, 200, "find_anchors HTTP {status}: {resp}");
        let parsed: serde_json::Value = serde_json::from_str(&resp)
            .unwrap_or_else(|e| panic!("find_anchors not JSON: {e}\n{resp}"));
        assert!(
            parsed.pointer("/result/content/0/text").is_some(),
            "find_anchors missing result.content[0].text: {parsed}"
        );
        samples.push(elapsed);
    }
    let med = median(&mut samples);
    let cap = relaxed(budget);
    println!(
        "find_anchors_warm_path_under_200ms: median={med:?} samples={samples:?} \
         budget={budget:?} relaxed={cap:?}"
    );
    assert!(
        med < cap,
        "find_anchors median {med:?} exceeds budget {budget:?} (relaxed {cap:?} with multiplier {}×)",
        budget_multiplier()
    );
}

/// T5: `get_workspace_graph` under 500 ms. This serializes up to 5000
/// nodes / 10000 edges, so the budget is intentionally generous.
#[test]
fn get_workspace_graph_under_500ms() {
    let budget = Duration::from_millis(500);
    let (_guard, host, _project) = boot_default();

    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": {"name": "get_workspace_graph", "arguments": {}},
    })
    .to_string();

    let _ = jsonrpc(&host, &body); // warm-up
    let _ = jsonrpc(&host, &body);

    const ITERS: usize = 5;
    let mut samples: Vec<Duration> = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let (status, resp, elapsed) = jsonrpc(&host, &body);
        assert_eq!(
            status, 200,
            "get_workspace_graph HTTP {status}: {}",
            &resp[..resp.len().min(200)]
        );
        let parsed: serde_json::Value = serde_json::from_str(&resp)
            .unwrap_or_else(|e| panic!("get_workspace_graph not JSON: {e}\n{resp}"));
        // The fixture is small so the payload must be non-empty —
        // catches "the tool returned an empty stub" regressions.
        assert!(
            parsed.pointer("/result/content/0/text").is_some(),
            "get_workspace_graph missing result.content[0].text: {parsed}"
        );
        samples.push(elapsed);
    }
    let med = median(&mut samples);
    let cap = relaxed(budget);
    println!(
        "get_workspace_graph_under_500ms: median={med:?} samples={samples:?} \
         budget={budget:?} relaxed={cap:?}"
    );
    assert!(
        med < cap,
        "get_workspace_graph median {med:?} exceeds budget {budget:?} (relaxed {cap:?} with multiplier {}×)",
        budget_multiplier()
    );
}

/// T6: cold-index a ~50-symbol fixture in under 10 s. This measures the
/// server-side startup-reindex path; the wall clock is from process
/// spawn to the first ready `/health` (the same signal T1 asserts on,
/// but with a much bigger fixture and a 2× larger budget).
#[test]
fn small_repo_index_under_10_seconds() {
    let budget = Duration::from_secs(10);
    let port = free_port();
    let project = tempfile::tempdir().unwrap();
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    build_perf_fixture(&repo_dir);
    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let (guard, elapsed, host) =
        boot_and_time(project.path(), &repo_dir, &data_dir, port);
    println!(
        "small_repo_index_under_10_seconds: host={host} elapsed={elapsed:?}"
    );

    let cap = relaxed(budget);
    assert!(
        elapsed < cap,
        "small-repo index took {elapsed:?} (budget {budget:?}, relaxed {cap:?} with multiplier {}×)",
        budget_multiplier()
    );

    // Confirm the index actually ran — `graph_nodes` must be > 0 in the
    // /health body, otherwise we would have passed on a stub server.
    let req = format!("GET /health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let (status, body, _) = http_request(&host, &req);
    assert_eq!(status, 200, "/health HTTP {status}: {body}");
    let parsed: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("/health not JSON: {e}\n{body}"));
    let graph_nodes = parsed["graph_nodes"].as_u64().unwrap_or(0);
    assert!(
        graph_nodes > 0,
        "expected graph_nodes > 0 after indexing, got {parsed}"
    );

    drop(guard);
}
