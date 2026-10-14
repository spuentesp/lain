//! Subprocess transport to the `zoanthrope` binary.
//!
//! Two surfaces, one contract:
//! - **CLI** (`zoanthrope claim|release|rejoin|status`) — the stable
//!   hook-facing contract: JSON on stdout, exit 0 granted / 1 conflict
//!   / 2 usage. Used for the claim hot path because it is what the
//!   hook family and the upstream experiments pin.
//! - **`zoanthrope mcp`** stdio JSON-RPC — where the CLI has no
//!   mirror (presence listings, postboard). A short-lived child:
//!   `initialize` + one `tools/call`, stdin kept open until the
//!   response with `id == 2` arrives (EOF mid-call races the SDK's
//!   transport shutdown), then stdin closed, response parsed, child
//!   reaped. zoanthrope exits on EOF, so nothing lingers.
//!
//! Resolution order for the binary: `$ZOANTHROPE_BIN`, then `PATH`
//! (same knobs zoanthrope's own hooks honor). Missing binary is not
//! an error — it is [`ClaimOutcome::Degraded`] / degraded JSON: lain
//! stays installable standalone and coordination fails soft to
//! advisory semantics (logged, never blocks).

use super::shapes::{parse_claim_body, CallerStamp, ClaimOutcome, DEGRADED_MARKER};
use crate::server::presence::{AgentId, Claim, ClaimIntent, SymbolHash};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

/// How long one zoanthrope subprocess may run before the proxy kills
/// it and reports an error. Coordination calls are microsecond-scale
/// upstream; a hang means a wedged binary, not a slow one.
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum ProxyError {
    /// No `zoanthrope` binary resolvable — degraded mode.
    Missing,
    /// The binary ran but the call failed (usage error, bad JSON…).
    Failed(String),
    /// The binary did not finish within [`SUBPROCESS_TIMEOUT`].
    Timeout,
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::Missing => write!(f, "zoanthrope binary not found on PATH"),
            ProxyError::Failed(e) => write!(f, "zoanthrope: {e}"),
            ProxyError::Timeout => write!(f, "zoanthrope: subprocess timed out"),
        }
    }
}

impl std::error::Error for ProxyError {}

fn resolve_bin() -> Option<PathBuf> {
    super::resolve_zoanthrope_bin()
}

/// Snapshot of the claims zoanthrope holds right now. Read-model for
/// consumers that used to walk the in-memory claims registry
/// (metrics, query, evaluation): one `list_claims` pull, then cheap
/// in-memory lookups — never a subprocess per node.
#[derive(Debug, Clone, Default)]
pub struct ClaimSnapshot {
    claims: Vec<Claim>,
}

impl ClaimSnapshot {
    pub fn all(&self) -> &[Claim] {
        &self.claims
    }

    pub fn for_path(&self, path: &Path) -> Option<&Claim> {
        self.claims.iter().find(|c| c.path == path)
    }

    pub fn for_agent(&self, agent: &AgentId) -> Vec<Claim> {
        self.claims
            .iter()
            .filter(|c| c.agent_id == *agent)
            .cloned()
            .collect()
    }
}

/// The proxy handle. One per server (workspace root fixed at
/// construction); binary resolution is cached because `PATH` does not
/// change under a running server.
pub struct CoordinationProxy {
    workspace: PathBuf,
    bin: std::sync::OnceLock<Option<PathBuf>>,
    /// Advisory log sink for degraded claims (what used to be "logged
    /// locally"). Bounded in memory so a degraded workspace cannot
    /// grow a leak; operators read it from the tool responses.
    degraded_log: parking_lot::Mutex<Vec<String>>,
}

impl std::fmt::Debug for CoordinationProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoordinationProxy")
            .field("workspace", &self.workspace)
            .field("bin", &self.bin.get().cloned().unwrap_or_default())
            .finish()
    }
}

impl CoordinationProxy {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            bin: std::sync::OnceLock::new(),
            degraded_log: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// Explicit binary override (tests install a fake `zoanthrope`;
    /// `None` forces degraded mode). Production uses [`Self::new`],
    /// which resolves `$ZOANTHROPE_BIN` / `PATH` lazily.
    pub fn with_bin(workspace: PathBuf, bin: Option<PathBuf>) -> Self {
        let s = Self::new(workspace);
        let _ = s.bin.set(bin);
        s
    }

    fn bin(&self) -> Option<&Path> {
        self.bin.get_or_init(resolve_bin).as_deref()
    }

