//! Zero-config shared daemon for `lain mcp` stdio (2026-10-17).
//!
//! Out of the box, N agents launching `{"command":"lain","args":["mcp"]}`
//! share ONE resident daemon: the stdio process either attaches to a
//! live daemon that already serves its workspace set (or a superset) or
//! starts a detached one, then bridges stdin↔socket JSON-RPC to it
//! transparently. A late client whose workspaces are NOT covered hands
//! the running daemon the missing paths via `add_workspaces`, which
//! hot-adds them to the served federation — then bridges. Only one
//! index, one memory, one process group per machine.
//!
//! ## Keying and discovery
//! The daemon's socket is keyed by the resolved workspace SET
//! ([`crate::config::shared_socket_path`]): same set → same socket →
//! start-race resolved by `bind_exclusive` (the loser connects to the
//! winner). Coverage is decided by asking each live daemon its served
//! set over `lain/daemon_info` — a socket filename alone cannot know
//! that a daemon grew past its initial set through `add_workspaces`.
//!
//! ## Fallback contract
//! Every failure path (non-Unix, opted out via `LAIN_MCP_NO_SHARE=1`
//! or `--no-share`, socket path too long, daemon failed to come up,
//! `add_workspaces` refused) falls back to the historical private
//! `lain mcp` server, so an agent is never worse off than before.
//!
//! ## Identity contract (2026-11-05)
//! One exception: a daemon whose build identity differs from the
//! client's is not a failure path but a refusal. `lain/daemon_info`
//! reports `version`, `build_commit` and `index_build_id`; a client
//! bridges only when all three match its own. A mismatch — or a
//! daemon too old to report them — is an error naming the socket and
//! both identities, and the client starts NO daemon beside it: a
//! second indexer would burn CPU answering with a different build.
//! The operator restarts the daemon, or asks for a private instance
//! with `--no-share` / `LAIN_MCP_NO_SHARE=1`.
//!
//! ## No-share instances (2026-10-18)
//! A daemon whose own config carries `share: false` is isolated:
//! discovery skips it (`daemon_info` reports `shared: false`), it
//! refuses `add_workspaces` with `not_shared`, and a client asked for
//! no-share (`--no-share` / `LAIN_MCP_NO_SHARE=1`) never attaches to
//! anything — it serves privately on the fallback path. A daemon this
//! module spawns inherits the property (`--no-share` on the child,
//! `share: false` in its synthesized config). Default stays shared.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::server::mcp::socket_server::path_fits;

/// Set by tests (and operators) that want the historical private
/// `lain mcp` server instead of the shared daemon.
pub fn sharing_opt_out() -> bool {
    std::env::var_os("LAIN_MCP_NO_SHARE").is_some()
}

/// How long `add_workspaces` may block the bridge setup while the
/// daemon indexes the handed-over workspaces (the tool itself waits at
/// most 60s per repo and answers `indexing` beyond that).
const ADD_BUDGET: Duration = Duration::from_secs(180);

/// How long a fresh daemon may take to accept its first connection.
/// Matches `oneshot`'s pre-existing startup wait.
fn start_wait_budget() -> Duration {
    Duration::from_secs(60)
}

/// What a live daemon reports on `lain/daemon_info`.
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    pub pid: u32,
    pub workspaces: Vec<PathBuf>,
    pub open_connections: u64,
    /// `false` for a no-share instance (`share: false` in its config):
    /// discovery must skip it. Missing field (older daemon) reads as
    /// `true` — the shipped default.
    pub shared: bool,
    /// The daemon's release version. `None` on a daemon that predates
    /// the field.
    pub version: Option<String>,
    /// The daemon's build commit (`GIT_SHA`). `None` on a daemon that
    /// predates the field.
    pub build_commit: Option<String>,
    /// The daemon's indexing-code identity (`INDEX_BUILD_ID`). `None`
    /// on a daemon that predates the field.
    pub index_build_id: Option<String>,
}

/// The build identity a client and a daemon must share before the
/// client bridges to it. All three parts matter: the version says
/// which release, the commit which source, and the index build id
/// which indexing code wrote the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonIdentity {
    version: String,
    build_commit: String,
    index_build_id: String,
}

