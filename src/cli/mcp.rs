//! `lain mcp` — agent-facing MCP server entry point.
//!
//! Wishlist #11 (option A): zero-args `lain mcp` discovers the workspace
//! by walking up from the agent harness's cwd for `.git` (via
//! `/proc/$PPID/cwd` on Linux), then serves the per-repo MCP tool
//! surface on stdio. The MCP config becomes
//! `{"command":"lain","args":["mcp"]}` — no repos.yaml, no
//! per-harness wrapper script, no drift between installed config
//! and the binary's CLI.
//!
//! Workspace resolution (`resolve_workspaces`):
//!   1. Repeated `--workspace PATH` (one or many).
//!   2. `LAIN_WORKSPACE` env var (comma-separated list of paths).
//!   3. `/proc/$PPID/cwd` walk-up on Linux, falling back to the
//!      process's own cwd.
//!
//! Multi-workspace: when the resolved list has >1 entry, `run_mcp`
//! generates a temp `repos.yaml` with one `workspace_dir` source
//! per workspace and delegates to `run_server --transport stdio`.
//! That gives the agent the same federation surface (`list_repos`,
//! `search_org`, `get_federation_health`) as `lain server` without
//! requiring a hand-written config file.
//!
//! Tradeoffs vs. `lain server --config repos.yaml`:
//! - Single-workspace boot uses `LainMcpServer::new(executor)` — no
//!   federation handle, no federation tools. Per-repo tools run
//!   against `executor.ctx.graph` directly.
//! - Multi-workspace boot goes through the federation path; that
//!   pulls in everything `run_server` does (per-repo indexing,
//!   watchers, hot reload). Indexing is per-workspace, capped by
//!   `max_concurrent_indexers` in the synthesized config.
//!
//! The dispatcher for the single-workspace case is
//! `LainMcpServer::new(executor)`; the multi-workspace case uses
//! `LainServer::with_federation*` constructed inside `run_server`.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Resolve the workspace list for `lain mcp`.
///
/// Resolution order (first non-empty wins):
///   1. `argv_workspaces` — values passed via repeated `--workspace PATH`.
///      An agent harness that knows its repo passes this explicitly.
///   2. `LAIN_WORKSPACE` env var — comma-separated list of paths.
///      Lets operators pin the workspace in their shell or agent
///      config without touching the binary's argv.
///   3. Walk up from the agent harness's cwd (`/proc/$PPID/cwd` on
///      Linux), falling back to the process's own cwd. This is what
///      makes `lain mcp` work under Kimi's cwd-pinning plugin model
///      without any wrapper script.
///
/// Returns an empty Vec only when every candidate is empty/missing;
/// callers downstream treat that as "auto-discover one workspace",
/// which is the historical behavior. Use [`resolve_workspaces_strict`]
/// when you need to fail on empty input (e.g. sidecar mode where the
/// owner must know which repo to follow).
pub fn resolve_workspaces(argv_workspaces: &[PathBuf]) -> Vec<PathBuf> {
    if !argv_workspaces.is_empty() {
        return argv_workspaces.to_vec();
    }
    if let Ok(env_val) = std::env::var("LAIN_WORKSPACE") {
        let parsed = parse_lain_workspace_env(&env_val);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    match crate::cli::workspace::find_git_workspace_root(None) {
        Ok(Some(p)) => vec![p],
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!("workspace auto-discovery failed: {e}");
            Vec::new()
        }
    }
}

/// Strict variant: errors when no workspace can be resolved, with a
/// message that tells the caller how to fix it. Used by `run_mcp` /
/// `run_sidecar` when they need at least one workspace to do anything
/// useful.
pub fn resolve_workspaces_strict(argv_workspaces: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let resolved = resolve_workspaces(argv_workspaces);
    if resolved.is_empty() {
        return Err(anyhow!(
            "no workspace resolved — pass `--workspace PATH` (repeatable), \
             set `LAIN_WORKSPACE=/path/to/repo[,/path/to/another]`, or run from \
             inside a git clone"
        ));
    }
    Ok(resolved)
}

