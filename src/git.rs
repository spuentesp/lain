//! Git sensor integration using git2
//!
//! Handles file walking, change detection, and uncommitted diff tracking.

use crate::error::LainError;
use git2::{DiffOptions, Repository, StatusOptions};
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Git repository sensor
pub struct GitSensor {
    repo: Repository,
    workspace: PathBuf,
}

impl GitSensor {
    /// Open a Git repository at the given path
    pub fn new(workspace: &Path) -> Result<Self, LainError> {
        let repo = Repository::open(workspace)?;
        
        Ok(Self {
            repo,
            workspace: workspace.to_path_buf(),
        })
    }

    /// Check if this is a valid Git repository with a working HEAD
    pub fn is_valid(&self) -> bool {
        self.repo.head().is_ok()
    }

    /// Get all tracked files in the repository, respecting .gitignore
    pub fn get_all_tracked_files(&self) -> Result<Vec<PathBuf>, LainError> {
        let mut files = Vec::new();

        let index = self.repo.index()?;
        for entry in index.iter() {
            if let Ok(path) = std::str::from_utf8(&entry.path) {
                let full_path = self.workspace.join(path);
                if full_path.is_file() && !self.repo.is_path_ignored(&full_path)? {
                    files.push(full_path);
                }
            }
        }

        info!("Found {} tracked files (gitignore filtered)", files.len());
        Ok(files)
    }

    /// Check if a file is ignored by .gitignore
    pub fn is_ignored(&self, path: &Path) -> Result<bool, LainError> {
        Ok(self.repo.is_path_ignored(path)?)
    }

    /// Get all uncommitted changes (staged and unstaged)
    pub fn get_uncommitted_changes(&self) -> Result<Vec<FileChange>, LainError> {
        let mut changes = Vec::new();
        
        // Get HEAD commit for comparison
        let head = self.repo.head().ok();
        let head_commit = head.as_ref().and_then(|h| h.peel_to_commit().ok());
        
        // Get staged changes
        let mut opts = DiffOptions::new();
        opts.include_untracked(true);
        
        // Compare index to HEAD for staged changes
        if let Some(commit) = head_commit {
            let diff = self.repo.diff_tree_to_index(
                commit.tree().ok().as_ref(),
                None,
                Some(&mut opts),
            )?;
            
            diff.foreach(
                &mut |delta, _| {
                    if let Some(path) = delta.new_file().path() {
                        changes.push(FileChange {
                            path: self.workspace.join(path),
                            change_type: ChangeType::Modified,
                            staged: true,
                        });
                    }
                    true
                },
                None,
                None,
                None,
            )?;
        }
        
        // Get unstaged changes (workdir to index)
        let diff = self.repo.diff_index_to_workdir(None, Some(&mut opts))?;
        
        diff.foreach(
            &mut |delta, _| {
                if let Some(path) = delta.new_file().path() {
                    let full_path = self.workspace.join(path);
                    let staged = changes.iter().any(|c| c.path == full_path);
                    changes.push(FileChange {
                        path: full_path,
                        change_type: if delta.old_file().path().is_none() {
                            ChangeType::Added
                        } else {
                            ChangeType::Modified
                        },
                        staged,
                    });
                }
                true
            },
            None,
            None,
            None,
        )?;
        
        // Get untracked files
        let mut status_opts = StatusOptions::new();
        status_opts.include_untracked(true);
        status_opts.recurse_untracked_dirs(true);
        
        let statuses = self.repo.statuses(Some(&mut status_opts))?;
        
        for entry in statuses.iter() {
            if entry.status().is_wt_new() {
                if let Some(path) = entry.path() {
                    let full_path = self.workspace.join(path);
                    changes.push(FileChange {
                        path: full_path,
                        change_type: ChangeType::Added,
                        staged: false,
                    });
                }
            }
        }
        
        debug!("Found {} uncommitted changes", changes.len());
        Ok(changes)
    }