impl DaemonIdentity {
    /// The identity of the running client binary.
    fn current() -> Self {
        Self {
            version: crate::server::build_info::VERSION.into(),
            build_commit: crate::server::build_info::GIT_SHA.into(),
            index_build_id: crate::server::build_info::INDEX_BUILD_ID.into(),
        }
    }
}

/// The reason a daemon is not compatible with this client, or `None`
/// when it is. A daemon that reports no identity at all (an older
/// build) is NOT compatible: it is unknown, not trusted.
///
/// The message names both the expected and the actual values, so an
/// operator can see which side moved. Shared with `lain oneshot` (the
/// other client of a shared daemon), so both refuse a stale daemon the
/// same way.
pub(crate) fn compatibility_error(info: &DaemonInfo) -> Option<String> {
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

/// The actionable refusal for a daemon whose identity differs from
/// this client's: it names the socket, both identities, and how to
/// recover. `recovery` is the caller's remedy text (the `lain mcp`
/// bridge offers `--no-share`, `lain oneshot` its own env override).
pub(crate) fn incompatible_daemon_error(
    info: &DaemonInfo,
    socket: &Path,
    recovery: &str,
) -> anyhow::Error {
    anyhow!(
        "{} at {}; {recovery}",
        compatibility_error(info).unwrap_or_else(|| "incompatible shared daemon".into()),
        socket.display(),
    )
}

/// The remedy text for a client that can serve a private instance.
const NO_SHARE_RECOVERY: &str = "restart the daemon or use --no-share";

/// One newline-framed JSON-RPC round trip on a short-lived connection.
/// Returns the `result` payload of the reply to `id`. Any timeout or
/// protocol noise fails the whole query — discovery treats that as
/// "no daemon here".
fn rpc_call(stream: &UnixStream, request: &Value, id: i64, budget: Duration) -> Result<Value> {
    let deadline = Instant::now() + budget;
    let mut writer = stream.try_clone().context("clone unix stream")?;
    let mut reader = BufReader::new(stream.try_clone().context("clone unix stream")?);
    let mut line = String::from(serde_json::to_string(request)?);
    line.push('\n');
    writer.write_all(line.as_bytes()).context("write rpc")?;
    writer.flush().context("flush rpc")?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow!("rpc timed out waiting for id {id}"));
        }
        stream
            .set_read_timeout(Some(remaining))
            .context("set read timeout")?;
        line.clear();
        let n = reader.read_line(&mut line).context("read rpc reply")?;
        if n == 0 {
            return Err(anyhow!("daemon closed the connection"));
        }
        let value: Value = serde_json::from_str(line.trim())
            .with_context(|| format!("parse rpc reply: {}", line.trim()))?;
        if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
            if let Some(err) = value.get("error") {
                return Err(anyhow!("rpc error: {err}"));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
        // Notifications and stale ids are skipped, as everywhere else.
    }
}

/// Ask the daemon behind `socket_path` who it is. `None` when nothing
/// answers — dead socket, timeout, or a non-daemon listener.
pub fn query_daemon_info(socket_path: &Path) -> Option<DaemonInfo> {
    let stream = UnixStream::connect(socket_path).ok()?;
    let result = rpc_call(
        &stream,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "lain/daemon_info"}),
        1,
        Duration::from_secs(2),
    )
    .ok()?;
    Some(DaemonInfo {
        pid: result.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32,
        workspaces: result
            .get("workspaces")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(PathBuf::from))
                    .collect()
            })
            .unwrap_or_default(),
        open_connections: result
            .get("open_connections")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        shared: result
            .get("shared")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        version: result
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string),
        build_commit: result
            .get("build_commit")
            .and_then(Value::as_str)
            .map(str::to_string),
        index_build_id: result
            .get("index_build_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Canonicalized, sorted, deduplicated workspace set — the identity
/// both sides compare coverage against.
fn canonical_set(workspaces: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = workspaces
        .iter()
        .map(|w| w.canonicalize().unwrap_or_else(|_| w.clone()))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The result of scanning the runtime dir for resident daemons.
pub enum Discovery {
    /// A live daemon serving a SUPERSET of the client's set can serve
    /// the client as-is.
    Covering { socket: PathBuf },
    /// Some daemon is alive but does not cover the client's set;
    /// `missing` are the client paths it lacks. Rule (6): attach and
    /// hand it those paths instead of spawning a rival daemon.
    Partial {
        socket: PathBuf,
        missing: Vec<PathBuf>,
    },
    /// A live daemon whose build identity differs from this client's.
    /// Bridging to it would answer with stale code, so the client
    /// refuses instead of attaching or starting a rival indexer.
    Incompatible { socket: PathBuf, info: DaemonInfo },
    /// No live daemon in the runtime dir.
    None,
}

/// Candidate daemon sockets in the run dir: `lain-mcp-<16hex>.sock`,
/// exactly the shape [`crate::config::shared_socket_path`] mints (the
/// reload-signal sockets have a different name shape and are skipped).
fn candidate_sockets() -> Vec<PathBuf> {
    let dir = crate::config::run_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("lain-mcp-"))
                .and_then(|s| s.strip_suffix(".sock"))
                .is_some_and(|stem| {
                    stem.len() == 16 && stem.chars().all(|c| c.is_ascii_alphanumeric())
                })
        })
        .collect();
    out.sort();
    out
}

