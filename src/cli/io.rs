use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Unique sibling temp file for an atomic write. The name carries the
/// pid and a per-process counter: two concurrent writers of the same
/// target used to share `path.with_extension("tmp")` and tear each
/// other's file apart on the rename.
fn tmp_sibling(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(
        ".tmp.{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

/// Write `bytes` to `path` atomically by writing to a uniquely named
/// sibling temp file first and renaming it over `path`. Creates the
/// parent directory if it doesn't exist. The rename is atomic, so a
/// reader never sees a torn file and concurrent writers cannot clobber
/// each other's temp file.
pub fn write_file_atomic(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = tmp_sibling(path);
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Async counterpart to `write_file_atomic` for callers running
/// inside a Tokio runtime. Currently consumed by
/// `server::graph::save_to_disk`.
pub async fn tokio_write_file_atomic(path: &Path, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let tmp = tmp_sibling(path);
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
        // The .tmp.<pid>-<n> sibling should be gone (rename moved it).
        let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("f.txt.tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp siblings left behind: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn tokio_write_file_atomic_writes_and_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("graph.bin");
        tokio_write_file_atomic(&path, b"\x01\x02\x03")
            .await
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\x01\x02\x03");
        let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("graph.bin.tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp siblings left behind: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn tokio_write_file_atomic_creates_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a/b/c/state.bin");
        tokio_write_file_atomic(&path, b"x").await.unwrap();
        assert!(path.exists());
    }
}
