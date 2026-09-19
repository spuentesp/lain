//! Bug #2 sidecar prototype — parent benchmark driver.
//!
//! Spawns `lain-git-sidecar` over a Unix domain socket, runs a
//! mix of `GitSensor` methods through it, and prints p50 / p95 / p99
//! latencies alongside an in-process baseline. The numbers decide
//! go/no-go on the full sidecar implementation.
//!
//! Usage:
//!
//! ```text
//! sidecar_bench <repo-path> [--iters N] [--socket PATH] [--warmup N]
//! ```
//!
//! If `--socket PATH` is given, the parent connects to an already-
//! running child at that path. Otherwise the parent spawns its own
//! child at a temp path and tears it down on exit.

use lain::git::GitSensor;
use lain::sidecar_proto::{read_frame, write_frame, Request, Response, PROTOCOL_VERSION};

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let repo_path = args
        .next()
        .ok_or("usage: sidecar_bench <repo-path> [--iters N] [--socket PATH] [--warmup N]")?;
    let repo_path = PathBuf::from(repo_path);

    let mut iters: usize = 1000;
    let mut warmup: usize = 50;
    let mut socket_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iters" => {
                iters = args.next().ok_or("--iters needs a value")?.parse()?;
            }
            "--warmup" => {
                warmup = args.next().ok_or("--warmup needs a value")?.parse()?;
            }
            "--socket" => {
                socket_path = Some(PathBuf::from(args.next().ok_or("--socket needs a value")?));
            }
            other => return Err(format!("unknown arg: {other}").into()),
        }
    }

    let (stream, child) = match socket_path {
        Some(p) => {
            eprintln!("sidecar_bench: connecting to existing child at {p:?}");
            (connect_with_retry(&p, Duration::from_secs(5))?, None)
        }
        None => {
            let socket = temp_socket_path();
            eprintln!("sidecar_bench: spawning child with socket {socket:?}");
            let child = spawn_child(&repo_path, &socket)?;
            (
                connect_with_retry(&socket, Duration::from_secs(10))?,
                Some((child, socket)),
            )
        }
    };

    let mut session = Session::new(stream);
    session.handshake()?;

    // Run baseline first (in-process, no IPC) so the comparison
    // table is in-process-vs-IPC, not the other way around.
    let baseline_sensor = GitSensor::new(&repo_path)?;
    let since_hash = baseline_sensor.get_latest_commit_info()?.0;

    println!("== Bug #2 sidecar prototype: IPC overhead ==");
    println!("repo:           {}", repo_path.display());
    println!("iterations:     {} (+ {} warmup)", iters, warmup);
    println!();

    // Warmup: ignore first N calls of each method to populate page
    // caches, fill CPU branch predictors, etc.
    run_method(
        &mut session,
        &baseline_sensor,
        Method::LatestCommitInfo,
        warmup,
    );
    run_method(
        &mut session,
        &baseline_sensor,
        Method::AllTrackedFiles,
        warmup,
    );
    if let Some(second) = second_commit(&baseline_sensor, &since_hash) {
        run_method(
            &mut session,
            &baseline_sensor,
            Method::ChangedFilesSince(second),
            warmup,
        );
    }

    println!(
        "{:<24}  {:>10}  {:>10}  {:>10}  {:>10}  {:>10}",
        "method", "baseline", "sidecar", "overhead", "overhead_p95", "verdict"
    );
    println!("{}", "-".repeat(92));

    let mut total_overhead_p95 = Duration::ZERO;
    let mut weights = 0usize;

    let baseline_p = bench_baseline(&baseline_sensor, Method::LatestCommitInfo, iters);
    let sidecar_p = run_method(
        &mut session,
        &baseline_sensor,
        Method::LatestCommitInfo,
        iters,
    );
    let overhead_p95 = report_row("GetLatestCommitInfo", baseline_p, sidecar_p);
    total_overhead_p95 += overhead_p95 * 1;
    weights += 1;

    let baseline_p = bench_baseline(&baseline_sensor, Method::AllTrackedFiles, iters);
    let sidecar_p = run_method(
        &mut session,
        &baseline_sensor,
        Method::AllTrackedFiles,
        iters,
    );
    let overhead_p95 = report_row("GetAllTrackedFiles", baseline_p, sidecar_p);
    total_overhead_p95 += overhead_p95 * 1;
    weights += 1;

    if let Some(second) = second_commit(&baseline_sensor, &since_hash) {
        let baseline_p = bench_baseline(
            &baseline_sensor,
            Method::ChangedFilesSince(second.clone()),
            iters,
        );
        let sidecar_p = run_method(
            &mut session,
            &baseline_sensor,
            Method::ChangedFilesSince(second),
            iters,
        );
        let overhead_p95 = report_row("GetChangedFilesSince", baseline_p, sidecar_p);
        total_overhead_p95 += overhead_p95 * 1;
        weights += 1;
    }

    let baseline_p = bench_baseline(&baseline_sensor, Method::AnalyzeCoChanges, iters);
    let sidecar_p = run_method(
        &mut session,
        &baseline_sensor,
        Method::AnalyzeCoChanges,
        iters,
    );
    let overhead_p95 = report_row("AnalyzeCoChanges", baseline_p, sidecar_p);
    total_overhead_p95 += overhead_p95 * 1;
    weights += 1;

    let baseline_p = bench_baseline(&baseline_sensor, Method::GetUncommittedChanges, iters);
    let sidecar_p = run_method(
        &mut session,
        &baseline_sensor,
        Method::GetUncommittedChanges,
        iters,
    );
    let overhead_p95 = report_row("GetUncommittedChanges", baseline_p, sidecar_p);
    total_overhead_p95 += overhead_p95 * 1;
    weights += 1;

    println!();
    let avg_overhead_p95 = total_overhead_p95 / weights as u32;
    let verdict = if avg_overhead_p95 < Duration::from_micros(500) {
        "GO (< 500 µs avg p95)"
    } else if avg_overhead_p95 < Duration::from_millis(2) {
        "MAYBE (500 µs–2 ms avg p95 — profile to find the cost)"
    } else {
        "NO-GO (> 2 ms avg p95)"
    };
    println!(
        "Average p95 overhead across 5 methods: {:?}",
        avg_overhead_p95
    );
    println!("Verdict: {verdict}");
    println!();

    // Clean shutdown: send Shutdown, wait for the child's exit.
    if let Some((mut child, socket)) = child {
        let _ = write_frame(&mut session.stream, &Request::Shutdown);
        let _ = session.stream.flush();
        let _ = child.wait();
        let _ = std::fs::remove_file(socket);
    }

    Ok(())
}

