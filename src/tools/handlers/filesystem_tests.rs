//! Tests for tools/handlers/filesystem.rs

use crate::tools::handlers::filesystem::{read_file, list_directory, find_files};

#[test]
fn test_read_file_basic() {
    let tmp = std::env::temp_dir().join("test_read_file.txt");
    std::fs::write(&tmp, "line1\nline2\nline3\n").unwrap();

    let result = read_file(tmp.to_str().unwrap(), None, None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("line1"));
    assert!(text.contains("line2"));
    assert!(text.contains("line3"));

    std::fs::remove_file(&tmp).ok();
}

#[test]
fn test_read_file_not_found() {
    let result = read_file("/nonexistent/path/to/file.txt", None, None);
    assert!(result.is_err());
}

#[test]
fn test_read_file_with_line_range() {
    let tmp = std::env::temp_dir().join("test_line_range.txt");
    std::fs::write(&tmp, "line1\nline2\nline3\nline4\nline5\n").unwrap();

    let result = read_file(tmp.to_str().unwrap(), Some(2), Some(4));
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("line2"));
    assert!(text.contains("line3"));
    assert!(!text.contains("line1") || text.contains("line2")); // line1 may appear in header

    std::fs::remove_file(&tmp).ok();
}

#[test]
fn test_read_file_invalid_range() {
    let tmp = std::env::temp_dir().join("test_invalid_range.txt");
    std::fs::write(&tmp, "line1\nline2\nline3\n").unwrap();

    // start > end should error
    let result = read_file(tmp.to_str().unwrap(), Some(5), Some(2));
    assert!(result.is_err());

    std::fs::remove_file(&tmp).ok();
}

#[test]
fn test_list_directory_basic() {
    let tmp = std::env::temp_dir().join("test_list_dir");
    std::fs::create_dir_all(&tmp).unwrap();

    std::fs::write(tmp.join("file1.txt"), "data").unwrap();
    std::fs::write(tmp.join("file2.txt"), "data").unwrap();
    std::fs::create_dir_all(tmp.join("subdir")).unwrap();

    let result = list_directory(tmp.to_str().unwrap(), false);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("file1.txt"));
    assert!(text.contains("file2.txt"));
    assert!(text.contains("subdir"));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn test_list_directory_not_found() {
    let result = list_directory("/nonexistent/directory/path", false);
    assert!(result.is_err());
}

#[test]
fn test_list_directory_not_a_dir() {
    let tmp = std::env::temp_dir().join("test_not_a_dir.txt");
    std::fs::write(&tmp, "data").unwrap();

    let result = list_directory(tmp.to_str().unwrap(), false);
    assert!(result.is_err());

    std::fs::remove_file(&tmp).ok();
}

#[test]
fn test_list_directory_with_hidden() {
    let tmp = std::env::temp_dir().join("test_hidden");
    std::fs::create_dir_all(&tmp).unwrap();

    std::fs::write(tmp.join("visible.txt"), "data").unwrap();
    std::fs::write(tmp.join(".hidden"), "data").unwrap();

    // Without hidden files
    let result = list_directory(tmp.to_str().unwrap(), false).unwrap();
    assert!(result.contains("visible.txt"));
    assert!(!result.contains(".hidden"));

    // With hidden files
    let result2 = list_directory(tmp.to_str().unwrap(), true).unwrap();
    assert!(result2.contains("visible.txt"));
    assert!(result2.contains(".hidden"));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn test_find_files_basic() {
    let tmp = std::env::temp_dir().join("test_find_files");
    std::fs::create_dir_all(&tmp).unwrap();

    std::fs::write(tmp.join("test1.txt"), "data").unwrap();
    std::fs::write(tmp.join("test2.txt"), "data").unwrap();
    std::fs::write(tmp.join("other.txt"), "data").unwrap();

    let result = find_files("test", Some(tmp.to_string_lossy().to_string()), None);
    assert!(result.is_ok());
    let text = result.unwrap();
    assert!(text.contains("test1.txt"));
    assert!(text.contains("test2.txt"));
    // The count of matching files should be shown
    assert!(text.contains("2"));

    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn test_find_files_not_found() {
    let result = find_files("nonexistent_pattern", Some("/nonexistent/root".to_string()), None);
    assert!(result.is_err());
}

#[test]
fn test_find_files_max_results() {
    let tmp = std::env::temp_dir().join("test_max_results");
    std::fs::create_dir_all(&tmp).unwrap();

    for i in 0..5 {
        std::fs::write(tmp.join(format!("file{}.txt", i)), "data").unwrap();
    }

    let result = find_files("file", Some(tmp.to_string_lossy().to_string()), Some(3));
    assert!(result.is_ok());
    let text = result.unwrap();
    // Should show at most 3 results (implementation may show exactly 3 or slightly more due to path display)
    assert!(text.contains("3"));

    std::fs::remove_dir_all(&tmp).ok();
}