//! Daemon-side `add_workspaces` — hand a running shared `lain mcp`
//! daemon more workspaces and have them added in hot (zero-config
//! multi-agent, 2026-10-17).
//!
//! A late MCP client whose resolved workspace set is not covered by any
//! live daemon attaches to one anyway and calls this tool with the
//! missing paths; the daemon then serves the union. The add runs the
//! exact hot-reload path `workspaces.yaml` / `repos.yaml` edits use:
//! rewrite the daemon's synthesized `repos.yaml`, then
//! [`crate::server::reload::run_rebuild`] diffs the live federation
//! against the file and calls `LainServer::add_repo` per addition
//! (per-repo index + cross-repo relink + watchers) and `remove_repo`
//! per removal. Writing the file first matters: without it, any later
//! reload would diff the added repos away again.
//!
//! `add_workspaces` is registered on the daemon's socket transport
//! only (`socket_server::dispatch`), not in the advertised tool
//! surface — advertising is not dispatch, and the bridge calls it
//! programmatically. It is idempotent: a path already served comes
//! back with `status: "already"` and touches nothing. A no-share
//! instance (`share: false` in its `repos.yaml`) refuses every call
//! with the typed `not_shared` error before any validation or
//! mutation.

use crate::server::LainServer;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long `add_workspaces` waits for a hot-added repo's first index
/// before reporting `indexing` instead of a terminal status. A fresh
/// repo is unusable until it lands, so the wait is on the caller's
/// behalf; callers that need a longer budget should poll
/// `get_health` / `list_repos` instead of blocking the tool call.
const INDEX_WAIT: Duration = Duration::from_secs(60);

/// Typed-error envelope for a failed `add_workspaces`. `code` is
/// machine-readable (`invalid_workspace`, `not_federated`,
/// `not_shared`, `add_failed`, `invalid_args`); `path` names the
/// offending workspace when there is one.
fn err(code: &str, path: Option<&Path>, message: impl AsRef<str>) -> Value {
    json!({
        "ok": false,
        "error": code,
        "path": path.map(|p| p.display().to_string()),
        "message": message.as_ref(),
    })
}

/// Whether this instance takes part in the shared-daemon pool: read
/// from the daemon's own `repos.yaml` (`share`, default `true`). A
/// private (`share: false`) instance is skipped by discovery and
/// refuses the `add_workspaces` handoff. No config on the server (or
/// one that cannot be loaded) defaults to shared — the property is
/// opt-out and the shipped default is the shared ephemeral service.
pub fn instance_shared(server: &LainServer) -> bool {
    server
        .repos_yaml()
        .and_then(|p| crate::server::federation::config::FederationConfig::load(p).ok())
        .map(|c| c.share)
        .unwrap_or(true)
}

