//! Cross-platform path string formatting.
//!
//! The MCP wire format and the audit log JSONL both store paths as
//! forward-slash strings regardless of host platform, so a Linux
//! agent talking to a Windows `lain` server sees the same path
//! shape a Windows agent does. `posix_string` is the single
//! canonical helper for that conversion.

use std::path::{Component, Path, PathBuf};

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

/// Lexically normalize a path (`.` and `..` components) without
/// touching the filesystem. `/a/../b` becomes `/b`, `./a/b` becomes
/// `a/b`, and `/..` stays `/`. A leading `..` on a relative path is
/// kept — there is nothing to pop it against.
///
/// Lexical on purpose: a claim may name a file the agent is about to
/// *create*, so `fs::canonicalize` would fail on exactly the paths that
/// matter most.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `/..` is `/`; a prefix behaves the same way.
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// One canonical form for a path: symlinks resolved when the
/// file/directory exists, otherwise lexically cleaned. Strips
/// the Windows `\\?\` extended-length prefix that `fs::canonicalize`
/// adds, and normalizes all backslashes to forward slashes. The
/// absolute and relative branches of `canonical_claim_path`
/// both pass through this helper so the symlink-rooted tempdir
/// case on macOS (`/var/folders/.../T/` → `/private/var/folders/.../T/`)
/// and the extended-length-prefix case on Windows
/// (`\\?\C:\Users\X\.tmpABC\...`) collapse to the same string
/// the relative anchor strips to.
pub fn canonical_form(path: &Path) -> PathBuf {
    let resolved = match std::fs::canonicalize(path) {
        Ok(r) => r,
        Err(_) => {
            // Unborn path: walk up to the closest existing ancestor,
            // canonicalize *that* (which resolves any symlinks in
            // the prefix — e.g. macOS's `/var/folders/.../T/` →
            // `/private/var/folders/.../T/`), then re-attach the
            // unborn tail and lexically collapse any `.`/`..` in
            // the result. Pure lexical normalization cannot resolve
            // symlinks, which broke the absolute-vs-relative key
            // invariant on macOS for unborn files: Alice's absolute
            // path stayed unresolved while Bob's relative path was
            // anchored to the canonicalized workspace root, so the
            // two claims landed on different keys and never collided.
            let mut ancestor = path.to_path_buf();
            let mut tail: Vec<std::ffi::OsString> = Vec::new();
            while !ancestor.exists() {
                match ancestor.file_name() {
                    Some(name) => {
                        tail.insert(0, name.to_os_string());
                        if !ancestor.pop() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            let resolved_ancestor =
                std::fs::canonicalize(&ancestor).unwrap_or_else(|_| lexical_normalize(&ancestor));
            let mut combined = resolved_ancestor;
            for c in tail {
                combined.push(c);
            }
            lexical_normalize(&combined)
        }
    };
    // `canonicalize` on Windows prepends the extended-length
    // `\\?\` UNC prefix for paths that don't fit MAX_PATH; strip
    // it so absolute and relative forms end up in the same string
    // form (the test-side `Path::new("C:\\Users\\X\\...")` doesn't
    // have that prefix, so `strip_prefix` would otherwise fail).
    let s = resolved.to_string_lossy();
    let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s);
    PathBuf::from(posix_string(Path::new(stripped)))
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

    #[test]
    fn lexical_normalize_resolves_dot_and_dot_dot() {
        assert_eq!(
            lexical_normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexical_normalize(Path::new("./a/b/c")),
            PathBuf::from("a/b/c")
        );
        assert_eq!(
            lexical_normalize(Path::new("/a/../../b")),
            PathBuf::from("/b")
        );
        assert_eq!(
            lexical_normalize(Path::new("../a/b")),
            PathBuf::from("../a/b")
        );
        assert_eq!(
            lexical_normalize(Path::new("a/./b/./c")),
            PathBuf::from("a/b/c")
        );
    }

    #[test]
    fn canonical_form_handles_unborn_paths_lexically() {
        let nonexistent = Path::new("/nonexistent/path/../destination/file.rs");
        let canon = canonical_form(nonexistent);
        assert_eq!(canon, PathBuf::from("/nonexistent/destination/file.rs"));
    }

    /// Regression for the macOS-only `claim_for_a_file_that_does_not_exist_yet_still_collides`
    /// failure in tests/presence.rs. The tempdir's parent on macOS is
    /// `/var/folders/.../T/`, which is symlinked to
    /// `/private/var/folders/.../T/`. `fs::canonicalize` resolves the
    /// symlink for existing paths but errors on unborn ones; the
    /// previous fallback (`lexical_normalize`) couldn't resolve the
    /// symlink, so Alice's absolute claim key stayed on the
    /// `/var/folders/...` prefix while Bob's relative key (anchored to
    /// the canonicalized workspace root) landed on `/private/var/...`
    /// — the two keys diverged and the unborn-file collision contract
    /// broke.
    ///
    /// This test creates its own symlink so the same scenario is
    /// reproducible on Linux and macOS without relying on the host's
    /// tempdir layout.
    #[cfg(unix)]
    #[test]
    fn canonical_form_resolves_symlink_for_unborn_paths() {
        let tmp = tempfile::tempdir().unwrap();
        // Place the symlink in the same directory as the tempdir so
        // we don't depend on `/tmp` or `/var/folders/...` being writable.
        let parent = tmp.path().parent().unwrap();
        let link = parent.join(format!(
            "{}-unborn-link",
            tmp.path().file_name().unwrap().to_string_lossy()
        ));
        std::os::unix::fs::symlink(tmp.path(), &link).unwrap();

        let unborn_via_link = link.join("src/new.rs");
        let unborn_direct = tmp.path().join("src/new.rs");

        let via_link = canonical_form(&unborn_via_link);
        let direct = canonical_form(&unborn_direct);

        assert_eq!(
            via_link, direct,
            "canonical_form must resolve the symlink so an unborn path accessed through it collides with the same path accessed directly"
        );
    }
}