    /// Workspace root: relative claim paths resolve against it (both
    /// for zoanthrope's `--workspace` and for symbol-stamp reads).
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// True when no zoanthrope is resolvable: every call fails soft to
    /// advisory semantics and every response carries
    /// `"degraded": "zoanthrope-missing"`.
    pub fn is_degraded(&self) -> bool {
        self.bin().is_none()
    }

    pub fn degraded_marker(&self) -> Option<&'static str> {
        self.is_degraded().then_some(DEGRADED_MARKER)
    }

    /// The recent advisory log (degraded mode only).
    pub fn degraded_notes(&self) -> Vec<String> {
        self.degraded_log.lock().clone()
    }

    fn note_degraded(&self, entry: String) {
        tracing::warn!("coordination degraded ({DEGRADED_MARKER}): {entry}");
        let mut log = self.degraded_log.lock();
        if log.len() >= 64 {
            log.remove(0);
        }
        log.push(entry);
    }

    // ── claim hot path (CLI contract) ──────────────────────────────

    /// Claim one file on behalf of `agent`, pinning `stamps` as
    /// zoanthrope `caller_stamps`. One file per call keeps lain's
    /// per-file partial-grant response shape and per-file intents
    /// (zoanthrope arbitrates a single intent per call).
    ///
    /// Degraded: logged + granted-advisory, never blocked.
    pub fn claim_file(
        &self,
        agent: &str,
        path: &str,
        purpose: Option<&str>,
        intent: ClaimIntent,
        stamps: &[CallerStamp],
    ) -> Result<ClaimOutcome, ProxyError> {
        let Some(bin) = self.bin() else {
            self.note_degraded(format!("claim {path} for {agent}"));
            return Ok(ClaimOutcome::Degraded {
                path: path.to_string(),
            });
        };
        let mut cmd = Command::new(bin);
        cmd.arg("claim")
            .arg("--files")
            .arg(path)
            .arg("--agent")
            .arg(agent)
            .arg("--intent")
            .arg(intent.as_str())
            .arg("--workspace")
            .arg(&self.workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        if let Some(p) = purpose {
            cmd.arg("--purpose").arg(p);
        }
        for s in stamps {
            cmd.arg("--stamp").arg(s.cli_arg());
        }
        let out = run_with_timeout(&mut cmd)?;
        // Exit 0 = granted, 1 = conflict — both are successful calls
        // whose body is the JSON we parse. Exit 2 (usage/IO) has a
        // message on stderr and no usable body.
        let body: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| {
            ProxyError::Failed(format!(
                "claim: non-JSON output: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        })?;
        parse_claim_body(path, &body).map_err(ProxyError::Failed)
    }

    /// `zoanthrope release --files … --agent …`. Body: `{ok, released}`.
    pub fn release_paths(
        &self,
        agent: &str,
        paths: &[String],
    ) -> Result<serde_json::Value, ProxyError> {
        if self.bin().is_none() {
            self.note_degraded(format!("release {paths:?} for {agent}"));
            return Ok(serde_json::json!({
                "ok": true,
                "released": paths,
                "degraded": DEGRADED_MARKER,
            }));
        }
        self.cli_json("release", |cmd| {
            cmd.arg("--files")
                .arg(paths.join(","))
                .arg("--agent")
                .arg(agent);
        })
    }

    /// `zoanthrope rejoin --agent … [--files …]`. Body:
    /// `{ok, agent_id, claims_kept, claims_at_risk, view_epoch, board}`.
    pub fn rejoin(
        &self,
        agent: &str,
        files: Option<&[String]>,
    ) -> Result<serde_json::Value, ProxyError> {
        if self.bin().is_none() {
            self.note_degraded(format!("rejoin for {agent}"));
            return Ok(serde_json::json!({
                "ok": true,
                "agent_id": agent,
                "claims_kept": [],
                "claims_at_risk": [],
                "degraded": DEGRADED_MARKER,
            }));
        }
        self.cli_json("rejoin", |cmd| {
            cmd.arg("--agent").arg(agent);
            if let Some(f) = files {
                cmd.arg("--files").arg(f.join(","));
            }
        })
    }

    /// `zoanthrope status --workspace …` — text summary (operators).
    pub fn status_text(&self) -> Result<String, ProxyError> {
        let Some(bin) = self.bin() else {
            return Err(ProxyError::Missing);
        };
        let mut cmd = Command::new(bin);
        cmd.arg("status")
            .arg("--workspace")
            .arg(&self.workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        let out = run_with_timeout(&mut cmd)?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn cli_json(
        &self,
        sub: &str,
        extra: impl FnOnce(&mut Command),
    ) -> Result<serde_json::Value, ProxyError> {
        let bin = self.bin().ok_or(ProxyError::Missing)?;
        let mut cmd = Command::new(bin);
        cmd.arg(sub)
            .arg("--workspace")
            .arg(&self.workspace)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        extra(&mut cmd);
        let out = run_with_timeout(&mut cmd)?;
        serde_json::from_slice(&out.stdout).map_err(|_| {
            ProxyError::Failed(format!(
                "{sub}: non-JSON output: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        })
    }

    // ── presence + postboard (MCP mirror) ──────────────────────────

    /// One `tools/call` against a short-lived `zoanthrope mcp`.
    pub fn mcp_call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ProxyError> {
        let bin = self.bin().ok_or(ProxyError::Missing)?;
        let mut cmd = Command::new(bin);
        cmd.arg("mcp")
            .arg("--workspace")
            .arg(&self.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| ProxyError::Failed(e.to_string()))?;
        {
            use std::io::Write;
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| ProxyError::Failed("zoanthrope mcp: no stdin".to_string()))?;
            let init = serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                           "clientInfo": {"name": "lain-proxy", "version": env!("CARGO_PKG_VERSION")}}
            });
            let call = serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": tool, "arguments": args}
            });
            writeln!(stdin, "{init}").map_err(|e| ProxyError::Failed(e.to_string()))?;
            writeln!(stdin, "{call}").map_err(|e| ProxyError::Failed(e.to_string()))?;
        }
        // stdin stays OPEN until the id-2 response arrives — closing
        // it now makes the MCP SDK start transport shutdown, and the
        // in-flight tools/call can lose that race before its response
        // is produced (the same bug `tests/oneshot_e2e.rs` pins for
        // `lain oneshot`). EOF is sent below, once we have the answer.
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProxyError::Failed("zoanthrope mcp: no stdout".to_string()))?;
        let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break, // EOF or IO error: child gone
                    Ok(_) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                            if v.get("id").is_some() && tx.send(v).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });
        // Drain stderr on its own thread: a child that fills the
        // ~64KB stderr pipe would otherwise block while we wait on
        // stdout (same pipe deadlock `wait_with_timeout` guards).
        if let Some(mut err_pipe) = child.stderr.take() {
            std::thread::spawn(move || {
                use std::io::Read;
                let mut sink = Vec::new();
                let _ = err_pipe.read_to_end(&mut sink);
            });
        }
        let deadline = std::time::Instant::now() + SUBPROCESS_TIMEOUT;
        let response = loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProxyError::Timeout);
            }
            match rx.recv_timeout(remaining) {
                // Skip the initialize response; wait for id 2.
                Ok(v) if v.get("id").and_then(|i| i.as_u64()) != Some(2) => continue,
                Ok(v) => break v,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProxyError::Timeout);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // stdout EOF before id 2: the child exited without
                    // answering.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProxyError::Failed(format!(
                        "{tool}: no tools/call response on stdout"
                    )));
                }
            }
        };
        // Answer in hand: EOF lets the child exit on its own; reap it
        // under the same hard deadline, killing a wedged child.
        drop(child.stdin.take());
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        if let Some(err) = response.get("error") {
            return Err(ProxyError::Failed(format!(
                "{tool}: {}",
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("mcp error")
            )));
        }
        let result = response
            .get("result")
            .ok_or_else(|| ProxyError::Failed(format!("{tool}: no result")))?;
        if result.get("isError").and_then(serde_json::Value::as_bool) == Some(true) {
            let text = result["content"][0]["text"]
                .as_str()
                .unwrap_or("tool error");
            return Err(ProxyError::Failed(format!("{tool}: {text}")));
        }
        let text = result["content"][0]["text"].as_str().unwrap_or("{}");
        serde_json::from_str(text)
            .map_err(|e| ProxyError::Failed(format!("{tool}: bad tool JSON: {e}")))
    }

    /// Pull every live claim (`list_claims`) and adapt it to lain's
    /// `Claim` DTO. Empty snapshot when degraded.
    pub fn claims_snapshot(&self) -> ClaimSnapshot {
        let Ok(body) = self.mcp_call("list_claims", serde_json::json!({})) else {
            return ClaimSnapshot::default();
        };
        let claims = body
            .get("claims")
            .and_then(serde_json::Value::as_array)
            .map(|arr| arr.iter().filter_map(claim_from_zoanthrope).collect())
            .unwrap_or_default();
        ClaimSnapshot { claims }
    }

    /// `list_claims` scoped to one agent identity (the agent name),
    /// adapted to lain's `Claim`.
    pub fn claims_for_agent(&self, identity: &str) -> Vec<Claim> {
        let body = match self.mcp_call("list_claims", serde_json::json!({ "agent_id": identity })) {
            Ok(b) => b,
            Err(_) => return Vec::new(),
        };
        body.get("claims")
            .and_then(serde_json::Value::as_array)
            .map(|arr| arr.iter().filter_map(claim_from_zoanthrope).collect())
            .unwrap_or_default()
    }

    /// `register_agent` / presence touch. zoanthrope registers by
    /// name, and the name *is* the agent id it hands back — so lain's
    /// session agent id round-trips unchanged.
    pub fn register_agent(&self, name: &str, kind: &str) -> Result<serde_json::Value, ProxyError> {
        self.mcp_call(
            "register_agent",
            serde_json::json!({ "name": name, "kind": kind }),
        )
    }

    pub fn who_am_i(&self, agent: &str) -> Result<serde_json::Value, ProxyError> {
        self.mcp_call("who_am_i", serde_json::json!({ "agent_id": agent }))
    }

    pub fn list_active_agents(&self, include_stale: bool) -> Result<serde_json::Value, ProxyError> {
        self.mcp_call(
            "list_active_agents",
            serde_json::json!({ "include_stale": include_stale }),
        )
    }

    // ── postboard (the `lain_intent` backend) ──────────────────────

    pub fn post_intention(
        &self,
        agent: &str,
        title: &str,
        note: &str,
        files: &[String],
    ) -> Result<serde_json::Value, ProxyError> {
        self.mcp_call(
            "post_intention",
            serde_json::json!({
                "agent_id": agent,
                "title": title,
                "note": note,
                "files": files,
            }),
        )
    }

    pub fn list_posts(
        &self,
        agent: Option<&str>,
        status: Option<&str>,
    ) -> Result<serde_json::Value, ProxyError> {
        let mut args = serde_json::Map::new();
        if let Some(a) = agent {
            args.insert("agent_id".into(), serde_json::Value::String(a.to_string()));
        }
        if let Some(s) = status {
            args.insert("status".into(), serde_json::Value::String(s.to_string()));
        }
        self.mcp_call("list_posts", serde_json::Value::Object(args))
    }

    pub fn update_post(
        &self,
        agent: &str,
        post_id: &str,
        status: Option<&str>,
        title: Option<&str>,
        note: Option<&str>,
        files: Option<&[String]>,
    ) -> Result<serde_json::Value, ProxyError> {
        let mut args = serde_json::Map::new();
        args.insert(
            "agent_id".into(),
            serde_json::Value::String(agent.to_string()),
        );
        args.insert(
            "post_id".into(),
            serde_json::Value::String(post_id.to_string()),
        );
        if let Some(s) = status {
            args.insert("status".into(), serde_json::Value::String(s.to_string()));
        }
        if let Some(t) = title {
            args.insert("title".into(), serde_json::Value::String(t.to_string()));
        }
        if let Some(n) = note {
            args.insert("note".into(), serde_json::Value::String(n.to_string()));
        }
        if let Some(f) = files {
            args.insert("files".into(), serde_json::json!(f));
        }
        self.mcp_call("update_post", serde_json::Value::Object(args))
    }

    pub fn remove_post(&self, agent: &str, post_id: &str) -> Result<serde_json::Value, ProxyError> {
        self.mcp_call(
            "remove_post",
            serde_json::json!({ "agent_id": agent, "post_id": post_id }),
        )
    }
}

