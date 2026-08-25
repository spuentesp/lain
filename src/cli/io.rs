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

/// Async counterpart to `write_file_atomic` for callers running
/// inside a Tokio runtime. Currently consumed by
/// `server::graph::save_to_disk`.
pub async fn tokio_write_file_atomic(
    path: &Path,
    bytes: impl AsRef<[u8]>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
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

    #[tokio::test]
    async fn tokio_write_file_atomic_writes_and_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("graph.bin");
        tokio_write_file_atomic(&path, b"\x01\x02\x03").await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\x01\x02\x03");
        assert!(!path.with_extension("tmp").exists());
    }

    #[tokio::test]
    async fn tokio_write_file_atomic_creates_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a/b/c/state.bin");
        tokio_write_file_atomic(&path, b"x").await.unwrap();
        assert!(path.exists());
    }
}
