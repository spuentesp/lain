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

pub fn sync_state(
    graph: &GraphDatabase,
    git: &Arc<Mutex<GitSensor>>,
    ingestion: &IngestionConfig,
) -> Result<String, LainError> {
    let last_commit = graph.get_last_commit()?;
    let latest_commit = git.lock().get_latest_commit().unwrap_or_default();

    if last_commit.as_ref() == Some(&latest_commit) {
        return Ok("No new commits. State is already up to date.".to_string());
    }

    let graph_clone = graph.clone();
    let git_clone = Arc::clone(git);
    // Copy fields so they can be moved into async block
    let cochange_max_commit_files = ingestion.cochange_max_commit_files;

    tokio::spawn(async move {
        tracing::info!("Starting background sync job");
        let start_time = std::time::Instant::now();

        let last_commit = match graph_clone.get_last_commit() {
            Ok(lc) => lc,
            Err(e) => {
                tracing::error!("Sync failed to get last commit: {}", e);
                return;
            }
        };

        let git_guard = git_clone.lock();
        let new_commits: Vec<CommitInfo> = if let Some(ref last) = last_commit {
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
        let latest_commit = git_guard.get_latest_commit().unwrap_or_default();
        drop(git_guard);

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

        tracing::info!("Background sync job completed in {:?}. Synced {} commits.", start_time.elapsed(), new_commits.len());
    });

    Ok("State sync started in background. Check 'get_health' later for status.".to_string())
}