#[derive(Clone)]
enum Method {
    LatestCommitInfo,
    AllTrackedFiles,
    ChangedFilesSince(String),
    AnalyzeCoChanges,
    GetUncommittedChanges,
}

struct Session {
    stream: UnixStream,
}

impl Session {
    fn new(stream: UnixStream) -> Self {
        Self { stream }
    }

    fn handshake(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let req = Request::Handshake {
            version: PROTOCOL_VERSION,
        };
        write_frame(&mut self.stream, &req)?;
        self.stream.flush()?;
        match read_frame(&mut self.stream)? {
            Response::HandshakeAck { version } => {
                if version != PROTOCOL_VERSION {
                    return Err(format!(
                        "handshake ack version mismatch: expected {}, got {}",
                        PROTOCOL_VERSION, version
                    )
                    .into());
                }
                Ok(())
            }
            Response::HandshakeNack {
                expected,
                received,
                reason,
            } => Err(format!(
                "handshake nack: expected {}, received {}, reason: {}",
                expected, received, reason
            )
            .into()),
            other => Err(format!("expected handshake response, got: {:?}", other).into()),
        }
    }
}

/// Run the sidecar-side bench for `iters` calls of `method`,
/// returning p50/p95/p99 latencies in microseconds.
fn run_method(
    session: &mut Session,
    _sensor: &GitSensor,
    method: Method,
    iters: usize,
) -> Percentiles {
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let req = match &method {
            Method::LatestCommitInfo => Request::GetLatestCommitInfo,
            Method::AllTrackedFiles => Request::GetAllTrackedFiles,
            Method::ChangedFilesSince(since) => Request::GetChangedFilesSince {
                since_hash: since.clone(),
            },
            Method::AnalyzeCoChanges => Request::AnalyzeCoChanges {
                window: 50,
                min_pair: 2,
                max_files: 200,
            },
            Method::GetUncommittedChanges => Request::GetUncommittedChanges,
        };
        let started = Instant::now();
        write_frame(&mut session.stream, &req).expect("write_frame");
        session.stream.flush().expect("flush");
        let _resp: Response = read_frame(&mut session.stream).expect("read_frame");
        let elapsed = started.elapsed();
        // Round-trip latency = request write + child compute + response read.
        // Subtracting the in-process compute gives IPC overhead, but we
        // just report the full round-trip here and compare against the
        // baseline round-trip (in-process method call only) below.
        samples.push(elapsed);
    }
    percentiles(&mut samples)
}

