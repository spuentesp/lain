//! GitOps domain handlers - git operations for agent workflow

use crate::error::LainError;
use crate::git::{GitSensor, ChangeType};
use std::sync::Arc;
use parking_lot::Mutex;

pub fn get_file_diff(
    git: &Arc<Mutex<GitSensor>>,
    path_filter: Option<&str>,
) -> Result<String, LainError> {
    let git_guard = git.lock();
    let changes = git_guard.get_uncommitted_changes()?;

    if changes.is_empty() {
        return Ok("No uncommitted changes.".to_string());
    }

    let mut result = String::from("## Uncommitted Changes\n\n");

    let filtered: Vec<_> = if let Some(p) = path_filter {
        changes.iter().filter(|c| c.path.to_string_lossy().contains(p)).collect()
    } else {
        changes.iter().collect()
    };

    for change in &filtered {
        let status = match change.change_type {
            ChangeType::Added => "✨ Added",
            ChangeType::Modified => "✏️ Modified",
            ChangeType::Deleted => "🗑️ Deleted",
        };
        result.push_str(&format!("{} `{}`\n", status, change.path.display()));
    }

    result.push_str(&format!("\n{} file(s) changed\n", filtered.len()));
    Ok(result)
}

pub fn get_commit_history(
    git: &Arc<Mutex<GitSensor>>,
    limit: Option<usize>,
) -> Result<String, LainError> {
    let git_guard = git.lock();
    let commits = git_guard.get_commit_history(limit.unwrap_or(20))?;

    if commits.is_empty() {
        return Ok("No commit history found.".to_string());
    }

    let mut result = String::from("## Commit History\n\n");
    for commit in commits {
        // Format the timestamp (time is i64 - Unix timestamp)
        let time_str = if commit.time > 0 {
            let duration = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let diff = duration - commit.time;
            if diff < 60 {
                format!("{}s ago", diff)
            } else if diff < 3600 {
                format!("{}m ago", diff / 60)
            } else if diff < 86400 {
                format!("{}h ago", diff / 3600)
            } else {
                format!("{}d ago", diff / 86400)
            }
        } else {
            "unknown".to_string()
        };

        let first_line = commit.message.lines().next().unwrap_or("(no message)").trim();
        result.push_str(&format!(
            "**{}** ({} ago)\n  {}\n\n",
            &commit.id[..7.min(commit.id.len())],
            time_str,
            first_line
        ));
    }

    Ok(result)
}

pub fn get_branch_status(
    git: &Arc<Mutex<GitSensor>>,
) -> Result<String, LainError> {
    let git_guard = git.lock();
    let branch = git_guard.get_current_branch()?;
    let is_valid = git_guard.is_valid();

    let mut status = String::from("## Git Branch Status\n\n");
    status.push_str(&format!("**Branch:** `{}`\n", branch));
    status.push_str(&format!("**Status:**{}\n", if is_valid { " ✅ Clean" } else { " ⚠️ Not a git repo" }));

    Ok(status)
}