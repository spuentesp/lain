//! Snapshot tools (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §12 + §13).
//!
//! `prepare_snapshot` and `get_snapshot` are the two MCP tools the
//! `contracts` package ships for revision-pinning. Both read from
//! the in-process [`SnapshotManager`] the server constructs at boot
//! (see `mcp::handler::McpContext::snapshots`). `prepare_snapshot`
//! resolves refs, deduplicates by snapshot id, queues jobs, and
//! waits up to `wait_ms`. `get_snapshot` reads the on-disk record
//! and refreshes `last_access_unix` on reads per §8.4.
//!
//! Both tools follow the §10.2 envelope: `success_envelope` for the
//! happy path, `error_outcome` for `snapshot_not_found` /
//! `repo_not_registered` / `ref_not_found` / `busy` /
//! `invalid_argument`. `busy` carries `retry_after_ms` per §13.
//!
//! ## Limits / sort
//!
//! - `prepare_snapshot.repos`: the tool accepts an object map
//!   `repo -> ref|sha`. The handler applies the federation's
//!   `ContractFederationConfig` after ref resolution.
//! - `get_snapshot`: a snapshot id is required.
//!
//! ## Idempotence
//!
//! §13: "same inputs after ref resolution → same id without
//! re-indexing; never touches the live index". The tool hands the
//! record's last-known state to the manager's `prepare` path; if
//! the id matches an existing `ready`/`failed` record the manager
//! returns it without re-indexing.

use super::envelope::{cap_2000, error_outcome, outcome, success_envelope};
use super::{ContractToolEntry, ContractToolFuture, ToolOutcome};
use crate::federation::contracts::config::ContractFederationConfig;
use crate::federation::contracts::snapshots::manager::{PrepareError, PrepareRequest};
use crate::server::mcp::handler::McpContext;
use serde_json::{json, Map, Value};
use std::time::Instant;

// ─── dispatch ─────────────────────────────────────────────────────────

inventory::submit!(ContractToolEntry {
    name: "prepare_snapshot",
    handler: prepare_snapshot_handle,
});

inventory::submit!(ContractToolEntry {
    name: "get_snapshot",
    handler: get_snapshot_handle,
});

