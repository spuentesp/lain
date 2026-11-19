//! §9.6 scope rendering and the live `RepoHealth` → scope mapping
//! (`§8.7`).

use crate::federation::federated_index::FederatedIndex;
use crate::federation::health::RepoHealth;
use serde_json::{json, Value};

/// Render the §9.6 scope sentence: always included when `scope` is
/// present (`§10.2`). The shape mirrors the §9.6 example verbatim so
/// an agent that hard-codes the string still recognises it.
///
/// "No known impact in N reviewed repos." — when `unreviewed` is
/// empty.
///
/// "No known impact in N reviewed repos. M configured repo(s) could
/// not be reviewed: <comma list>." — when `unreviewed` is non-empty.
///
/// The output never exceeds ~200 chars; the per-tool text cap
/// (`envelope::cap_2000`) is still applied.
pub fn render_sentence(scope: &Value) -> String {
    let reviewed = scope
        .get("reviewed")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let unreviewed: Vec<String> = scope
        .get("unreviewed")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let repo = r.get("repo").and_then(|s| s.as_str())?;
                    let reason = r.get("reason").and_then(|s| s.as_str())?;
                    Some(format!("{repo} ({reason})"))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut out = format!("No known impact in {reviewed} reviewed repos.");
    if !unreviewed.is_empty() {
        out.push(' ');
        out.push_str(&format!(
            "{} configured repo{} could not be reviewed: {}.",
            unreviewed.len(),
            if unreviewed.len() == 1 { "" } else { "s" },
            unreviewed.join(", "),
        ));
    }
    out
}

/// Build the §9.6 scope for the live federation: one `reviewed`
/// entry per `RepoHealth::Ready` repo, one `unreviewed` entry per
/// other repo (`Indexing → not_ready`, `Degraded` / `Unavailable` /
/// `Missing → failed`). `configured_only: true` is set per §9.6.
///
/// `dirty` is `true` when the repo's overlay has uncommitted changes
/// (the §10.8 `EvidenceRef.dirty` rule). The brief says: "live
/// `EvidenceRef.commit` = `GraphDatabase::get_last_commit`, `dirty:
/// true` when overlay has uncommitted changes." The overlay-dirty
/// signal here is `overlay_paths` non-empty (every cycle where
/// `sync_overlay` saw uncommitted changes leaves at least one entry).
/// The accessor below is on `RepoIndex` — see the small wrapper
/// added there for this PR.
pub fn live_scope(fed: &FederatedIndex) -> Value {
    let mut reviewed: Vec<Value> = Vec::new();
    let mut unreviewed: Vec<Value> = Vec::new();
    for (id, health) in fed.list_repos() {
        match health {
            RepoHealth::Ready => {
                let commit = fed
                    .get_repo(&id)
                    .and_then(|r| r.db().get_last_commit().ok().flatten());
                let dirty = fed.get_repo(&id).map(|r| r.overlay_has_pending_changes());
                reviewed.push(json!({
                    "repo": id.as_str(),
                    "commit": commit,
                    "dirty": dirty.unwrap_or(false),
                }));
            }
            RepoHealth::Indexing => {
                unreviewed.push(json!({"repo": id.as_str(), "reason": "not_ready"}))
            }
            RepoHealth::Degraded | RepoHealth::Unavailable | RepoHealth::Missing => {
                // Carry the indexer's own explanation. A timeout was reported
                // as a bare `failed` with `error: null`; only the server log
                // had the reason.
                let error = fed.get_repo(&id).and_then(|r| r.last_index_error());
                let cause = error.as_deref().map(|e| {
                    if e.starts_with(crate::federation::repo_index::INDEX_TIMEOUT_CAUSE) {
                        crate::federation::repo_index::INDEX_TIMEOUT_CAUSE
                    } else {
                        "index_error"
                    }
                });
                unreviewed.push(
                    json!({"repo": id.as_str(), "reason": "failed", "cause": cause, "error": error}),
                )
            }
        }
    }
    // Repos in repos.yaml that failed to load — same mapping.
    for (repo, error) in fed.load_errors() {
        unreviewed.push(json!({
            "repo": repo,
            "reason": "failed",
            "error": error,
        }));
    }
    reviewed.sort_by(|a, b| {
        a.get("repo")
            .and_then(|v| v.as_str())
            .cmp(&b.get("repo").and_then(|v| v.as_str()))
    });
    unreviewed.sort_by(|a, b| {
        a.get("repo")
            .and_then(|v| v.as_str())
            .cmp(&b.get("repo").and_then(|v| v.as_str()))
    });
    json!({
        "reviewed": reviewed,
        "unreviewed": unreviewed,
        "configured_only": true,
    })
}

