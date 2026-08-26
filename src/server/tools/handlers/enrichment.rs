//! Enrichment and sync domain handlers

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::git::{GitSensor, CommitInfo};
use crate::tuning::IngestionConfig;
use std::sync::Arc;
use parking_lot::Mutex;

pub fn run_enrichment(
    graph: &GraphDatabase,
    git: &Arc<Mutex<GitSensor>>,
    ingestion: &IngestionConfig,
) -> Result<String, LainError> {
    let graph_clone = graph.clone();
    let git_clone = Arc::clone(git);
    // Copy fields so they can be moved into async block
    let cochange_commit_window = ingestion.cochange_commit_window;
    let cochange_min_pair_count = ingestion.cochange_min_pair_count;
    let cochange_max_commit_files = ingestion.cochange_max_commit_files;

    tokio::spawn(async move {
        tracing::info!("Starting background enrichment job");
        let start_time = std::time::Instant::now();

        // 1. Analyze git history for co-change pairs
        let (co_change_pairs, latest_commit) = {
            let git_guard = git_clone.lock();
            let pairs = match git_guard.analyze_co_changes(
                cochange_commit_window,
                cochange_min_pair_count,
                cochange_max_commit_files,
            ) {
                Ok(pairs) => pairs,
                Err(e) => {
                    tracing::warn!("Co-change analysis failed: {}, skipping", e);
                    Vec::new()
                }
            };
            let commit = git_guard.get_latest_commit().unwrap_or_default();
            (pairs, commit)
        };

        // 2. Insert co-change edges into the graph
        if !co_change_pairs.is_empty() {
            let pair_tuples: Vec<_> = co_change_pairs
                .iter()
                .map(|p| {
                    let file1 = p.file1.clone();
                    let file2 = p.file2.clone();
                    (file1, file2, p.co_change_count)
                })
                .collect();
            if let Err(e) = graph_clone.insert_co_change_edges(&pair_tuples) {
                tracing::error!("Failed to insert co-change edges: {}", e);
            }
        }

        // 3. Calculate anchor scores
        if let Err(e) = graph_clone.calculate_anchor_scores() {
            tracing::error!("Failed to calculate anchor scores: {}", e);
        }

        // 4. Calculate depth-from-main
        if let Err(e) = graph_clone.calculate_depths() {
            tracing::error!("Failed to calculate depths: {}", e);
        }

        // 5. Store latest commit for incremental updates
        if !latest_commit.is_empty() {
            if let Err(e) = graph_clone.set_last_commit(latest_commit) {
                tracing::error!("Failed to set last commit: {}", e);
            }
        }

        tracing::info!("Background enrichment job completed in {:?}", start_time.elapsed());
    });

    Ok("Enrichment job started in background. Check 'get_health' later for status.".to_string())
}