/// `prepare_snapshot` async handler. Maps the JSON args to a
/// [`PrepareRequest`] and runs it through the [`SnapshotManager`].
pub fn prepare_snapshot_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
    let args_map: Map<String, Value> = match args {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let started = Instant::now();
    Box::pin(async move {
        match run_prepare_snapshot(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

/// `get_snapshot` async handler.
pub fn get_snapshot_handle<'a>(ctx: &'a McpContext<'a>, args: Value) -> ContractToolFuture<'a> {
    let args_map: Map<String, Value> = match args {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let started = Instant::now();
    Box::pin(async move {
        match run_get_snapshot(ctx, args_map, started).await {
            Ok(o) => Ok(o),
            Err(o) => Ok(o),
        }
    })
}

// ─── prepare_snapshot implementation ─────────────────────────────────

async fn run_prepare_snapshot(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snapshots = ctx.snapshots.ok_or_else(|| {
        error_outcome(
            "snapshot_manager_unavailable",
            "snapshot manager is not configured for this server",
            None,
            "live",
            started,
        )
    })?;

    let repos = parse_repos(&args_map).map_err(|m| invalid_argument(m, started))?;
    let excluded = parse_excluded(&args_map);
    let from = args_map
        .get("from")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let max_base_age_s = args_map.get("max_base_age_s").and_then(|v| v.as_u64());
    let wait_ms = args_map
        .get("wait_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(crate::federation::contracts::snapshots::manager::DEFAULT_SNAPSHOT_WAIT_MS);
    let config = ctx
        .federation
        .and_then(|f| f.contract_config())
        .unwrap_or_else(|| Arc::new(ContractFederationConfig::default()));

    if let Some(from_id) = from.as_deref() {
        if !from_id.is_empty()
            && !crate::federation::contracts::snapshots::record::SNAPSHOT_ID_PREFIX.is_empty()
            && !from_id
                .starts_with(crate::federation::contracts::snapshots::record::SNAPSHOT_ID_PREFIX)
        {
            // §12 accepts only `snap_*` ids for `from`.
            return Err(error_outcome(
                "invalid_argument",
                format!("`from` snapshot id must start with `snap_`: {from_id:?}"),
                Some(json!({"arg": "from"})),
                "live",
                started,
            ));
        }
    }

    let req = PrepareRequest {
        repos: repos.clone(),
        // The caller-named refs — we record what the operator
        // typed before ref resolution so `get_snapshot` can
        // surface the input form alongside the resolved commit.
        refs_: repos,
        excluded,
        from,
        max_base_age_s,
        wait_ms,
        config,
    };

    let result = Arc::clone(snapshots).prepare(req).await;
    match result {
        Ok(outcome_record) => {
            // Build the success envelope. The manager's `view`
            // already carries the §12 JSON shape (`snapshot`,
            // `state`, `repos`); we copy it under `data`.
            let data = outcome_record.view;
            let snapshot_id = data["snapshot"].as_str().unwrap_or("").to_string();
            let envelope = success_envelope(data.clone(), &snapshot_id, true, started);
            let text = render_prepare_snapshot(&data);
            Ok(outcome(envelope, &data, text))
        }
        Err(PrepareError::SnapshotNotFound { snapshot }) => Err(error_outcome(
            "snapshot_not_found",
            format!("snapshot {snapshot:?} not found"),
            Some(json!({"snapshot": snapshot})),
            "live",
            started,
        )),
        Err(PrepareError::RepoNotRegistered { repo }) => Err(error_outcome(
            "repo_not_registered",
            format!("repo {repo:?} is not configured"),
            Some(json!({"repo": repo})),
            "live",
            started,
        )),
        Err(PrepareError::InvalidArgument { message }) => Err(error_outcome(
            "invalid_argument",
            message,
            None,
            "live",
            started,
        )),
        Err(PrepareError::Busy { retry_after_ms }) => Err(error_outcome(
            "busy",
            "snapshot queue is full or no residency slot available within wait_ms",
            Some(json!({"retry_after_ms": retry_after_ms})),
            "live",
            started,
        )),
        Err(PrepareError::RefNotFound { entries }) => {
            // §13: every failure was a missing ref/sha after one
            // fetch. The first (repo, ref) pair is the canonical
            // detail; the full list is in `details.entries` for
            // tools that need it.
            let first = entries.first().cloned();
            Err(error_outcome(
                "ref_not_found",
                format!("ref or sha does not exist after one fetch: {:?}", first),
                Some(json!({
                    "entries": entries,
                    "repo": first.as_ref().map(|(r, _)| r.clone()),
                    "ref": first.as_ref().map(|(_, r)| r.clone()),
                })),
                "live",
                started,
            ))
        }
        Err(PrepareError::Other(message)) => Err(error_outcome(
            "internal_error",
            message,
            None,
            "live",
            started,
        )),
    }
}

// ─── get_snapshot implementation ─────────────────────────────────────

async fn run_get_snapshot(
    ctx: &McpContext<'_>,
    args_map: Map<String, Value>,
    started: Instant,
) -> Result<ToolOutcome, ToolOutcome> {
    let snapshots = ctx.snapshots.ok_or_else(|| {
        error_outcome(
            "snapshot_manager_unavailable",
            "snapshot manager is not configured for this server",
            None,
            "live",
            started,
        )
    })?;

    let snapshot_id = args_map
        .get("snapshot")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            error_outcome(
                "invalid_argument",
                "missing required argument: snapshot",
                Some(json!({"arg": "snapshot"})),
                "live",
                started,
            )
        })?
        .to_string();
    let wait_ms = args_map
        .get("wait_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(crate::federation::contracts::snapshots::manager::DEFAULT_SNAPSHOT_WAIT_MS);

    // §12 table note: `get_snapshot` on `live` returns a
    // readiness-shaped answer derived from the federation's
    // current `RepoHealth` (§8.7). No record is written; no
    // residency slot is consumed.
    if snapshot_id == "live" {
        let fed = ctx.federation.ok_or_else(|| {
            error_outcome(
                "federation_disabled",
                "this server is not configured with a federation",
                None,
                "live",
                started,
            )
        })?;
        let prepared = Arc::clone(snapshots).live_readiness_view(|| live_per_repo(fed));
        let data = prepared.view;
        let envelope = success_envelope(data.clone(), "live", false, started);
        let text = render_get_snapshot(&data);
        return Ok(outcome(envelope, &data, text));
    }

    let result = Arc::clone(snapshots).get(&snapshot_id, wait_ms).await;
    match result {
        Ok(outcome_record) => {
            let data = outcome_record.view;
            let envelope = success_envelope(data.clone(), &snapshot_id, true, started);
            let text = render_get_snapshot(&data);
            Ok(outcome(envelope, &data, text))
        }
        Err(PrepareError::SnapshotNotFound { snapshot }) => Err(error_outcome(
            "snapshot_not_found",
            format!("snapshot {snapshot:?} not found"),
            Some(json!({"snapshot": snapshot})),
            "live",
            started,
        )),
        Err(PrepareError::RefNotFound { entries }) => {
            let first = entries.first().cloned();
            Err(error_outcome(
                "ref_not_found",
                format!("ref or sha does not exist after one fetch: {:?}", first),
                Some(json!({
                    "entries": entries,
                    "repo": first.as_ref().map(|(r, _)| r.clone()),
                    "ref": first.as_ref().map(|(_, r)| r.clone()),
                })),
                "live",
                started,
            ))
        }
        Err(PrepareError::InvalidArgument { message }) => Err(error_outcome(
            "invalid_argument",
            message,
            None,
            "live",
            started,
        )),
        Err(PrepareError::Other(message)) => Err(error_outcome(
            "internal_error",
            message,
            None,
            "live",
            started,
        )),
        // `prepare_snapshot`-only error paths that `get_snapshot`
        // cannot hit; map to a clean argument error if they do.
        Err(other) => Err(error_outcome(
            "internal_error",
            format!("{other:?}"),
            None,
            "live",
            started,
        )),
    }
}

/// Build the per-repo `RepoSnapshotState` list for the live
/// federation per §8.7's mapping. Used by `get_snapshot("live")`.
fn live_per_repo(
    fed: &crate::federation::federated_index::FederatedIndex,
) -> Vec<(
    String,
    crate::federation::contracts::snapshots::RepoSnapshotState,
)> {
    use crate::federation::contracts::snapshots::RepoSnapshotState;
    use crate::federation::health::RepoHealth;
    let mut out: Vec<(String, RepoSnapshotState)> = Vec::new();
    for (id, health) in fed.list_repos() {
        let commit = fed
            .get_repo(&id)
            .and_then(|r| r.db().get_last_commit().ok().flatten());
        let state = match (health, &commit) {
            (RepoHealth::Ready, Some(c)) => RepoSnapshotState::Cached { commit: c.clone() },
            (RepoHealth::Indexing, Some(c)) => RepoSnapshotState::Indexing { commit: c.clone() },
            (RepoHealth::Degraded, Some(c)) => RepoSnapshotState::Failed {
                commit: c.clone(),
                error: "degraded".into(),
            },
            (RepoHealth::Unavailable, Some(c)) => RepoSnapshotState::Failed {
                commit: c.clone(),
                error: "unavailable".into(),
            },
            (RepoHealth::Missing, Some(c)) => RepoSnapshotState::Failed {
                commit: c.clone(),
                error: "missing".into(),
            },
            // Without a commit we still surface the health as the
            // state — the caller may not have indexed the repo yet.
            (RepoHealth::Ready, None) => RepoSnapshotState::Queued {
                commit: String::new(),
            },
            _ => RepoSnapshotState::Failed {
                commit: commit.unwrap_or_default(),
                error: format!("{:?}", health).to_lowercase(),
            },
        };
        out.push((id.as_str().to_string(), state));
    }
    // §8.7: repos in repos.yaml that failed to load are mapped
    // to `unreviewed: failed`. They appear here without a
    // `RepoId` (no `RepoIndex`) — the tool layer reports them as
    // failed with the loader error string.
    for (repo, error) in fed.load_errors() {
        out.push((
            repo,
            RepoSnapshotState::Failed {
                commit: String::new(),
                error,
            },
        ));
    }
    out
}

// ─── helpers ──────────────────────────────────────────────────────────

use std::collections::BTreeMap;
use std::sync::Arc;

fn parse_repos(args: &Map<String, Value>) -> Result<BTreeMap<String, String>, String> {
    let raw = match args.get("repos") {
        Some(Value::Object(m)) => m,
        Some(Value::Null) | None => return Ok(BTreeMap::new()),
        Some(other) => {
            return Err(format!(
                "repos must be an object of repo -> ref; got {}",
                kind(other)
            ))
        }
    };
    let mut out = BTreeMap::new();
    for (k, v) in raw {
        let s = match v {
            Value::String(s) => s.clone(),
            _ => return Err(format!("repos.{k} must be a ref string; got {}", kind(v))),
        };
        out.insert(k.clone(), s);
    }
    Ok(out)
}

fn parse_excluded(args: &Map<String, Value>) -> Vec<String> {
    match args.get("exclude") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn invalid_argument(message: String, started: Instant) -> ToolOutcome {
    error_outcome("invalid_argument", message, None, "live", started)
}

fn render_prepare_snapshot(data: &Value) -> String {
    let snapshot = data["snapshot"].as_str().unwrap_or("");
    let state = data["state"].as_str().unwrap_or("");
    let repos = data["repos"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("# {snapshot} ({state})\n\n"));
    for r in &repos {
        let repo = r["repo"].as_str().unwrap_or("");
        let state = r["state"].as_str().unwrap_or("");
        let commit = r["commit"].as_str().unwrap_or("");
        let line = match state {
            "excluded" => format!("- {repo} (excluded)\n"),
            _ => format!("- {repo} ({state} @ {commit})\n"),
        };
        out.push_str(&line);
    }
    cap_2000(out)
}

fn render_get_snapshot(data: &Value) -> String {
    let snapshot = data["snapshot"].as_str().unwrap_or("");
    let state = data["state"].as_str().unwrap_or("");
    let repos = data["repos"].as_array().cloned().unwrap_or_default();
    let mut out = String::new();
    out.push_str(&format!("# {snapshot} ({state})\n\n"));
    for r in &repos {
        let repo = r["repo"].as_str().unwrap_or("");
        let state = r["state"].as_str().unwrap_or("");
        let commit = r["commit"].as_str().unwrap_or("");
        let line = match state {
            "excluded" => format!("- {repo} (excluded)\n"),
            _ => format!("- {repo} ({state} @ {commit})\n"),
        };
        out.push_str(&line);
    }
    cap_2000(out)
}

// Apply optional `limit` paging to the `repos` list. §12 does not
// bound the per-tool repo count, but the §10.5 default + max
// apply to anything that paginates. Snapshots carry every repo
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repos_handles_object_map() {
        let mut m = Map::new();
        m.insert(
            "repos".to_string(),
            json!({"orders": "main", "billing": "abc1234"}),
        );
        let parsed = parse_repos(&m).unwrap();
        assert_eq!(parsed.get("orders"), Some(&"main".to_string()));
        assert_eq!(parsed.get("billing"), Some(&"abc1234".to_string()));
    }

    #[test]
    fn parse_repos_handles_omitted() {
        let m = Map::new();
        assert!(parse_repos(&m).unwrap().is_empty());
    }

    #[test]
    fn parse_repos_rejects_non_object() {
        let mut m = Map::new();
        m.insert("repos".to_string(), json!("orders"));
        assert!(parse_repos(&m).is_err());
    }

    #[test]
    fn parse_repos_rejects_non_string_value() {
        let mut m = Map::new();
        m.insert("repos".to_string(), json!({"orders": 42}));
        assert!(parse_repos(&m).is_err());
    }

    #[test]
    fn parse_excluded_filters_to_strings() {
        let mut m = Map::new();
        m.insert("exclude".to_string(), json!(["reports", 42, "platform"]));
        let parsed = parse_excluded(&m);
        assert_eq!(parsed, vec!["reports", "platform"]);
    }

    #[test]
    fn render_prepare_snapshot_includes_state() {
        let data = json!({
            "snapshot": "snap_abc",
            "state": "ready",
            "repos": [
                {"repo": "orders", "state": "cached", "commit": "abc"},
                {"repo": "reports", "state": "excluded"},
            ]
        });
        let out = render_prepare_snapshot(&data);
        assert!(out.contains("snap_abc"));
        assert!(out.contains("ready"));
        assert!(out.contains("orders (cached @ abc)"));
        assert!(out.contains("reports (excluded)"));
    }

    #[test]
    fn render_get_snapshot_includes_state() {
        let data = json!({
            "snapshot": "snap_xyz",
            "state": "failed",
            "repos": [
                {"repo": "orders", "state": "failed", "commit": "abc", "error": "ref_not_found"},
            ]
        });
        let out = render_get_snapshot(&data);
        assert!(out.contains("snap_xyz"));
        assert!(out.contains("failed"));
        assert!(out.contains("orders (failed @ abc)"));
    }

    #[test]
    fn api_version_constant_is_one() {
        assert_eq!(super::super::API_VERSION, 1);
        assert!(!super::super::ANALYZER_VERSION.is_empty());
    }
}