pub fn snapshot_scope(
    record: &crate::federation::contracts::snapshots::record::SnapshotRecord,
) -> Value {
    let mut reviewed: Vec<Value> = Vec::new();
    let mut unreviewed: Vec<Value> = Vec::new();
    for (repo, commit) in &record.repos {
        let is_cached = record
            .repo_states
            .get(repo)
            .map(|s| {
                matches!(
                    s,
                    crate::federation::contracts::snapshots::RepoSnapshotState::Cached { .. }
                )
            })
            .unwrap_or(false);
        if is_cached {
            reviewed.push(json!({
                "repo": repo,
                "commit": commit,
                "dirty": false,
            }));
        }
    }
    for (repo, state) in &record.repo_states {
        let entry = match state {
            crate::federation::contracts::snapshots::RepoSnapshotState::Excluded => {
                Some(("excluded", None))
            }
            crate::federation::contracts::snapshots::RepoSnapshotState::Failed {
                error, ..
            } => Some(("failed", Some(error.clone()))),
            crate::federation::contracts::snapshots::RepoSnapshotState::Indexing { .. }
            | crate::federation::contracts::snapshots::RepoSnapshotState::Queued { .. } => {
                Some(("not_ready", None))
            }
            crate::federation::contracts::snapshots::RepoSnapshotState::Cached { .. } => None,
        };
        if let Some((reason, error)) = entry {
            let mut obj = json!({
                "repo": repo,
                "reason": reason,
            });
            if let Some(err) = error {
                obj["error"] = json!(err);
            }
            unreviewed.push(obj);
        }
    }
    reviewed.sort_by(|a, b| {
        a.get("repo")
            .and_then(|v| v.as_str())
            .cmp(&b.get("repo").and_then(|v| v.as_str()))
    });
    unreviewed.sort_by(|a, b| {
        a.get("repo")
            .and_then(|v| v.as_str())
            .cmp(&b.get("repo").and_then(|v| v.as_str()))
    });
    json!({
        "reviewed": reviewed,
        "unreviewed": unreviewed,
        "configured_only": true,
    })
}

pub fn empty_scope() -> Value {
    json!({
        "reviewed": [],
        "unreviewed": [],
        "configured_only": true,
    })
}

/// Whether the repo's health means `provider_reviewed: true` for
/// `get_service` (`§12` guarantee). Only `RepoHealth::Ready`
/// counts as reviewed.
pub fn provider_is_reviewed(health: RepoHealth) -> bool {
    matches!(health, RepoHealth::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_sentence_only_reviewed() {
        let s = json!({
            "reviewed": [{"repo": "orders"}, {"repo": "billing"}, {"repo": "reports"}],
            "unreviewed": [],
            "configured_only": true,
        });
        let out = render_sentence(&s);
        assert_eq!(out, "No known impact in 3 reviewed repos.");
    }

    #[test]
    fn render_sentence_with_unreviewed() {
        let s = json!({
            "reviewed": [{"repo": "orders"}, {"repo": "billing"}],
            "unreviewed": [{"repo": "reports", "reason": "excluded"}],
            "configured_only": true,
        });
        let out = render_sentence(&s);
        assert!(out.contains("2 reviewed repos"));
        assert!(out.contains("reports (excluded)"));
    }

    #[test]
    fn render_sentence_singular_repo() {
        let s = json!({
            "reviewed": [{"repo": "orders"}],
            "unreviewed": [{"repo": "reports", "reason": "not_ready"}],
            "configured_only": true,
        });
        let out = render_sentence(&s);
        assert!(out.contains("1 configured repo could not"));
    }

    #[tokio::test]
    async fn timed_out_repo_reports_its_cause_and_error_in_scope() {
        use crate::federation::graph_backend::PetgraphBackend;
        use crate::federation::repo_id::RepoId;
        use crate::federation::repo_index::RepoIndex;
        use crate::federation::repo_source::WorkspaceDirSource;
        use std::sync::Arc;
        let tmp = tempfile::tempdir().unwrap();
        let fed = FederatedIndex::new(Arc::new(PetgraphBackend::new(tmp.path()).unwrap()));
        let src_dir = tempfile::tempdir().unwrap();
        git2::Repository::init(src_dir.path()).unwrap();
        let src: Box<dyn crate::federation::repo_source::RepoSource> = Box::new(
            WorkspaceDirSource::new(RepoId::new("big").unwrap(), src_dir.path().to_path_buf())
                .unwrap(),
        );
        fed.add_repo(src, tmp.path()).await.unwrap();
        let repo: Arc<RepoIndex> = fed.get_repo(&RepoId::new("big").unwrap()).unwrap();
        repo.record_index_failure_for_test(
            "index_timeout: indexing exceeded the 60s budget".to_string(),
        );
        let scope = live_scope(&fed);
        let entry = &scope["unreviewed"][0];
        assert_eq!(entry["repo"], "big");
        assert_eq!(entry["reason"], "failed");
        assert_eq!(entry["cause"], "index_timeout");
        assert!(entry["error"].as_str().unwrap().contains("60s budget"));
    }
}
