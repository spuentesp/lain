//! GitOps domain handlers - git operations for agent workflow

use crate::error::LainError;
use crate::git::{AnyGitSensor, ChangeType};
use std::sync::Arc;

pub fn get_file_diff(
    git: &Arc<AnyGitSensor>,
    path_filter: Option<&str>,
) -> Result<String, LainError> {
    let changes = git.get_uncommitted_changes()?;

    if changes.is_empty() {
        return Ok("No uncommitted changes.".to_string());
    }

    let mut result = String::from("## Uncommitted Changes\n\n");

    let filtered: Vec<_> = if let Some(p) = path_filter {
        changes
            .iter()
            // A repo-relative file or directory, not a substring of the
            // absolute path (`a` matched every file under `/home/a…`).
            .filter(|c| {
                let full = c.path.to_string_lossy().replace('\\', "/");
                let want = p.trim_start_matches("./").trim_end_matches('/');
                full == want
                    || full.ends_with(&format!("/{want}"))
                    || full.contains(&format!("/{want}/"))
            })
            .collect()
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
    git: &Arc<AnyGitSensor>,
    limit: Option<usize>,
) -> Result<String, LainError> {
    let commits = git.get_commit_history(limit.unwrap_or(20))?;

    if commits.is_empty() {
        return Ok("No commit history found.".to_string());
    }

    let mut result = String::from("## Commit History\n\n");
    for commit in commits {
        // Format the timestamp (time is i64 - Unix timestamp)
        let time_str = crate::server::tools::utils::format_ago(commit.time);

        let first_line = commit
            .message
            .lines()
            .next()
            .unwrap_or("(no message)")
            .trim();
        result.push_str(&format!(
            // `format_ago` already ends in "ago".
            "**{}** ({})\n  {}\n\n",
            &commit.id[..7.min(commit.id.len())],
            time_str,
            first_line
        ));
    }

    Ok(result)
}

pub fn get_branch_status(git: &Arc<AnyGitSensor>) -> Result<String, LainError> {
    let branch = git.get_current_branch()?;
    let is_valid = git.is_valid();

    let mut status = String::from("## Git Branch Status\n\n");
    status.push_str(&format!("**Branch:** `{}`\n", branch));
    if !is_valid {
        status.push_str("**Status:** ⚠️ Not a git repo\n");
        return Ok(status);
    }
    // "Clean" used to mean only "this is a git repository".
    let changes = git.get_uncommitted_changes()?;
    if changes.is_empty() {
        status.push_str("**Status:** ✅ Clean\n");
    } else {
        let count =
            |t: fn(&ChangeType) -> bool| changes.iter().filter(|c| t(&c.change_type)).count();
        let staged = changes.iter().filter(|c| c.staged).count();
        status.push_str(&format!(
            "**Status:** ✏️ {} uncommitted change(s): {} modified, {} added, {} deleted ({} staged)\n",
            changes.len(),
            count(|t| matches!(t, ChangeType::Modified)),
            count(|t| matches!(t, ChangeType::Added)),
            count(|t| matches!(t, ChangeType::Deleted)),
            staged
        ));
    }

    Ok(status)
}