/// Scan for a daemon that can serve `workspaces` — preferring an
/// existing covering daemon over spawning (rule c), and surfacing the
/// best partial candidate for the `add_workspaces` hand-off path.
/// No-share daemons (`shared: false`) are not candidates at all: they
/// are neither covering nor partial, so they are never attached to and
/// never grown.
///
/// A daemon whose build identity differs from this client's is neither
/// covering nor partial either: the scan keeps the FIRST such daemon
/// and keeps looking, because a compatible daemon (covering or
/// partial) always wins. Only when no compatible daemon exists does
/// the scan report [`Discovery::Incompatible`].
pub fn discover(workspaces: &[PathBuf]) -> Discovery {
    let needed = canonical_set(workspaces);
    let mut covering: Vec<(PathBuf, usize)> = Vec::new(); // (socket, served size)
    let mut partial: Option<(PathBuf, usize, Vec<PathBuf>)> = None; // (socket, overlap, missing)
    let mut incompatible: Option<(PathBuf, DaemonInfo)> = None; // (socket, reported identity)

    for socket in candidate_sockets() {
        let Some(info) = query_daemon_info(&socket) else {
            continue;
        };
        if !info.shared {
            continue;
        }
        if compatibility_error(&info).is_some() {
            // Stale code behind the socket: never bridged to. Keep the
            // first one and keep scanning — a compatible daemon may
            // still be live beside it.
            if incompatible.is_none() {
                incompatible = Some((socket, info));
            }
            continue;
        }
        let served = canonical_set(&info.workspaces);
        let missing: Vec<PathBuf> = needed
            .iter()
            .filter(|p| !served.contains(p))
            .cloned()
            .collect();
        if missing.is_empty() {
            covering.push((socket, served.len()));
        } else {
            let overlap = needed.len() - missing.len();
            let better = match &partial {
                Some((_, best, _)) => overlap > *best,
                None => true,
            };
            if better {
                partial = Some((socket, overlap, missing));
            }
        }
    }

    // Prefer the exact set-keyed socket when it is among the covering
    // daemons, then the smallest superset, then lexicographic order.
    let exact = crate::config::shared_socket_path(workspaces);
    covering.sort_by_key(|(sock, size)| (*sock != exact, *size, sock.clone()));
    if let Some((socket, _)) = covering.into_iter().next() {
        return Discovery::Covering { socket };
    }
    match partial {
        Some((socket, _, missing)) => Discovery::Partial { socket, missing },
        None => match incompatible {
            Some((socket, info)) => Discovery::Incompatible { socket, info },
            None => Discovery::None,
        },
    }
}