    /// Get diff content for a specific file
    pub fn get_file_diff(&self, path: &Path) -> Result<String, LainError> {
        let relative = path.strip_prefix(&self.workspace).unwrap_or(path);
        
        let mut opts = DiffOptions::new();
        opts.pathspec(relative);
        
        let diff = self.repo.diff_index_to_workdir(None, Some(&mut opts))?;
        
        let mut diff_text = String::new();
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' => "+",
                '-' => "-",
                ' ' => " ",
                _ => "",
            };
            diff_text.push_str(prefix);
            if let Ok(content) = std::str::from_utf8(line.content()) {
                diff_text.push_str(content);
            }
            true
        })?;
        
        Ok(diff_text)
    }

    /// Get the current branch name
    pub fn get_current_branch(&self) -> Result<String, LainError> {
        let head = self.repo.head()?;
        let branch = head.shorthand().unwrap_or("unknown");
        Ok(branch.to_string())
    }

    /// Get the latest commit hash
    pub fn get_latest_commit(&self) -> Result<String, LainError> {
        let head = self.repo.head()?;
        let commit = head.peel_to_commit()?;
        Ok(commit.id().to_string())
    }

    /// Get latest commit hash and its timestamp
    pub fn get_latest_commit_info(&self) -> Result<(String, i64), LainError> {
        let head = self.repo.head()?;
        let commit = head.peel_to_commit()?;
        Ok((commit.id().to_string(), commit.time().seconds()))
    }

    /// Get commit history for co-change analysis
    /// Returns a list of commits with their associated files
    pub fn get_commit_history(&self, count: usize) -> Result<Vec<CommitInfo>, LainError> {
        let mut commits = Vec::new();

        let mut revwalk = self.repo.revwalk()?;
        revwalk.push_head()?;

        let mut commit_count = 0;
        for oid in revwalk.flatten() {
            if commit_count >= count {
                break;
            }
            commit_count += 1;
            
            let commit = self.repo.find_commit(oid)?;
            let message = commit.message().unwrap_or("").to_string();
            
            // Get the parent commit tree to find changed files
            let tree = commit.tree()?;
            let parent_tree = if commit.parent_count() > 0 {
                Some(commit.parent(0)?.tree()?)
            } else {
                None
            };
            
            // Diff to find changed files
            let diff = self.repo.diff_tree_to_tree(
                parent_tree.as_ref(),
                Some(&tree),
                None,
            )?;
            
            let mut files = Vec::new();
            diff.foreach(
                &mut |delta, _| {
                    if let Some(path) = delta.new_file().path() {
                        files.push(path.to_string_lossy().to_string());
                    } else if let Some(path) = delta.old_file().path() {
                        files.push(path.to_string_lossy().to_string());
                    }
                    true
                },
                None,
                None,
                None,
            )?;
            
            commits.push(CommitInfo {
                id: commit.id().to_string(),
                message: message.split('\n').next().unwrap_or("").to_string(),
                files,
                time: commit.time().seconds(),
            });
        }
        
        debug!("Retrieved {} commits for co-change analysis", commits.len());
        Ok(commits)
    }

    /// Analyze co-changes from commit history
    /// Returns pairs of files that frequently change together
    pub fn analyze_co_changes(&self, count: usize, threshold: usize, max_files: usize) -> Result<Vec<CoChangePair>, LainError> {
        let commits = self.get_commit_history(count)?;

        use std::collections::HashMap;
        let mut pair_counts: HashMap<(String, String), usize> = HashMap::new();

        for commit in &commits {
            // Optimization: Skip commits that touch too many files
            // to avoid O(N^2) complexity explosions in pair generation.
            if commit.files.len() > max_files {
                debug!("Skipping commit {} for co-change: {} files exceeds max {}", commit.id, commit.files.len(), max_files);
                continue;
            }

            // Sort files to ensure consistent pair ordering
            let mut files = commit.files.clone();
            files.sort();

            // Generate all pairs
            for i in 0..files.len() {
                for j in (i + 1)..files.len() {
                    let pair = (files[i].clone(), files[j].clone());
                    *pair_counts.entry(pair).or_insert(0) += 1;
                }
            }
        }

        // Filter by threshold and convert to sorted pairs
        let mut co_changes: Vec<CoChangePair> = pair_counts
            .into_iter()
            .filter(|(_, count)| *count >= threshold)
            .map(|((file1, file2), count)| CoChangePair {
                file1,
                file2,
                co_change_count: count,
            })
            .collect();

        // Sort by co-change count descending
        co_changes.sort_by_key(|b| std::cmp::Reverse(b.co_change_count));

        debug!("Found {} co-change pairs above threshold {}", co_changes.len(), threshold);
        Ok(co_changes)
    }

    /// Get commits newer than the given commit hash
    /// Returns commits after (not including) the specified hash
    pub fn get_new_commits_since(&self, since_hash: &str) -> Result<Vec<CommitInfo>, LainError> {
        let mut commits = Vec::new();

        let mut revwalk = self.repo.revwalk()?;
        revwalk.push_head()?;

        let mut found_start = false;
        for oid in revwalk.flatten() {
            let oid_str = oid.to_string();

            // Skip until we find the starting hash
            if !found_start {
                if oid_str == since_hash {
                    found_start = true;
                }
                continue;
            }

            let commit = self.repo.find_commit(oid)?;
            let message = commit.message().unwrap_or("").to_string();

            let tree = commit.tree()?;
            let parent_tree = if commit.parent_count() > 0 {
                Some(commit.parent(0)?.tree()?)
            } else {
                None
            };

            let diff = self.repo.diff_tree_to_tree(
                parent_tree.as_ref(),
                Some(&tree),
                None,
            )?;

            let mut files = Vec::new();
            diff.foreach(
                &mut |delta, _| {
                    if let Some(path) = delta.new_file().path() {
                        files.push(path.to_string_lossy().to_string());
                    } else if let Some(path) = delta.old_file().path() {
                        files.push(path.to_string_lossy().to_string());
                    }
                    true
                },
                None,
                None,
                None,
            )?;

            commits.push(CommitInfo {
                id: oid_str,
                message: message.split('\n').next().unwrap_or("").to_string(),
                files,
                time: commit.time().seconds(),
            });
        }

        debug!("Found {} new commits since {}", commits.len(), since_hash);
        Ok(commits)
    }

    /// Get all files that were changed since a specific commit hash
    pub fn get_changed_files_since(&self, since_hash: &str) -> Result<Vec<PathBuf>, LainError> {
        let commits = self.get_new_commits_since(since_hash)?;
        let mut files = std::collections::HashSet::new();
        for commit in commits {
            for file in commit.files {
                let full_path = self.workspace.join(&file);
                if !self.repo.is_path_ignored(&full_path)? {
                    files.insert(full_path);
                }
            }
        }
        Ok(files.into_iter().collect())
    }
}

/// Type of change detected
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
}

/// A file with its change information
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub staged: bool,
}

/// Information about a commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub id: String,
    pub message: String,
    pub files: Vec<String>,
    pub time: i64,
}

/// A pair of files that were changed together
#[derive(Debug, Clone)]
pub struct CoChangePair {
    pub file1: String,
    pub file2: String,
    pub co_change_count: usize,
}