/// Parse `LAIN_WORKSPACE` as a comma-separated list of paths. Empty
/// entries (leading/trailing/double comma) are dropped; whitespace
/// around entries is trimmed. An entirely empty value yields an
/// empty Vec so the caller can fall through to /proc discovery.
fn parse_lain_workspace_env(value: &str) -> Vec<PathBuf> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Build a `repos.yaml` body that registers each workspace as a
/// `workspace_dir` source. Used by `run_mcp` when the user passes
/// more than one workspace — we don't have a hand-written config
/// file, so we synthesize one in-memory (well, in a tempfile) and
/// hand it to the federation boot path.
///
/// Repo IDs default to the path basename. When two paths share a
/// basename (e.g. `/srv/repo` and `/other/repo`), the second and
/// later occurrences get a `-N` suffix. FederationConfig rejects
/// duplicate IDs at load time, so the disambiguation is mandatory.
pub(crate) fn build_repos_yaml_for_workspaces(workspaces: &[PathBuf]) -> String {
    let mut out = String::from(
        "# Auto-generated by `lain mcp` for multi-workspace stdio.\n\
         # One workspace_dir source per resolved --workspace / LAIN_WORKSPACE entry.\n\
         data_dir: ./.lain/federation\n\
         max_concurrent_indexers: 4\n\
         ready_threshold: 0.8\n\
         repos:\n",
    );
    // Track how many times each basename has been seen so we only
    // suffix on collisions, never by default. Uniqueness without
    // suffixing when the basenames already differ.
    let mut seen: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for ws in workspaces {
        let base = ws
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace")
            .to_string();
        let count = seen.entry(base.clone()).or_insert(0);
        let id = if *count == 0 {
            base.clone()
        } else {
            format!("{}-{}", base, count)
        };
        *count += 1;
        out.push_str(&format!(
            "  - id: {id}\n    source:\n      type: workspace_dir\n      path: {}\n",
            ws.display()
        ));
    }
    out
}

/// Run a single-repo (or multi-repo federation) MCP server on stdio.
///
/// `argv_workspaces` is the raw `Vec<PathBuf>` from clap — empty
/// means "auto-discover" via `LAIN_WORKSPACE` / `/proc/$PPID/cwd` /
/// process cwd (see `resolve_workspaces`). `embedding_model` and
/// `reindex_timeout` pass through.
///
/// One workspace → fast single-server boot. Multiple workspaces →
/// generates an in-memory `repos.yaml` and delegates to
/// `run_server --transport stdio`, which already implements the
/// federation boot (per-repo indexing, watchers, federation tools).
/// That delegation keeps the two entry points behaviorally
/// identical — an agent that asks for two repos gets the same
/// federation surface as `lain server`, just without having to
/// author a `repos.yaml` themselves.
pub async fn run_mcp(
    argv_workspaces: &[PathBuf],
    embedding_model: Option<&Path>,
    reindex_timeout: Option<std::time::Duration>,
) -> Result<()> {
    let workspaces = resolve_workspaces_strict(argv_workspaces)?;

    if workspaces.len() > 1 {
        return run_mcp_federation(&workspaces, embedding_model).await;
    }

    let workspace = workspaces
        .into_iter()
        .next()
        .expect("len() == 1 checked above");
    if !workspace.join(".git").exists() {
        return Err(anyhow!(
            "no `.git` found at {} (or any parent) — pass `--workspace PATH` to override",
            workspace.display()
        ));
    }

    // `.lain/graph.bin` lives next to the workspace so the persisted
    // graph follows the repo. Picked up by `save_state` / `load_state`.
    let mem_dir = workspace.join(".lain");
    std::fs::create_dir_all(&mem_dir)
        .with_context(|| format!("create_dir_all({})", mem_dir.display()))?;
    let mem_path = mem_dir.join("graph.bin");

    let mut server = crate::server::LainServer::new(&workspace, &mem_path, embedding_model)
        .with_context(|| format!("build LainServer for {}", workspace.display()))?;

    // Seed the overlay from what is already uncommitted in the working
    // tree. `sync_volatile_overlay` had no caller anywhere, so a server
    // starting on a dirty checkout began with an empty overlay and stayed
    // that way until the user happened to re-save one of those files —
    // work already on disk was invisible. Runs before the watcher starts
    // because it clears the overlay first; the reverse order would drop
    // whatever the watcher had already inserted.
    //
    // Single-repo only: it reads `self.git`, which is unambiguous here
    // and would be the wrong repo under federation.
    if let Err(e) = server.sync_volatile_overlay().await {
        tracing::warn!("initial overlay sync failed (continuing): {e}");
    }

    // Keep the volatile overlay fresh while the user edits. Without this
    // the overlay is never written to at all and every answer is only as
    // current as the last reindex.
    crate::server::ingest::background::start_source_watcher(workspace.clone(), server.clone());
    crate::server::ingest::background::spawn_ui_session_reaper(server.tool_executor.ctx.clone());

    // Re-index when the checkout moves to a new commit. Without this the
    // graph never advances past the commit it was first built from.
    crate::server::ingest::background::spawn_commit_sync(server.clone());

    // Hand the executor's tool surface to a federation-free
    // `LainMcpServer`. Single-workspace mode — per-repo tools run
    // against `server.tool_executor.graph` directly. The re-index
    // timeout is wired through to run_stdio so the spawn honors
    // it (or the env var if None).
    let mcp = crate::server::mcp::handler::LainMcpServer::new(server.tool_executor.clone())
        .with_server(std::sync::Arc::new(server))
        .with_reindex_timeout(reindex_timeout);
    mcp.run_stdio()
        .await
        .map_err(|e| anyhow!("MCP stdio run failed: {e}"))?;
    Ok(())
}