/// Kick off a background re-sync of the graph with git HEAD.
///
/// Returns a `job_id`. It used to return only "State sync started in
/// background. Check 'get_health' later for status." — and then every
/// failure inside the spawned task went to `tracing::error!`, which no
/// MCP client surfaces. An agent watching `get_health` saw the enriched
/// commit never move, with nothing anywhere saying why. The job record
/// is the answer to "did my sync work?", and a failure also degrades
/// the health banner.
pub fn sync_state(
    graph: &GraphDatabase,
    git: &Arc<Mutex<GitSensor>>,
    ingestion: &IngestionConfig,
    jobs: &Arc<tokio::sync::Mutex<std::collections::HashMap<String, crate::server::tools::JobInfo>>>,
    last_outcome: &Arc<Mutex<crate::server::refresh::RefreshOutcome>>,
    fed: Option<&Arc<crate::federation::federated_index::FederatedIndex>>,
) -> Result<String, LainError> {
    let last_commit = graph.get_last_commit()?;
    let latest_commit = git.lock().get_latest_commit().unwrap_or_default();
    // Whether HEAD has moved does NOT gate the overlay-refresh phase —
    // a brand-new untracked file with no new commit must still become
    // visible. We log the no-op commit case so callers reading the
    // logs can see why a sync ran without touching co-change edges.
    let no_new_commits = last_commit.as_ref() == Some(&latest_commit);
    if no_new_commits {
        tracing::info!("sync_state: no new commits, but overlay refresh will still run");
    }

    let graph_clone = graph.clone();
    let git_clone = Arc::clone(git);
    // Copy fields so they can be moved into async block
    let cochange_max_commit_files = ingestion.cochange_max_commit_files;
    // Clone the Arc so the spawned task owns a `'static` handle.
    // `Option<&Arc<...>>` lets the caller pass `ctx.federation.as_ref()`
    // without forcing an extra clone at every call site.
    let fed_handle = fed.map(Arc::clone);

    let job_id = uuid::Uuid::new_v4().to_string();
    let jobs_registry = Arc::clone(jobs);
    let outcome_slot = Arc::clone(last_outcome);
    {
        let mut guard = jobs_registry.blocking_lock();
        guard.insert(
            job_id.clone(),
            crate::server::tools::JobInfo {
                id: job_id.clone(),
                created_at: std::time::SystemTime::now(),
                state: crate::server::tools::JobState::Running,
            },
        );
    }
    let job_id_for_task = job_id.clone();

    tokio::spawn(async move {
        tracing::info!("Starting background sync job");
        // Every early return below must land in the job record, so the
        // caller's `get_job_status` can distinguish "still running"
        // from "failed two minutes ago".
        let finish = |registry: Arc<tokio::sync::Mutex<std::collections::HashMap<String, crate::server::tools::JobInfo>>>,
                      id: String,
                      result: Result<String, String>| async move {
            let mut guard = registry.lock().await;
            if let Some(j) = guard.get_mut(&id) {
                j.state = match result {
                    Ok(out) => crate::server::tools::JobState::Completed {
                        success: true,
                        output: Some(out),
                        error: None,
                    },
                    Err(e) => crate::server::tools::JobState::Completed {
                        success: false,
                        output: None,
                        error: Some(e),
                    },
                };
            }
        };
        let start_time = std::time::Instant::now();

        let last_commit = match graph_clone.get_last_commit() {
            Ok(lc) => lc,
            Err(e) => {
                tracing::error!("Sync failed to get last commit: {}", e);
                {
                    let mut slot = outcome_slot.lock();
                    *slot = crate::server::refresh::RefreshOutcome::failed(
                        std::time::SystemTime::now(),
                        format!("sync_state: {e}"),
                    );
                }
                finish(jobs_registry, job_id_for_task, Err(format!("failed to get last commit: {e}"))).await;
                return;
            }
        };

        // Scoped: a `parking_lot` guard live across an `.await` makes
        // the spawned future non-`Send`.
        let (new_commits, latest_commit): (Vec<CommitInfo>, String) = {
            let git_guard = git_clone.lock();
            let new_commits = if let Some(ref last) = last_commit {
                match git_guard.get_new_commits_since(last) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Failed to get new commits: {}, doing full refresh", e);
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let latest = git_guard.get_latest_commit().unwrap_or_default();
            (new_commits, latest)
        };

        // Analyze co-changes from new commits only
        let mut new_pairs: std::collections::HashMap<(String, String), usize> = std::collections::HashMap::new();
        for commit in &new_commits {
            // Skip mega-commits to avoid O(N^2) pair explosion
            if commit.files.len() > cochange_max_commit_files {
                tracing::debug!("Skipping mega-commit {} ({} files) in sync co-change", commit.id, commit.files.len());
                continue;
            }
            let mut files = commit.files.clone();
            files.sort();
            for i in 0..files.len() {
                for j in (i + 1)..files.len() {
                    let pair = (files[i].clone(), files[j].clone());
                    *new_pairs.entry(pair).or_insert(0) += 1;
                }
            }
        }

        let pair_tuples: Vec<_> = new_pairs
            .into_iter()
            .map(|((f1, f2), c)| (f1, f2, c))
            .collect();

        if !pair_tuples.is_empty() {
            if let Err(e) = graph_clone.insert_co_change_edges(&pair_tuples) {
                tracing::error!("Sync failed to insert edges: {}", e);
            }
        }

        if let Err(e) = graph_clone.calculate_anchor_scores() {
            tracing::error!("Sync failed to calculate anchors: {}", e);
        }
        if let Err(e) = graph_clone.calculate_depths() {
            tracing::error!("Sync failed to calculate depths: {}", e);
        }

        if !latest_commit.is_empty() {
            if let Err(e) = graph_clone.set_last_commit(latest_commit) {
                tracing::error!("Sync failed to set last commit: {}", e);
            }
        }

        // Phase 2: refresh volatile overlays for every repo in the
        // federation. Runs regardless of whether there were new
        // commits, so a brand-new untracked file becomes visible
        // immediately after `sync_state` instead of waiting on the
        // next watcher tick (which, on a freshly-created workspace,
        // may be hours away).
        let mut overlay_refresh_count = 0usize;
        if let Some(fed_ref) = fed_handle.as_ref() {
            for (id, _) in fed_ref.list_repos() {
                let Some(repo) = fed_ref.get_repo(&id) else {
                    continue;
                };
                match repo.sync_overlay().await {
                    Ok(()) => overlay_refresh_count += 1,
                    Err(e) => {
                        tracing::warn!(
                            "[sync_state] overlay refresh for {} failed: {}",
                            id,
                            e
                        );
                        *outcome_slot.lock() = crate::server::refresh::RefreshOutcome::failed(
                            std::time::SystemTime::now(),
                            format!("sync_state overlay refresh for {}: {}", id, e),
                        );
                    }
                }
            }
        }

        let summary = format!(
            "enrichment: {} new commits; overlay: refreshed ({} repos) in {:?}",
            new_commits.len(),
            overlay_refresh_count,
            start_time.elapsed()
        );
        tracing::info!("Background sync job completed: {summary}");
        finish(jobs_registry, job_id_for_task, Ok(summary)).await;
    });

    Ok(format!(
        "State sync started in background as job {job_id}. Poll `get_job_status` with that \
         job_id for the outcome; failures also show up in `get_health`."
    ))
}