/// Adapt one zoanthrope `list_claims` row to lain's `Claim` DTO.
fn claim_from_zoanthrope(v: &serde_json::Value) -> Option<Claim> {
    let path = v.get("path")?.as_str()?.to_string();
    let agent = v.get("agent")?.as_str()?.to_string();
    let stamps: Vec<CallerStamp> = v
        .get("caller_stamps")
        .and_then(|s| serde_json::from_value(s.clone()).ok())
        .unwrap_or_default();
    let symbols = super::symbols_from_stamps(&stamps);
    let intent = match v.get("intent").and_then(|i| i.as_str()) {
        Some("read") => ClaimIntent::Read,
        _ => ClaimIntent::Edit,
    };
    let claimed_at = v.get("claimed_at").and_then(serde_json::Value::as_f64);
    let at = |secs: Option<f64>| {
        secs.map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs_f64(s.max(0.0)))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    };
    Some(Claim {
        agent_id: AgentId(agent),
        path: PathBuf::from(path),
        symbols,
        // Per-symbol fingerprints travel as `caller_stamps`; the
        // legacy single `content_hash` field has no zoanthrope
        // counterpart (zoanthrope verifies its own file stamp).
        content_hash: None::<SymbolHash>,
        intent,
        claimed_at: at(claimed_at),
        last_touched_unix: at(claimed_at),
        expires_at: None,
        plan_revision: None,
        inferred: false,
    })
}