/// Canonicalized local path for every workspace the server currently
/// serves: one entry per federation repo's source root, or the single
/// workspace of a non-federation server. Sorted, deduplicated. This is
/// what `lain/daemon_info` reports and what coverage checks compare
/// against.
pub fn served_workspaces(server: &LainServer) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match server.federation() {
        Some(fed) => fed.repo_paths(),
        None => vec![server.ingest().config().workspace.clone()],
    };
    for p in &mut out {
        if let Ok(c) = p.canonicalize() {
            *p = c;
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Resolve + validate one workspace path the way the CLI resolution
/// contract does: must exist, be a directory, and hold a `.git` entry.
/// Returns the canonicalized path or a typed-error envelope.
fn validated(path_str: &str) -> Result<PathBuf, Value> {
    let raw = PathBuf::from(path_str);
    let canonical = raw.canonicalize().map_err(|e| {
        err(
            "invalid_workspace",
            Some(&raw),
            format!("cannot resolve: {e}"),
        )
    })?;
    if !canonical.is_dir() {
        return Err(err(
            "invalid_workspace",
            Some(&canonical),
            "not a directory",
        ));
    }
    if !canonical.join(".git").exists() {
        return Err(err(
            "invalid_workspace",
            Some(&canonical),
            "not a git workspace (no .git entry) — `add_workspaces` registers workspace roots",
        ));
    }
    Ok(canonical)
}

/// A repo id for `path`: its basename, with `-N` suffixes on collision
/// (mirrors `cli::mcp::build_repos_yaml_for_workspaces`). `taken` is
/// updated in place.
fn unique_id(path: &Path, taken: &mut Vec<String>) -> String {
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();
    if !taken.contains(&base) {
        taken.push(base.clone());
        return base;
    }
    let mut n = 1;
    loop {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            taken.push(candidate.clone());
            return candidate;
        }
        n += 1;
    }
}

/// One `add_workspaces` call must not be able to pin the daemon for
/// hours or fill the runtime dir: the entry count and each path's
/// length are capped up front, before any validation or mutation.
const MAX_PATHS: usize = 32;
/// Longest accepted path string (PATH_MAX on Linux/macOS).
const MAX_PATH_BYTES: usize = 4096;

/// Serializes the config read-modify-write + rebuild so two late
/// clients handing config at once cannot interleave: the temp file is
/// uniquely named now (`cli::io::write_file_atomic`), but two writers
/// could still lose each other's repo entries in the
/// load→append→write sequence without one writer at a time.
fn add_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// The `add_workspaces` implementation. Returns the tool payload on
/// success and the typed-error envelope on failure (the socket layer
/// turns the latter into `isError: true`).
pub async fn run_add_workspaces(server: &LainServer, args: &Value) -> Result<Value, Value> {
    // A no-share instance refuses the handoff outright — BEFORE any
    // argument validation or mutation — so nothing about it can be
    // grown by a foreign client.
    if !instance_shared(server) {
        return Err(err(
            "not_shared",
            None,
            "this instance is not shared (share: false); it refuses the add_workspaces handoff",
        ));
    }
    let paths = args
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| err("invalid_args", None, "missing `paths: [string]` argument"))?;
    if paths.is_empty() {
        return Err(err("invalid_args", None, "`paths` must not be empty"));
    }
    if paths.len() > MAX_PATHS {
        return Err(err(
            "invalid_args",
            None,
            format!("too many paths: {} (max {MAX_PATHS} per call)", paths.len()),
        ));
    }

    // Validate everything BEFORE mutating anything: a bad path fails
    // the whole call loudly instead of half-applying the request.
    let mut requested: Vec<PathBuf> = Vec::new();
    for p in paths {
        let s = p
            .as_str()
            .ok_or_else(|| err("invalid_args", None, "`paths` entries must be strings"))?;
        if s.len() > MAX_PATH_BYTES {
            return Err(err(
                "invalid_args",
                None,
                format!("path exceeds {MAX_PATH_BYTES} bytes: {:.64}…", s),
            ));
        }
        requested.push(validated(s)?);
    }
    requested.sort();
    requested.dedup();

    // One writer at a time from here to the end of the rebuild: two
    // late clients handing config concurrently would otherwise
    // interleave load→append→write and lose each other's entries.
    let add_guard = add_lock().lock().await;

    let served = served_workspaces(server);
    let (already, missing): (Vec<PathBuf>, Vec<PathBuf>) =
        requested.into_iter().partition(|p| served.contains(p));

    let results_for = |path: &Path, status: &str, repo_id: Option<&str>, indexing: &str| {
        json!({
            "path": path.display().to_string(),
            "status": status,
            "repo_id": repo_id,
            "indexing": indexing,
        })
    };

    if missing.is_empty() {
        let results: Vec<Value> = already
            .iter()
            .map(|p| {
                let id = served_repo_id(server, p);
                results_for(p, "already", id.as_deref(), "ready")
            })
            .collect();
        return Ok(json!({
            "ok": true,
            "results": results,
            "served": json_paths(&served_workspaces(server)),
        }));
    }

    // The hot-add path is the federation reload path; a server with
    // neither a federation nor a `repos.yaml` has nothing to reload.
    let repos_yaml = server.repos_yaml().ok_or_else(|| {
        err(
            "not_federated",
            None,
            "this server has no federation config to extend; start the shared daemon via `lain mcp`",
        )
    })?;
    if server.federation().is_none() {
        return Err(err(
            "not_federated",
            None,
            "this server does not serve a federation; cannot hot-add workspaces",
        ));
    }

    // 1. Extend the daemon's `repos.yaml` — source of truth for every
    //    future reload (file watcher, signal socket, request_reload).
    let mut config = crate::server::federation::config::FederationConfig::load(repos_yaml)
        .map_err(|e| {
            err(
                "add_failed",
                None,
                format!("load {}: {e}", repos_yaml.display()),
            )
        })?;
    let mut taken: Vec<String> = config.repos.iter().map(|r| r.id.clone()).collect();
    let mut additions: Vec<(String, PathBuf)> = Vec::new();
    for path in &missing {
        let id = unique_id(path, &mut taken);
        config
            .repos
            .push(crate::server::federation::config::RepoConfig {
                id: id.clone(),
                source: crate::server::federation::config::SourceConfig::WorkspaceDir {
                    path: path.clone(),
                },
            });
        additions.push((id, path.clone()));
    }
    let yaml = serde_yaml::to_string(&config)
        .map_err(|e| err("add_failed", None, format!("serialize config: {e}")))?;
    crate::cli::io::write_file_atomic(repos_yaml, yaml.as_bytes()).map_err(|e| {
        err(
            "add_failed",
            None,
            format!("write {}: {e}", repos_yaml.display()),
        )
    })?;

    // 2. Apply it through the reload path (add + remove diff). This
    //    registers each repo, projects it, and spawns its index /
    //    relink / watcher work exactly like a hand-edited reload.
    let bus = server.reload_bus();
    crate::server::reload::run_rebuild(server, bus.as_ref())
        .await
        .map_err(|e| err("add_failed", None, format!("hot-add rebuild failed: {e}")))?;

    // Rebuild done — hand the lock to the next adder before waiting on
    // indexing, which can take a full `INDEX_WAIT`.
    drop(add_guard);

    // 3. Per-repo indexing status. The index runs in the background
    //    (same as cold boot), so wait — bounded — for each addition to
    //    reach a terminal health before answering.
    let mut results: Vec<Value> = Vec::new();
    for p in &already {
        let id = served_repo_id(server, p);
        results.push(results_for(p, "already", id.as_deref(), "ready"));
    }
    for (id, path) in &additions {
        let indexing = wait_for_index(server, id, INDEX_WAIT).await;
        results.push(results_for(path, "added", Some(id), &indexing));
    }

    Ok(json!({
        "ok": true,
        "results": results,
        "served": json_paths(&served_workspaces(server)),
    }))
}