/// Run the in-process baseline for `iters` calls of `method`. We
/// measure with a tight loop and no I/O so the numbers reflect
/// libgit2's pure compute cost.
fn bench_baseline(sensor: &GitSensor, method: Method, iters: usize) -> Percentiles {
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let started = Instant::now();
        match &method {
            Method::LatestCommitInfo => {
                let _ = sensor.get_latest_commit_info();
            }
            Method::AllTrackedFiles => {
                let _ = sensor.get_all_tracked_files();
            }
            Method::ChangedFilesSince(since) => {
                let _ = sensor.get_changed_files_since(since);
            }
            Method::AnalyzeCoChanges => {
                let _ = sensor.analyze_co_changes(50, 2, 200);
            }
            Method::GetUncommittedChanges => {
                let _ = sensor.get_uncommitted_changes();
            }
        }
        samples.push(started.elapsed());
    }
    percentiles(&mut samples)
}

struct Percentiles {
    p50: Duration,
    p95: Duration,
    p99: Duration,
}

fn percentiles(samples: &mut Vec<Duration>) -> Percentiles {
    samples.sort();
    let n = samples.len();
    let pick = |p: f64| -> Duration {
        if n == 0 {
            return Duration::ZERO;
        }
        let idx = ((n as f64) * p).ceil() as usize - 1;
        samples[idx.min(n - 1)]
    };
    Percentiles {
        p50: pick(0.50),
        p95: pick(0.95),
        p99: pick(0.99),
    }
}

fn report_row(label: &str, baseline: Percentiles, sidecar: Percentiles) -> Duration {
    let overhead_p95 = sidecar.p95.saturating_sub(baseline.p95);
    let verdict = if overhead_p95 < Duration::from_micros(500) {
        "ok"
    } else if overhead_p95 < Duration::from_millis(2) {
        "warn"
    } else {
        "FAIL"
    };
    // Print p50 / p95 / p99 for both sides plus the p95 overhead
    // (the threshold-based verdict) so the table is self-describing
    // without needing to read the field doc.
    println!(
        "{:<22}  baseline p50={:>9?} p95={:>9?} p99={:>9?}  sidecar p50={:>9?} p95={:>9?} p99={:>9?}  p95_overhead={:>9?}  {}",
        label,
        baseline.p50,
        baseline.p95,
        baseline.p99,
        sidecar.p50,
        sidecar.p95,
        sidecar.p99,
        overhead_p95,
        verdict,
    );
    overhead_p95
}

/// Pick the second-newest commit hash for `GetChangedFilesSince`.
/// If HEAD is the only commit, skip that benchmark.
fn second_commit(sensor: &GitSensor, head: &str) -> Option<String> {
    let commits = sensor.get_commit_history(2).ok()?;
    let second = commits.into_iter().find(|c| c.id != head)?;
    Some(second.id)
}

fn temp_socket_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.push(format!(
        "lain-sidecar-{}-{}.sock",
        std::process::id(),
        nanos
    ));
    path
}

fn spawn_child(repo: &Path, socket: &Path) -> Result<Child, Box<dyn std::error::Error>> {
    // `current_exe()` returns `target/release/sidecar_bench` (this bin).
    // The child is a sibling binary in the same target dir, so we just
    // swap the basename. This works whether the parent was launched
    // directly or via `cargo run --bin sidecar_bench`.
    let exe = std::env::current_exe()?;
    let child_exe = exe.with_file_name("lain-git-sidecar");
    let child = Command::new(child_exe)
        .arg(repo)
        .arg(socket)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    Ok(child)
}

fn connect_with_retry(
    socket: &Path,
    timeout: Duration,
) -> Result<UnixStream, Box<dyn std::error::Error>> {
    let started = Instant::now();
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(e) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
                let _ = e;
            }
            Err(e) => return Err(Box::new(e)),
        }
    }
}
