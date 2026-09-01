//! Cross-platform path string formatting.
//!
//! The MCP wire format and the audit log JSONL both store paths as
//! forward-slash strings regardless of host platform, so a Linux
//! agent talking to a Windows `lain` server sees the same path
//! shape a Windows agent does. `posix_string` is the single
//! canonical helper for that conversion.

use std::path::Path;

/// Render `path` as a forward-slash string, the form every wire
/// protocol and on-disk log in this crate expects.
///
/// On Unix this is a no-op — `to_string_lossy` already produces
/// `/`-separated strings. On Windows it rewrites `\` to `/` so the
/// output matches what a Linux consumer would have written.
///
/// This is the same shape `crate::server::graph::graph_path` uses
/// for index-map keys; the two helpers differ only in that
/// `graph_path` strips a workspace prefix first. Do not duplicate
/// the platform branch anywhere else in the crate — call this.
pub fn posix_string(path: &Path) -> String {
    let s = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_string_is_no_op_on_native_forward_slash_paths() {
        // Runs on every platform: confirms forward-slash input is
        // preserved verbatim.
        assert_eq!(posix_string(Path::new("src/a.rs")), "src/a.rs");
        assert_eq!(posix_string(Path::new("a/b/c.rs")), "a/b/c.rs");
        assert_eq!(posix_string(Path::new("")), "");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn posix_string_normalizes_windows_separators() {
        // Windows-only: confirm the `\` → `/` rewrite happens.
        assert_eq!(posix_string(Path::new("src\\a.rs")), "src/a.rs");
        assert_eq!(posix_string(Path::new("a\\b\\c.rs")), "a/b/c.rs");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn posix_string_does_not_touch_unix_paths_with_literal_backslash() {
        // Unix-only guard: `Path::new("src\\a.rs")` is a single
        // component with a backslash in the filename, not a
        // separator. The helper must not rewrite it on Unix.
        assert_eq!(posix_string(Path::new("src\\a.rs")), "src\\a.rs");
    }
}