/// Spawn a detached `lain mcp --daemon` for `workspaces` on
/// `socket_path` (own process group, no stdio — it outlives the
/// caller) and return a connection once its socket accepts. Start
/// race: when the child exits early because another daemon won the
/// bind, connect to the winner (the `oneshot.rs` comment path).
/// `no_share` is inherited by the child (`--no-share`): a no-share
/// caller's daemon is a no-share instance.
pub fn start_detached_daemon(
    workspaces: &[PathBuf],
    socket_path: &Path,
    wait: Duration,
    embedding_model: Option<&Path>,
    reindex_timeout: Option<std::time::Duration>,
    no_share: bool,
) -> Option<UnixStream> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().ok()?;
    let mut command = Command::new(exe);
    command.arg("mcp");
    for ws in workspaces {
        command.arg("--workspace").arg(ws);
    }
    command.arg("--socket").arg(socket_path).arg("--daemon");
    if no_share {
        command.arg("--no-share");
    }
    if let Some(model) = embedding_model {
        command.arg("--embedding-model").arg(model);
    }
    if let Some(timeout) = reindex_timeout {
        command
            .arg("--reindex-timeout")
            .arg(timeout.as_secs().to_string());
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(stream) = UnixStream::connect(socket_path) {
            return Some(stream);
        }
        // The daemon died (e.g. another one won the start race and this
        // one exited, or startup failed): stop waiting and try once
        // more — the winner may be the one listening now.
        if let Ok(Some(_)) = child.try_wait() {
            return UnixStream::connect(socket_path).ok();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// Hand `paths` to the daemon behind `stream` via `tools/call
/// add_workspaces` and return the tool payload. On `isError` (typed
/// error envelope) the caller falls back.
pub fn admin_add_workspaces(
    stream: &UnixStream,
    paths: &[PathBuf],
    budget: Duration,
) -> Result<Value> {
    let path_strings: Vec<Value> = paths
        .iter()
        .map(|p| json!(p.display().to_string()))
        .collect();
    // `initialize` first, like any MCP client — the daemon's socket
    // dispatch is stateless per frame, but the greeting keeps the
    // conversation spec-shaped.
    rpc_call(
        stream,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "lain-mcp-bridge", "version": env!("CARGO_PKG_VERSION")}},
        }),
        1,
        budget,
    )?;
    let result = rpc_call(
        stream,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "add_workspaces", "arguments": {"paths": path_strings}},
        }),
        2,
        budget,
    )?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let text = result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or("add_workspaces failed");
        return Err(anyhow!("add_workspaces: {text}"));
    }
    let text = result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    serde_json::from_str(text).with_context(|| format!("parse add_workspaces payload: {text}"))
}

/// Transparent stdin↔socket relay: the stdio process becomes a bridge,
/// preserving every frame verbatim (`initialize`/`tools/list`/
/// `tools/call`/notifications both ways, `serverInfo` included). When
/// stdin hits EOF the socket write half is shut down but replies still
/// in flight are drained; when the daemon goes away the relay stops.
pub async fn run_stdio_bridge(stream: tokio::net::UnixStream) {
    use tokio::io::AsyncWriteExt;
    let (read_half, mut write_half) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let up = async {
        let _ = tokio::io::copy(&mut stdin, &mut write_half).await;
        let _ = write_half.shutdown().await;
    };
    let down = async {
        let mut read_half = read_half;
        let _ = tokio::io::copy(&mut read_half, &mut stdout).await;
        let _ = stdout.flush().await;
    };
    tokio::pin!(down);
    tokio::select! {
        // Daemon side done: nothing left to forward.
        _ = &mut down => {}
        // stdin done: half-close the socket and drain pending replies.
        _ = up => {
            let _ = down.await;
        }
    }
}

