//! Runtime contract context: providers declared into `repos.yaml` by an
//! operator or an agent, applied without a restart.
//!
//! The write is the only side effect of this path, so it is narrow:
//! merge into the `provides` sequence, validate the whole result, then
//! write. A rejected write never touches the file. A write that loses a
//! race with another writer is refused. A failed re-join is rolled back.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::LainError;
use crate::federation::contracts::config::ProviderDecl;

/// Serializes writes within this process. Cross-process races are caught
/// by the byte comparison in [`write_guarded`].
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Append the declarations `incoming` to the `provides` sequence of a
/// `repos.yaml` document, skipping any already present (same service and
/// key). Returns the declarations actually added. Other sections are left
/// untouched, so operator-written content survives the merge.
pub fn merge_provides(
    doc: &mut serde_yaml::Value,
    incoming: &[ProviderDecl],
) -> Result<Vec<ProviderDecl>, LainError> {
    let mapping = doc
        .as_mapping_mut()
        .ok_or_else(|| LainError::Config("repos.yaml is not a YAML mapping".to_string()))?;
    let key = serde_yaml::Value::String("provides".to_string());
    if !mapping.contains_key(&key) {
        mapping.insert(key.clone(), serde_yaml::Value::Sequence(Vec::new()));
    }
    let sequence = mapping
        .get_mut(&key)
        .and_then(|v| v.as_sequence_mut())
        .ok_or_else(|| LainError::Config("provides: must be a list".to_string()))?;

    let existing: Vec<ProviderDecl> = sequence
        .iter()
        .map(|v| {
            serde_yaml::from_value(v.clone())
                .map_err(|e| LainError::Config(format!("provides: {e}")))
        })
        .collect::<Result<_, _>>()?;

    let mut added = Vec::new();
    for decl in incoming {
        let present = existing
            .iter()
            .chain(added.iter())
            .any(|e: &ProviderDecl| e.service == decl.service && e.key == decl.key);
        if !present {
            let value = serde_yaml::to_value(decl)
                .map_err(|e| LainError::Config(format!("provides entry: {e}")))?;
            sequence.push(value);
            added.push(decl.clone());
        }
    }
    Ok(added)
}

/// The `.bak` path beside `path`: `repos.yaml` → `repos.yaml.bak`.
pub fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{name}.bak"))
}

/// Replace the file's bytes with `new`, but only if they still equal
/// `expected` (the bytes the caller read and validated against). The old
/// file is copied to `.bak` first, then replaced atomically.
///
/// A file that changed since it was read is refused, so a concurrent
/// writer's change is not lost.
pub fn write_guarded(path: &Path, expected: &[u8], new: &[u8]) -> Result<(), LainError> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let current = std::fs::read(path)
        .map_err(|e| LainError::Config(format!("read {}: {e}", path.display())))?;
    if current != expected {
        return Err(LainError::Config(format!(
            "{} changed since it was read; re-run the request against the new content",
            path.display()
        )));
    }
    std::fs::copy(path, backup_path(path))
        .map_err(|e| LainError::Config(format!("backup {}: {e}", path.display())))?;
    crate::cli::io::write_file_atomic(path, new)
        .map_err(|e| LainError::Config(format!("write {}: {e}", path.display())))
}

/// Put the original bytes back. Used when the re-join after a write fails,
/// so the file does not claim a declaration the federation does not serve.
pub fn restore(path: &Path, original: &[u8]) -> Result<(), LainError> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    crate::cli::io::write_file_atomic(path, original)
        .map_err(|e| LainError::Config(format!("restore {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(service: &str, key: &str) -> ProviderDecl {
        ProviderDecl {
            service: service.into(),
            key: key.into(),
            source: Some("agent".into()),
        }
    }

    fn doc(text: &str) -> serde_yaml::Value {
        serde_yaml::from_str(text).unwrap()
    }

    #[test]
    fn merge_appends_and_keeps_other_sections() {
        let mut d =
            doc("repos:\n  - id: nat\nprovides:\n  - service: nat\n    key: \"http:POST /chat\"\n");
        let added = merge_provides(&mut d, &[decl("nat", "http:POST /chat/stream")]).unwrap();
        assert_eq!(added.len(), 1);
        assert!(d.get("repos").is_some(), "other sections must survive");
        assert_eq!(d["provides"].as_sequence().unwrap().len(), 2);
    }

    #[test]
    fn merge_is_idempotent_for_an_identical_declaration() {
        let mut d = doc("repos: []\n");
        let patch = [decl("nat", "http:POST /chat")];
        assert_eq!(merge_provides(&mut d, &patch).unwrap().len(), 1);
        assert_eq!(merge_provides(&mut d, &patch).unwrap().len(), 0);
        assert_eq!(d["provides"].as_sequence().unwrap().len(), 1);
    }

    #[test]
    fn write_guarded_refuses_a_file_that_changed_since_it_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.yaml");
        std::fs::write(&path, b"repos: []\n").unwrap();
        // A concurrent writer changed the file after this caller read it.
        std::fs::write(&path, b"repos: [changed]\n").unwrap();
        let err = write_guarded(&path, b"repos: []\n", b"repos: [mine]\n").unwrap_err();
        assert!(
            err.to_string().contains("changed since it was read"),
            "{err}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"repos: [changed]\n");
        assert!(
            !backup_path(&path).exists(),
            "a refused write leaves no backup"
        );
    }

    #[test]
    fn write_guarded_backs_up_then_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.yaml");
        std::fs::write(&path, b"repos: []\n").unwrap();
        write_guarded(&path, b"repos: []\n", b"repos: [new]\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"repos: [new]\n");
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"repos: []\n");
    }
}
