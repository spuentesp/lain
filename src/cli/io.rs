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

/// A file written in a stream and published atomically.
///
/// Bytes go to a uniquely named sibling temp file through a buffered writer,
/// so a caller can encode a large value straight to disk instead of building
/// it in memory first. Nothing reaches `dest` until [`Self::commit`] has
/// flushed and fsynced the temp file and renamed it over `dest`; dropping the
/// writer without committing (an encode error, a panic, a full disk) removes
/// the temp file and leaves whatever was at `dest` untouched. A reader
/// therefore sees the old file or the whole new one, never a prefix.
pub struct AtomicWriter {
    tmp: PathBuf,
    dest: PathBuf,
    out: Option<io::BufWriter<fs::File>>,
}

impl AtomicWriter {
    /// Buffer size for the temp file: large enough that a 100 MB encode is
    /// a few hundred writes, small enough to be noise next to the data.
    const BUFFER_BYTES: usize = 1 << 20;

    /// Open the temp file beside `dest`, creating the parent directory.
    pub fn create(dest: &Path) -> io::Result<Self> {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let tmp = tmp_sibling(dest);
        let file = fs::File::create(&tmp)?;
        Ok(Self {
            tmp,
            dest: dest.to_path_buf(),
            out: Some(io::BufWriter::with_capacity(Self::BUFFER_BYTES, file)),
        })
    }

    /// Flush, fsync and rename the temp file over `dest`.
    pub fn commit(mut self) -> io::Result<()> {
        let out = self.out.take().expect("commit consumes the writer once");
        let file = out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.tmp, &self.dest)
        // `self` drops with `out == None`, which skips the cleanup below.
    }
}

impl io::Write for AtomicWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.as_mut().expect("writer is open").write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.as_mut().expect("writer is open").flush()
    }
}

impl Drop for AtomicWriter {
    fn drop(&mut self) {
        // Still open means `commit` never ran: discard the partial file.
        if self.out.take().is_some() {
            let _ = fs::remove_file(&self.tmp);
        }
    }
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

    #[test]
    fn atomic_writer_publishes_only_on_commit() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        let mut w = AtomicWriter::create(&path).unwrap();
        w.write_all(b"whole ").unwrap();
        w.write_all(b"file").unwrap();
        assert!(!path.exists(), "nothing is published before commit");
        w.commit().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"whole file");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp file renamed away: {leftovers:?}"
        );
    }

    /// A write that stops part way (an encode error, a panic, a full disk)
    /// must leave the previous good file in place and clean up its temp.
    #[test]
    fn a_partial_write_never_replaces_the_good_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        fs::write(&path, b"GOOD").unwrap();
        {
            let mut w = AtomicWriter::create(&path).unwrap();
            w.write_all(b"half a graph").unwrap();
            w.flush().unwrap(); // bytes really are on disk in the temp file
                                // dropped without commit: the encode "failed" here
        }
        assert_eq!(fs::read(&path).unwrap(), b"GOOD");
        let entries = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(entries, 1, "the partial temp file must be removed");
    }

    #[test]
    fn atomic_writer_replaces_an_existing_file_whole() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        fs::write(&path, b"old and much longer than the new contents").unwrap();
        let mut w = AtomicWriter::create(&path).unwrap();
        w.write_all(b"new").unwrap();
        w.commit().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
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
