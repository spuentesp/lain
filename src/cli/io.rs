use std::fs;
use std::io;
use std::path::Path;

/// Write `bytes` to `path` atomically by writing to a sibling temp
/// file first and renaming it over `path`. Creates the parent
/// directory if it doesn't exist. The temp file uses
/// `path.with_extension("tmp")` for compatibility with every
/// existing caller (the prior `repos.rs::write_atomic` used
/// `.{name}.tmp`; the canonical helper uses `with_extension` to
/// match `state.rs` and `presence.rs`, and the rename semantics
/// are equivalent).
pub fn write_file_atomic(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_file_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("data.json");
        write_file_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn creates_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("deeper").join("f.txt");
        write_file_atomic(&path, b"x").unwrap();
        assert!(path.exists());
    }

    #[test]
    fn overwrites_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        write_file_atomic(&path, b"first").unwrap();
        write_file_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
    }

    #[test]
    fn no_tmp_file_left_behind_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("f.txt");
        write_file_atomic(&path, b"ok").unwrap();
        // The .tmp sibling should be gone (rename moved it).
        assert!(!path.with_extension("tmp").exists());
    }
}