/// Poll a hot-added repo's health until it leaves `Indexing`, or the
/// budget elapses. Returns the health label (`ready`, `degraded`, …)
/// as the per-repo indexing status. Async throughout: a blocking
/// sleep here would stall the tokio worker that is also running the
/// socket accept loop and every other connection's dispatch.
async fn wait_for_index(server: &LainServer, repo_id: &str, budget: Duration) -> String {
    let deadline = Instant::now() + budget;
    loop {
        let health = server
            .federation()
            .and_then(|fed| {
                fed.list_repos()
                    .into_iter()
                    .find(|(id, _)| id.as_str() == repo_id)
                    .map(|(_, h)| h)
            })
            .map(|h| h.as_str().to_string());
        match health.as_deref() {
            Some("ready") | Some("degraded") | Some("missing") | Some("unavailable") => {
                return health.unwrap_or_default();
            }
            _ => {
                if Instant::now() >= deadline {
                    return health.unwrap_or_else(|| "indexing".to_string());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn served_repo_id(server: &LainServer, path: &Path) -> Option<String> {
    let fed = server.federation()?;
    fed.list_repos()
        .into_iter()
        .find(|(id, _)| {
            fed.get_repo(id)
                .map(|r| {
                    r.source()
                        .local_path()
                        .canonicalize()
                        .map(|c| c == path)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        })
        .map(|(id, _)| id.to_string())
}

fn json_paths(paths: &[PathBuf]) -> Value {
    Value::Array(
        paths
            .iter()
            .map(|p| json!(p.display().to_string()))
            .collect(),
    )
}