/// Run a CLI subcommand with a hard deadline, capturing stdout and
/// stderr. The child is killed on overrun so a wedged binary can
/// never wedge the server. A just-written executable can transiently
/// fail `exec` with ETXTBSY ("Text file busy") on Linux — retry a
/// few times before surfacing the error.
fn run_with_timeout(cmd: &mut Command) -> Result<std::process::Output, ProxyError> {
    let mut child = None;
    for attempt in 0..3 {
        match cmd.spawn() {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(26) =>
            {
                if attempt == 2 {
                    return Err(ProxyError::Failed(format!("spawn zoanthrope: {e}")));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(ProxyError::Failed(format!("spawn zoanthrope: {e}"))),
        }
    }
    let mut child = child.ok_or_else(|| ProxyError::Failed("spawn zoanthrope: no child".into()))?;
    drop(child.stdin.take());
    let (stdout, stderr) = wait_with_timeout(&mut child)?;
    Ok(std::process::Output {
        status: child
            .wait()
            .map_err(|e| ProxyError::Failed(e.to_string()))?,
        stdout,
        stderr,
    })
}

/// Collect a child's stdout + stderr with a hard deadline. The child
/// is killed on overrun so a wedged binary can never wedge the server.
fn wait_with_timeout(child: &mut std::process::Child) -> Result<(Vec<u8>, Vec<u8>), ProxyError> {
    use std::io::Read;
    let mut pipe = match child.stdout.take() {
        Some(p) => p,
        None => return Err(ProxyError::Failed("zoanthrope: no stdout".to_string())),
    };
    let mut err_pipe = child.stderr.take();
    // One thread per pipe. A single reader draining stdout to EOF
    // first and only then stderr deadlocks when the child fills the
    // ~64KB stderr pipe while stdout is still open: the child blocks
    // on stderr, we block on stdout, and the deadline kills a healthy
    // process (whose failure `claims.rs` then fails open on).
    let (out_tx, out_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = out_tx.send(buf);
    });
    let (err_tx, err_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let err_reader = std::thread::spawn(move || {
        let mut ebuf = Vec::new();
        if let Some(ref mut ep) = err_pipe {
            let _ = ep.read_to_end(&mut ebuf);
        }
        let _ = err_tx.send(ebuf);
    });
    let deadline = std::time::Instant::now() + SUBPROCESS_TIMEOUT;
    let remaining = || deadline.saturating_duration_since(std::time::Instant::now());
    let out = out_rx.recv_timeout(remaining());
    let err = err_rx.recv_timeout(remaining());
    match (out, err) {
        (Ok(stdout), Ok(stderr)) => {
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            Ok((stdout, stderr))
        }
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            // Reader threads are not joined: they exit on EOF once the
            // killed child's pipe ends close.
            Err(ProxyError::Timeout)
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Regression: both pipes must drain concurrently. The child
    /// writes >64KB to stderr (more than the OS pipe buffer) while
    /// keeping stdout open, then exits. Reading stdout to EOF before
    /// touching stderr deadlocks exactly like that — child blocked on
    /// stderr, reader blocked on stdout — and the 10s deadline would
    /// kill a healthy process.
    #[test]
    fn wait_with_timeout_drains_stderr_without_deadlocking() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("head -c 200000 /dev/zero >&2; echo ok")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn sh");
        let start = std::time::Instant::now();
        let (out, _err) = wait_with_timeout(&mut child).expect("must complete within deadline");
        assert_eq!(String::from_utf8_lossy(&out), "ok\n");
        assert!(
            start.elapsed() < SUBPROCESS_TIMEOUT,
            "completed in {:?} — the deadline must not be hit",
            start.elapsed()
        );
    }
}
