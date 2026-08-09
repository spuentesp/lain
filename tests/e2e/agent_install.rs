//! End-to-end DST-style harness: drives every installed agent binary
//! through the same scripted scenario and asserts the same invariants.
//! Gated on `RUN_E2E_AGENT=1`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn lain_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

#[allow(dead_code)]
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

#[allow(dead_code)]
fn pick_port() -> u16 {
    use std::net::TcpListener;
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let p = l.local_addr().expect("addr").port();
    drop(l);
    p
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn prepare_home(case: &AgentCase, home: &Path) {
    match case.id {
        "kimi" => {
            let dir = home.join(".kimi-code");
            std::fs::create_dir_all(&dir).expect("kimi config dir");
            std::fs::write(
                dir.join("config.toml"),
                r#"default_model = "kimi-code/kimi-for-coding"

[providers."managed:kimi-code"]
type = "kimi"
api_key = ""
base_url = "https://api.kimi.com/coding/v1"

[providers."managed:kimi-code".oauth]
storage = "file"
key = "oauth/kimi-code"

[models."kimi-code/kimi-for-coding"]
provider = "managed:kimi-code"
model = "kimi-for-coding"
max_context_size = 262144
"#,
            )
            .expect("kimi config");
            // Kimi needs OAuth / device credentials to run headlessly.
            // Copy them from the caller's real HOME into the temp HOME.
            if let Ok(real_home) = std::env::var("HOME") {
                let real = Path::new(&real_home).join(".kimi-code");
                for sub in ["credentials", "oauth", "device_id"] {
                    let src = real.join(sub);
                    if src.exists() {
                        let dst = dir.join(sub);
                        if src.is_dir() {
                            copy_dir_all(&src, &dst).expect("copy kimi {sub}");
                        } else {
                            std::fs::copy(&src, &dst).expect("copy kimi {sub}");
                        }
                    }
                }
            }
        }
        "omp" => {
            let dir = home.join(".config/omp");
            std::fs::create_dir_all(&dir).expect("omp config dir");
            std::fs::write(
                dir.join("config.json"),
                r#"{"providers":{"ollama":{"base_url":"http://localhost:11434"}}}"#,
            )
            .expect("omp config");
        }
        _ => {}
    }
}

fn install_into(case: &AgentCase, home: &Path, port: u16) -> Command {
    let xdg = home.join(".config");
    let mut c = Command::new(lain_bin());
    c.env("HOME", home)
        .env("XDG_CONFIG_HOME", xdg)
        .env("LAIN_PORT", port.to_string())
        .args(["agents", "install", "--scope", "user", case.id]);
    c
}

fn spawn_agent(case: &AgentCase, home: &Path) -> Child {
    let prompt = "list the MCP tools you have, then call get_health on the one named lain, and print both the tool list and the get_health response verbatim";
    let mut c = Command::new(case.binary);
    c.current_dir(case.workspace)
        .env("HOME", home)
        .args(case.run_args.iter().copied())
        .arg(prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c.spawn().expect("agent spawn")
}

fn assert_case_invariants(stdout: &str, stderr: &str, case: &AgentCase) -> Result<(), String> {
    if !case.requires_auth {
        let tool_re = regex::Regex::new(r"mcp__plugin-lain_lain__\w+").unwrap();
        if !tool_re.is_match(stdout) {
            return Err(format!("{}: tool list missing mcp__plugin-lain_lain__* (stdout head: {:.200})", case.id, stdout));
        }
        if !stdout.to_lowercase().contains("operational") {
            return Err(format!("{}: get_health body missing 'Operational' (stdout head: {:.200})", case.id, stdout));
        }
        // The `static_nodes` line is `- **Static Nodes:** <count>`. Find
        // the line, take the trailing number, parse it as `u64`, and
        // assert it is greater than 1000. This is the spec's "the body
        // looks right" smoke check.
        let static_nodes_line = stdout
            .lines()
            .find(|l| l.contains("Static Nodes"))
            .ok_or_else(|| format!("{}: get_health body missing 'Static Nodes' line (stdout head: {:.200})", case.id, stdout))?;
        let static_nodes: u64 = static_nodes_line
            .rsplit(':')
            .next()
            .unwrap_or("")
            .trim()
            .parse()
            .map_err(|e| format!("{}: cannot parse static_nodes count from '{}': {} (stdout head: {:.200})", case.id, static_nodes_line, e, stdout))?;
        if static_nodes <= 1000 {
            return Err(format!("{}: static_nodes {} is not > 1000 (stdout head: {:.200})", case.id, static_nodes, stdout));
        }
    } else {
        eprintln!("[{}] auth-gated: skipped inner assertions", case.id);
    }
    if stderr.contains("error sending request") || stderr.contains("connect error") {
        return Err(format!("{}: stderr reports a fatal MCP error (stderr head: {:.200})", case.id, stderr));
    }
    Ok(())
}

fn get_health_json(port: u16) -> Option<String> {
    let url = std::env::var("LAIN_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{port}/mcp"));
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "get_health", "arguments": {}}
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .ok()?;
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let response: serde_json::Value = response.json().ok()?;
    response
        .pointer("/result/content/0/text")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Parse the `Last Enriched Commit` SHA out of a get_health body. The
/// singleton emits a `- **Last Enriched Commit:** <sha>` line; we grab the
/// last whitespace-separated token on that line. Returns `None` if the
/// marker is missing or the line is empty.
fn parse_last_enriched_commit(body: &str) -> Option<String> {
    body.lines()
        .find(|l| l.contains("Last Enriched Commit"))
        .and_then(|l| l.split_whitespace().last())
        .filter(|tok| !tok.is_empty() && tok.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_owned)
}

fn assert_watcher_round_trip(
    case: &AgentCase,
    home: &Path,
    port: u16,
    before: &str,
) -> Result<(), String> {
    let _ = home;
    let trigger = case.workspace.join("e2e_trigger.py");
    std::fs::write(&trigger, b"# lain e2e trigger\n").map_err(|e| e.to_string())?;
    let before_sha = parse_last_enriched_commit(before);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = before.to_string();
    while Instant::now() < deadline {
        if let Some(line) = get_health_json(port) {
            if line != before && line.contains("Operational") { last = line; break; }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = std::fs::remove_file(&trigger);
    // Primary signal: the Last Enriched Commit SHA advanced, which is the
    // concrete evidence that the watcher fired `build_core_memory` after
    // the trigger file landed. Fall back to the body-changed check if
    // either side failed to parse a SHA.
    if let (Some(b), Some(a)) = (before_sha.as_deref(), parse_last_enriched_commit(&last).as_deref()) {
        if b == a {
            return Err(format!(
                "{}: watcher did not advance Last Enriched Commit within 5s (before={}, after={})",
                case.id, b, a
            ));
        }
        return Ok(());
    }
    if last == before {
        return Err(format!("{}: watcher did not surface trigger file in get_health body within 5s (before/after identical)", case.id));
    }
    Ok(())
}

/// Adapter round-trip: re-run the install loop in a fresh temp HOME and
/// assert the resulting per-agent config file is valid JSON, contains the
/// `mcpServers.lain` key, and (for non-auth-gated agents) carries either a
/// `command` or `url` entry.
///
/// The brief notes that the seven adapter manifests write different paths;
/// `lain agents list` prints the resolved `config_user` path on each row.
/// We use that output to discover which file to read, then expand the
/// leading `~/` against the temp HOME we just set.
fn assert_adapter_round_trip(case: &AgentCase) -> Result<(), String> {
    let home = tempfile::tempdir().expect("tempdir");
    prepare_home(case, home.path());
    // Same port resolution as the live install path so the URL written to
    // the agent config matches what the singleton would actually answer.
    let port: u16 = std::env::var("LAIN_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9999);
    let xdg = home.path().join(".config");
    let install_status = Command::new(lain_bin())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", &xdg)
        .env("LAIN_PORT", port.to_string())
        .args(["agents", "install", "--scope", "user", case.id])
        .status()
        .map_err(|e| format!("{}: install spawn failed: {}", case.id, e))?;
    if !install_status.success() {
        return Err(format!("{}: install exited with {:?}", case.id, install_status));
    }
    let list_out = Command::new(lain_bin())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", &xdg)
        .args(["agents", "list"])
        .output()
        .map_err(|e| format!("{}: agents list spawn failed: {}", case.id, e))?;
    let list = String::from_utf8_lossy(&list_out.stdout);
    let line = list
        .lines()
        .find(|l| l.contains(case.id))
        .ok_or_else(|| format!("{}: agents list missing row for `{}` (output head: {:.200})", case.id, case.id, list))?;
    // The PATH column is the last whitespace-separated token on the row.
    let path_token = line
        .split_whitespace()
        .last()
        .ok_or_else(|| format!("{}: cannot parse config path from `{}`", case.id, line))?;
    // Expand the leading `~/` against the temp HOME we just set. Strip
    // only the leading prefix to avoid mangling paths that happen to
    // contain `~/` deeper in the string (none of the current manifest
    // entries do, but this is safer than `replace("~/", "")`).
    let rel = path_token.strip_prefix("~/").unwrap_or(path_token);
    let full = home.path().join(rel);
    let body = std::fs::read_to_string(&full)
        .map_err(|e| format!("{}: cannot read {}: {}", case.id, full.display(), e))?;
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("{}: bad json in {}: {}", case.id, full.display(), e))?;
    let lain = json.pointer("/mcpServers/lain").ok_or_else(|| {
        format!("{}: mcpServers.lain missing in {} (body head: {:.200})", case.id, full.display(), body)
    })?;
    if !case.requires_auth {
        // For HTTP-format adapters the entry has `url`; for stdio/sidecar
        // it has `command`. Either is acceptable as a smoke check.
        if lain.get("command").is_none() && lain.get("url").is_none() {
            return Err(format!("{}: no command or url in mcpServers.lain entry: {:.200}", case.id, lain));
        }
    }
    Ok(())
}

fn run_case(case: &AgentCase) -> Result<(), String> {
    use std::io::Read;
    let tmp = tempfile::tempdir().expect("tempdir");
    prepare_home(case, tmp.path());
    // The brief calls `pick_port()`, but the live HTTP singleton listens on
    // a fixed port (default 9999). `LAIN_PORT` overrides the install-time
    // URL the agent will call; the spec says default 9999 and the harness
    // depends on the singleton being reachable at that URL.
    let port: u16 = std::env::var("LAIN_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9999);
    // Lain's main() bails when the workspace lacks `.git`. If the workspace
    // already has `.git` (e.g. because it's a git worktree like the
    // /home/sebastian/orca/workspaces/lain/langostino worktree this harness
    // is developed in), skip the init — re-initing a worktree dir is a
    // destructive operation we do not want the test to attempt.
    let already_repo = Command::new("git")
        .args(["-C", case.workspace.to_str().unwrap(), "rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !already_repo {
        let init_status = Command::new("git").args(["init", "--quiet", case.workspace.to_str().unwrap()]).status().map_err(|e| e.to_string())?;
        if !init_status.success() { return Err(format!("{}: git init failed", case.id)); }
    }
    let install_status = install_into(case, tmp.path(), port).status().map_err(|e| e.to_string())?;
    let install_succeeded = install_status.success();
    let mut child = spawn_agent(case, tmp.path());
    let timeout = Duration::from_secs(90);
    let start = Instant::now();
    let mut stdout = String::new();
    let mut stderr = String::new();
    let exit: std::process::ExitStatus = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{}: timed out after {:?}", case.id, timeout));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("{}: wait error: {}", case.id, e)),
        }
    };
    child.stdout.take().unwrap().read_to_string(&mut stdout).map_err(|e| e.to_string())?;
    child.stderr.take().unwrap().read_to_string(&mut stderr).map_err(|e| e.to_string())?;
    let status = exit;
    if !status.success() && !case.requires_auth {
        return Err(format!("{}: non-zero exit ({:?}); stderr: {:.200}", case.id, status, stderr));
    }
    assert_case_invariants(&stdout, &stderr, case)?;
    let before = get_health_json(port).unwrap_or_default();
    if !install_succeeded {
        // Install failed (today, because `lain agents` is not wired into
        // `main()` — see task-7 §1). The plan said auth-gated cases
        // should skip cleanly when the install fails. Best-effort: still
        // run the watcher step (it might surface the install failure),
        // then surface the install failure as a clean skip from the
        // adapter step rather than letting the watcher bubble up the
        // generic `before/after identical` error.
        let _ = assert_watcher_round_trip(case, tmp.path(), port, &before);
        return Err(format!("{}: install failed; cannot verify adapter", case.id));
    }
    assert_watcher_round_trip(case, tmp.path(), port, &before)?;
    assert_adapter_round_trip(case)
}

pub struct AgentCase {
    pub id: &'static str,
    pub binary: &'static str,
    pub run_args: &'static [&'static str],
    pub requires_auth: bool,
    pub workspace: &'static Path,
}

fn agent_cases() -> &'static [AgentCase] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<AgentCase>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let workspace: &'static Path = Box::leak(Box::new(PathBuf::from(
            "/home/sebastian/orca/workspaces/lain/langostino",
        )));
        // Kimi's CLI exposes `-p <prompt>` and `-y/--yolo`; the two flags
        // cannot be combined (kimi errors with "Cannot combine --prompt
        // with --yolo"), so we use `-p` alone. There is no `--print-timeout`
        // flag. agy exposes `--print` (alias `-p`) and `--print-timeout`.
        // The harness appends the test prompt as the final positional
        // argument via `spawn_agent`.
        vec![
            AgentCase {
                id: "kimi",
                binary: "kimi",
                run_args: &["-p"],
                requires_auth: false,
                workspace,
            },
            AgentCase {
                id: "agy",
                binary: "agy",
                run_args: &["--dangerously-skip-permissions", "--print-timeout", "60s", "-p"],
                requires_auth: false,
                workspace,
            },
            AgentCase {
                id: "claude",
                binary: "claude",
                run_args: &["--allow-dangerously-skip-permissions"],
                requires_auth: true,
                workspace,
            },
            AgentCase {
                id: "cursor",
                binary: "cursor-agent",
                run_args: &["--print"],
                requires_auth: true,
                workspace,
            },
            AgentCase {
                id: "cline",
                binary: "cline",
                run_args: &["--yolo", "--print", "--output-format", "json"],
                requires_auth: true,
                workspace,
            },
            AgentCase {
                id: "continue",
                binary: "cn",
                run_args: &["-p", "--output-format", "json"],
                requires_auth: true,
                workspace,
            },
            AgentCase {
                id: "omp",
                binary: "omp",
                run_args: &[
                    "-p",
                    "--provider",
                    "ollama",
                    "--model",
                    "qwen2.5:latest",
                    "--yolo",
                ],
                requires_auth: false,
                workspace,
            },
            AgentCase {
                id: "codex",
                binary: "codex",
                run_args: &["exec", "--yolo"],
                requires_auth: true,
                workspace,
            },
        ]
    })
}

fn run_case_assert(case: &AgentCase) {
    assert_eq!(run_case(case), Ok(()), "agent case {} failed", case.id);
}

#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_kimi() { run_case_assert(&agent_cases()[0]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_agy() { run_case_assert(&agent_cases()[1]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_claude() { run_case_assert(&agent_cases()[2]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_cursor() { run_case_assert(&agent_cases()[3]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_cline() { run_case_assert(&agent_cases()[4]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_cn() { run_case_assert(&agent_cases()[5]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1"]
fn e2e_omp() { run_case_assert(&agent_cases()[6]) }

#[test]
#[ignore = "requires RUN_E2E_AGENT=1; auth-gated"]
fn e2e_codex() { run_case_assert(&agent_cases()[7]) }