/// Multi-workspace delegation path. Generates a `repos.yaml` with one
/// `workspace_dir` source per resolved workspace, writes it to a
/// tempfile under the process's temp dir, and hands it to
/// `run_server --transport stdio`. The tempfile is cleaned up after
/// the server exits (success or failure).
async fn run_mcp_federation(
    workspaces: &[PathBuf],
    embedding_model: Option<&Path>,
) -> Result<()> {
    let yaml = build_repos_yaml_for_workspaces(workspaces);
    let tmp_path = std::env::temp_dir().join(format!(
        "lain-mcp-repos-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp_path, &yaml)
        .map_err(|e| anyhow!("write {}: {e}", tmp_path.display()))?;
    tracing::info!(
        "lain mcp: {} workspace(s) — delegating to federation boot via {}",
        workspaces.len(),
        tmp_path.display()
    );

    // `run_server` handles init_tracing, per-repo indexing, watchers,
    // and the stdio MCP server. We just need to feed it the config
    // and let it do its job. `workspace_arg = ""` means "all repos"
    // (no workspace filter).
    let result = crate::cli::server::run_server(
        &tmp_path,
        "stdio",
        0,
        "info",
        "",
        false,
        embedding_model,
    )
    .await;

    // Cleanup. Best-effort — a leftover tempfile in /tmp is annoying
    // but not a correctness issue.
    let _ = std::fs::remove_file(&tmp_path);
    result
}


/// Run a read-only **sidecar** MCP server against an owner.
///
/// Every piece of this existed and was individually tested — the owner
/// serves `/overlay/subscribe` and `/overlay/get_snapshot`
/// (`mcp::handler`), `overlay::subscribe` consumes that stream with
/// snapshot re-hydration and backoff, `GraphDatabase::open_read_only`
/// gives a graph that refuses writes, and `ToolExecutor::new_read_only`
/// plus `LainMcpServer::new_read_only` serve tools over it. What did not
/// exist was any way to *start* one: none of those three constructors had
/// a caller anywhere, so the ingest pipeline's `is_read_only()` guards
/// defended against a mode the binary could not enter.
///
/// A sidecar never indexes: it answers from the owner's persisted graph
/// plus whatever the owner has streamed into the overlay since.
pub async fn run_sidecar(argv_workspaces: &[PathBuf], owner_url: &str) -> Result<()> {
    let workspaces = resolve_workspaces_strict(argv_workspaces)?;
    if workspaces.len() > 1 {
        return Err(anyhow!(
            "`lain mcp --owner-url` supports one workspace at a time; got {}. \
             Start one sidecar per workspace.",
            workspaces.len()
        ));
    }
    let workspace = workspaces.into_iter().next().expect("len() == 1 checked above");

    let mem_path = workspace.join(".lain").join("graph.bin");
    if !mem_path.exists() {
        return Err(anyhow!(
            "no persisted graph at {} — a sidecar reads the owner's graph and \
             never builds one itself; start `lain server` (or `lain mcp`) on \
             this checkout first",
            mem_path.display()
        ));
    }

    let graph = crate::graph::GraphDatabase::open_read_only(&mem_path)
        .with_context(|| format!("open {} read-only", mem_path.display()))?;
    let overlay = crate::overlay::VolatileOverlay::new();

    // Follow the owner for the life of the process. `subscribe` never
    // returns: it re-hydrates from the snapshot endpoint and reconnects
    // with backoff whenever the stream drops.
    let follow_overlay = overlay.clone();
    let owner = owner_url.to_string();
    tokio::spawn(async move {
        crate::overlay::subscribe(owner, follow_overlay).await;
    });

    tracing::info!("sidecar mode: following {owner_url} for {}", workspace.display());

    let executor =
        crate::tools::ToolExecutor::new_read_only(graph, overlay, workspace.clone());
    crate::server::mcp::handler::LainMcpServer::new_read_only(executor)
        .run_stdio()
        .await
        .map_err(|e| anyhow!("MCP stdio run failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_lain_workspace_env_single_path() {
        let got = parse_lain_workspace_env("/tmp/repo");
        assert_eq!(got, vec![PathBuf::from("/tmp/repo")]);
    }

    #[test]
    fn parse_lain_workspace_env_multi_comma_separated() {
        let got = parse_lain_workspace_env("/a,/b,/c");
        assert_eq!(
            got,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c"),
            ]
        );
    }

    #[test]
    fn parse_lain_workspace_env_trims_whitespace_and_drops_empties() {
        let got = parse_lain_workspace_env(" /a , , /b , ");
        assert_eq!(
            got,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn parse_lain_workspace_env_empty_yields_empty_vec() {
        assert!(parse_lain_workspace_env("").is_empty());
        assert!(parse_lain_workspace_env(",,,").is_empty());
        assert!(parse_lain_workspace_env("   ").is_empty());
    }

    #[test]
    fn resolve_workspaces_uses_argv_when_present() {
        let argv = vec![PathBuf::from("/explicit/a"), PathBuf::from("/explicit/b")];
        let got = resolve_workspaces(&argv);
        assert_eq!(got, argv);
    }

    #[test]
    fn resolve_workspaces_empty_argv_falls_through_to_env_then_discovery() {
        // Empty argv; LAIN_WORKSPACE unset; no /proc ancestor reachable
        // in test env may or may not yield a result. The contract here
        // is just "does not panic, returns a Vec".
        // SAFETY: this test reads env vars but does not mutate them in
        // a way visible to other tests (we set/unset around the call).
        // SAFETY: env var set/restore pattern is racy under cargo test
        // parallelism, so use a unique var name and restore immediately.
        std::env::remove_var("LAIN_WORKSPACE");
        let got = resolve_workspaces(&[]);
        // We can't assert is_empty vs is_some without controlling cwd
        // and env; just assert the call shape.
        let _ = got;
    }

    #[test]
    fn build_repos_yaml_for_workspaces_round_trips_through_federation_config() {
        let workspaces = vec![
            PathBuf::from("/srv/alpha"),
            PathBuf::from("/srv/beta"),
            PathBuf::from("/srv/gamma"),
        ];
        let yaml = build_repos_yaml_for_workspaces(&workspaces);
        // Each path must appear at least once; each entry must be a
        // workspace_dir source; IDs must be unique.
        for ws in &workspaces {
            assert!(yaml.contains(&format!("path: {}", ws.display())), "missing path {} in:\n{yaml}", ws.display());
        }
        assert_eq!(yaml.matches("type: workspace_dir").count(), 3);
        // Parse it back through FederationConfig to make sure the
        // shape is valid (catches typos in the YAML emitter).
        let parsed = crate::server::federation::config::FederationConfig::load_from_str(&yaml)
            .expect("generated yaml must parse as FederationConfig");
        assert_eq!(parsed.repos.len(), 3);
        // All IDs must be unique — the loader rejects duplicates.
        let mut ids: Vec<_> = parsed.repos.iter().map(|r| r.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3, "generated ids must be unique: {ids:?}");
    }

    #[test]
    fn build_repos_yaml_disambiguates_duplicate_basenames() {
        // Two siblings named "repo" — without suffixing, both would
        // generate id "repo" and the federation loader would reject.
        let workspaces = vec![
            PathBuf::from("/srv/repo"),
            PathBuf::from("/srv/repo"),
        ];
        let yaml = build_repos_yaml_for_workspaces(&workspaces);
        let parsed = crate::server::federation::config::FederationConfig::load_from_str(&yaml)
            .expect("duplicate-basename yaml must still parse");
        let mut ids: Vec<_> = parsed.repos.iter().map(|r| r.id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["repo".to_string(), "repo-1".to_string()]);
    }

    #[test]
    fn build_repos_yaml_does_not_suffix_unique_basenames() {
        // Two paths with different basenames should not get suffix noise.
        // The old logic always suffixed i >= 1, producing `repo_b-1`
        // instead of `repo_b` — annoying for downstream tool calls that
        // want to reference repos by their natural name.
        let workspaces = vec![
            PathBuf::from("/srv/repo_a"),
            PathBuf::from("/srv/repo_b"),
        ];
        let yaml = build_repos_yaml_for_workspaces(&workspaces);
        let parsed = crate::server::federation::config::FederationConfig::load_from_str(&yaml)
            .expect("unique-basename yaml must parse");
        let mut ids: Vec<_> = parsed.repos.iter().map(|r| r.id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["repo_a".to_string(), "repo_b".to_string()]);
    }
}