/// Phase A entry point: attach to (or start) the resident shared
/// daemon for `workspaces` and return the connection to bridge on.
/// `Ok(None)` means "cannot or should not share" — the caller serves
/// privately exactly as before. A no-share caller (`no_share` flag or
/// `LAIN_MCP_NO_SHARE`) never attaches and never starts a shared
/// daemon: it serves privately on the fallback path.
///
/// `Err` means the client found a daemon it must not use — its build
/// identity differs — and refuses to bridge to it. The error names the
/// socket and both identities. No rival daemon is started in that
/// case: the operator restarts the daemon or asks for a private
/// instance.
pub fn attach_or_start(
    workspaces: &[PathBuf],
    embedding_model: Option<&Path>,
    reindex_timeout: Option<std::time::Duration>,
    no_share: bool,
) -> anyhow::Result<Option<UnixStream>> {
    if no_share || sharing_opt_out() {
        return Ok(None);
    }
    let socket_key = crate::config::shared_socket_path(workspaces);
    if !path_fits(&socket_key) {
        tracing::warn!(
            "shared daemon socket path {} is too long; serving privately",
            socket_key.display()
        );
        return Ok(None);
    }
    match discover(workspaces) {
        Discovery::Covering { socket } => match UnixStream::connect(&socket) {
            Ok(stream) => {
                tracing::info!(
                    "lain mcp: attaching to shared daemon at {}",
                    socket.display()
                );
                only_shared(stream, &socket)
            }
            Err(e) => {
                // Raced a dying daemon: fall through to a fresh start.
                tracing::warn!(
                    "covering daemon at {} vanished ({e}); starting one",
                    socket.display()
                );
                finish_start(
                    start_detached_daemon(
                        workspaces,
                        &socket_key,
                        start_wait_budget(),
                        embedding_model,
                        reindex_timeout,
                        no_share,
                    ),
                    &socket_key,
                )
            }
        },
        Discovery::Partial { socket, missing } => {
            // Rival-spawn convergence: if the hand-off fails we spawn
            // a daemon for OUR set rather than falling back to a
            // private server — two daemons may then coexist briefly
            // (each on its own set-keyed socket), and later clients
            // converge on one because a covering daemon is preferred
            // over spawning and `add_workspaces` grows the survivor;
            // the unreferenced one idle-exits.
            let handoff = UnixStream::connect(&socket)
                .map_err(|e| anyhow!("connect: {e}"))
                .and_then(|admin| {
                    tracing::info!(
                        "lain mcp: handing {} workspace(s) to the daemon at {}",
                        missing.len(),
                        socket.display()
                    );
                    admin_add_workspaces(&admin, &missing, ADD_BUDGET)
                });
            match handoff {
                Ok(payload) => {
                    tracing::info!("lain mcp: daemon accepted the config: {payload}");
                    // The bridge gets a FRESH connection: the admin
                    // exchange's BufReader may have buffered bytes
                    // past the tool response, and those would be lost
                    // if the admin stream were reused as the bridge.
                    match UnixStream::connect(&socket) {
                        Ok(stream) => only_shared(stream, &socket),
                        Err(e) => {
                            tracing::warn!(
                                "daemon at {} vanished after add_workspaces ({e}); starting one",
                                socket.display()
                            );
                            finish_start(
                                start_detached_daemon(
                                    workspaces,
                                    &socket_key,
                                    start_wait_budget(),
                                    embedding_model,
                                    reindex_timeout,
                                    no_share,
                                ),
                                &socket_key,
                            )
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "handing config to the daemon at {} failed ({e}); starting a dedicated daemon",
                        socket.display()
                    );
                    finish_start(
                        start_detached_daemon(
                            workspaces,
                            &socket_key,
                            start_wait_budget(),
                            embedding_model,
                            reindex_timeout,
                            no_share,
                        ),
                        &socket_key,
                    )
                }
            }
        }
        Discovery::Incompatible { socket, info } => {
            Err(incompatible_daemon_error(&info, &socket, NO_SHARE_RECOVERY))
        }
        Discovery::None => {
            tracing::info!(
                "lain mcp: no shared daemon; starting one at {}",
                socket_key.display()
            );
            finish_start(
                start_detached_daemon(
                    workspaces,
                    &socket_key,
                    start_wait_budget(),
                    embedding_model,
                    reindex_timeout,
                    no_share,
                ),
                &socket_key,
            )
        }
    }
}

/// A bridge may only land on a shared daemon. `discover` filters
/// no-share daemons out, but the bind-race fallback in
/// [`start_detached_daemon`] connects to whoever won the socket — which
/// can be a no-share daemon that owns the same set-keyed path. A
/// definite `shared: false` refuses the bridge (the caller serves
/// privately). A probe that errors or times out is NOT a refusal: the
/// connection we already hold is live, and silence is not evidence of
/// no-share. (Discovery's treatment of silent sockets is different in
/// kind: it skips them as candidates rather than picking one.)
///
/// The same probe also re-checks the build identity: the winner of a
/// bind race may be an older daemon than the one the scan saw, and
/// bridging to it would answer with stale code.
fn only_shared(stream: UnixStream, socket: &Path) -> anyhow::Result<Option<UnixStream>> {
    match query_daemon_info(socket) {
        Some(info) if !info.shared => {
            tracing::warn!(
                "daemon at {} is a no-share instance; serving privately",
                socket.display()
            );
            Ok(None)
        }
        Some(info) => match compatibility_error(&info) {
            Some(_) => Err(incompatible_daemon_error(&info, socket, NO_SHARE_RECOVERY)),
            None => Ok(Some(stream)),
        },
        None => Ok(Some(stream)),
    }
}

/// [`start_detached_daemon`] still answers with an `Option`; this
/// adapter folds it into the `Result` shape of the attach path and
/// runs the shared-daemon check on the stream it produced.
fn finish_start(start: Option<UnixStream>, socket: &Path) -> anyhow::Result<Option<UnixStream>> {
    match start {
        Some(stream) => only_shared(stream, socket),
        None => Ok(None),
    }
}

#[cfg(test)]
mod identity_tests {
    use super::{compatibility_error, DaemonIdentity, DaemonInfo};
    use std::path::Path;

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
        })
        .unwrap();
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
        })
        .unwrap();
        assert!(error.contains("build_commit"));
        assert!(error.contains("index_build_id"));
    }

    /// A daemon whose identity matches the client's exactly is
    /// compatible: no error, so the bridge proceeds as before.
    #[test]
    fn matching_identity_is_compatible() {
        let current = DaemonIdentity::current();
        assert!(compatibility_error(&DaemonInfo {
            pid: 1,
            workspaces: vec![],
            open_connections: 0,
            shared: true,
            version: Some(current.version),
            build_commit: Some(current.build_commit),
            index_build_id: Some(current.index_build_id),
        })
        .is_none());
    }

    /// Each identity field on its own is enough to refuse the daemon.
    #[test]
    fn every_identity_field_is_compared() {
        let current = DaemonIdentity::current();
        for (label, info) in [
            (
                "version",
                DaemonInfo {
                    pid: 1,
                    workspaces: vec![],
                    open_connections: 0,
                    shared: true,
                    version: Some("0.0.1".into()),
                    build_commit: Some(current.build_commit.clone()),
                    index_build_id: Some(current.index_build_id.clone()),
                },
            ),
            (
                "build_commit",
                DaemonInfo {
                    pid: 1,
                    workspaces: vec![],
                    open_connections: 0,
                    shared: true,
                    version: Some(current.version.clone()),
                    build_commit: Some("0000000".into()),
                    index_build_id: Some(current.index_build_id.clone()),
                },
            ),
            (
                "index_build_id",
                DaemonInfo {
                    pid: 1,
                    workspaces: vec![],
                    open_connections: 0,
                    shared: true,
                    version: Some(current.version.clone()),
                    build_commit: Some(current.build_commit.clone()),
                    index_build_id: Some("stale-index".into()),
                },
            ),
        ] {
            let error = compatibility_error(&info)
                .unwrap_or_else(|| panic!("{label} mismatch must be refused"));
            assert!(error.contains(label), "{label}: {error}");
        }
    }

    /// A daemon that reports no identity at all is not trusted: a
    /// missing field is unknown, not a match.
    #[test]
    fn missing_version_is_rejected() {
        let error = compatibility_error(&DaemonInfo {
            pid: 1,
            workspaces: vec![],
            open_connections: 0,
            shared: true,
            version: None,
            build_commit: Some(crate::server::build_info::GIT_SHA.into()),
            index_build_id: Some(crate::server::build_info::INDEX_BUILD_ID.into()),
        })
        .unwrap();
        assert!(error.contains("version"), "{error}");
    }

    /// An unreachable daemon is not a refusal: the connection the
    /// client already holds is live, and silence is not evidence of
    /// anything. The stream is kept.
    #[test]
    fn an_unavailable_probe_keeps_the_stream() {
        let (stream, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let kept = super::only_shared(stream, Path::new("/nonexistent/lain-mcp.sock"))
            .expect("an unreachable probe is not an error");
        assert!(kept.is_some(), "the stream must survive a failed probe");
    }
}
