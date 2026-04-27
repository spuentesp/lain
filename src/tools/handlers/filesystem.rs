//! Filesystem domain handlers

use crate::error::LainError;
use std::path::Path;
use walkdir::WalkDir;

pub fn read_file(path: &str, line_start: Option<u32>, line_end: Option<u32>) -> Result<String, LainError> {
    let path = Path::new(path);
    if !path.exists() {
        return Err(LainError::NotFound(format!("File not found: {}", path.display())));
    }
    let content = std::fs::read_to_string(path).map_err(LainError::Io)?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(format!("File: {}\n\n(empty)", path.display()));
    }
    let start = line_start.unwrap_or(1) as usize;
    let end = line_end.unwrap_or(lines.len() as u32) as usize;
    let start = start.saturating_sub(1).min(lines.len());
    let end = end.min(lines.len());
    if start >= end {
        return Err(LainError::NotFound(format!("Invalid line range: {} to {}", start + 1, end)));
    }
    let selected: Vec<String> = lines[start..end].iter().enumerate()
        .map(|(i, line)| format!("{:4}: {}", start + i + 1, line))
        .collect();
    Ok(format!("File: {}\nShowing lines {}-{} of {}\n\n{}\n",
        path.display(), start + 1, end, lines.len(), selected.join("\n")))
}

pub fn list_directory(path: &str, include_hidden: bool) -> Result<String, LainError> {
    let dir_path = Path::new(path);
    if !dir_path.exists() {
        return Err(LainError::NotFound(format!("Directory not found: {}", dir_path.display())));
    }
    if !dir_path.is_dir() {
        return Err(LainError::NotFound(format!("Not a directory: {}", dir_path.display())));
    }
    let mut entries: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(dir_path).map_err(LainError::Io)? {
        let entry = entry.map_err(LainError::Io)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !include_hidden && name.starts_with('.') { continue; }
        let ft = if entry.file_type().map_err(LainError::Io)?.is_dir() {
            "📁"
        } else { "📄" };
        entries.push(format!("{} {}", ft, name));
    }
    entries.sort();
    Ok(format!("Directory: {}\n{} entries\n\n{}\n", dir_path.display(), entries.len(), entries.join("\n")))
}

pub fn find_files(pattern: &str, root: Option<String>, max_results: Option<usize>) -> Result<String, LainError> {
    let root_path = root.as_ref().map(Path::new).unwrap_or(Path::new("."));
    let max = max_results.unwrap_or(100);
    if !root_path.exists() {
        return Err(LainError::NotFound(format!("Root not found: {}", root_path.display())));
    }
    let glob_pattern = if pattern.contains('*') { pattern.to_string() } else { format!("**/*{}*", pattern) };
    let mut matches: Vec<String> = Vec::new();
    for entry in WalkDir::new(root_path).follow_links(true).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let relative = path.strip_prefix(root_path).unwrap_or(path);
        if glob_match(&glob_pattern, &relative.to_string_lossy()) {
            matches.push(path.to_string_lossy().to_string());
            if matches.len() >= max { break; }
        }
    }
    matches.sort();
    Ok(format!("Found {} files matching '{}'\n\n{}\n", matches.len(), pattern, matches.join("\n")))
}

fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();
    let mut pi = 0;
    for ppart in &pattern_parts {
        if *ppart == "**" {
            if pi >= path_parts.len() { return false; }
            let remaining: Vec<&str> = pattern_parts.iter().skip(pi + 1).cloned().collect();
            return remaining_pattern_matches(&remaining, &path_parts[pi..]);
        }
        if pi >= path_parts.len() {
            return false;
        }
        if *ppart != path_parts[pi] && !ppart.contains('*') {
            return false;
        }
        pi += 1;
    }
    pi == path_parts.len()
}

fn remaining_pattern_matches(pattern_parts: &[&str], path_parts: &[&str]) -> bool {
    if pattern_parts.is_empty() { return true; }
    if path_parts.is_empty() { return pattern_parts.iter().all(|p| *p == "**"); }
    for i in 0..=path_parts.len() {
        let remaining = &path_parts[i..];
        let mut pi = 0;
        let mut path_idx = 0;
        while pi < pattern_parts.len() && path_idx < remaining.len() {
            let ppart = pattern_parts[pi];
            if ppart == "**" { pi += 1; if pi >= pattern_parts.len() { return true; } path_idx += 1; }
            else {
                if ppart != remaining[path_idx] && !ppart.contains('*') { break; }
                pi += 1; path_idx += 1;
            }
        }
        if pi >= pattern_parts.len() && path_idx >= remaining.len() { return true; }
    }
    false
}
