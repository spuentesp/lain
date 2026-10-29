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
use crate::federation::contracts::config::{HostAliasDecl, ProviderDecl};

/// Serializes writes within this process. Cross-process races are caught
/// by the byte comparison in [`write_guarded`].
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// The sequence a declaration of `section` belongs in.
///
/// The loader prefers the nested `contract:` block for a section it
/// defines and the top-level section otherwise, so a declaration has
/// to land where the loader will read it. A write into the other
/// place would be reported as applied and then dropped on the next
/// start.
fn declaration_sequence<'a>(
    doc: &'a mut serde_yaml::Value,
    section: &str,
) -> Result<&'a mut Vec<serde_yaml::Value>, LainError> {
    let mapping = doc
        .as_mapping_mut()
        .ok_or_else(|| LainError::Config("repos.yaml is not a YAML mapping".to_string()))?;
    let contract_key = serde_yaml::Value::String("contract".to_string());
    let section_key = serde_yaml::Value::String(section.to_string());
    let has_contract_block = mapping.contains_key(&contract_key);
    let nested_len = mapping
        .get_mut(&contract_key)
        .and_then(|v| v.as_mapping_mut())
        .and_then(|m| m.get_mut(&section_key))
        .and_then(|v| v.as_sequence_mut())
        .map(|s| s.len())
        .unwrap_or(0);
    let top_len = mapping
        .get_mut(&section_key)
        .and_then(|v| v.as_sequence_mut())
        .map(|s| s.len())
        .unwrap_or(0);
    // The nested section wins when it holds declarations; the
    // top-level one wins when it does and the nested block does not.
    // With neither, a file that has a `contract:` block takes the
    // nested section — the block is the file's contract layout.
    let use_nested = nested_len > 0 || (has_contract_block && top_len == 0);
    if use_nested {
        let nested = mapping
            .get_mut(&contract_key)
            .and_then(|v| v.as_mapping_mut())
            .ok_or_else(|| LainError::Config("contract: must be a mapping".to_string()))?;
        if !nested.contains_key(&section_key) {
            nested.insert(section_key.clone(), serde_yaml::Value::Sequence(Vec::new()));
        }
        nested
            .get_mut(&section_key)
            .and_then(|v| v.as_sequence_mut())
            .ok_or_else(|| LainError::Config(format!("contract.{section}: must be a list")))
    } else {
        if !mapping.contains_key(&section_key) {
            mapping.insert(section_key.clone(), serde_yaml::Value::Sequence(Vec::new()));
        }
        mapping
            .get_mut(&section_key)
            .and_then(|v| v.as_sequence_mut())
            .ok_or_else(|| LainError::Config(format!("{section}: must be a list")))
    }
}

/// Append the declarations `incoming` to the `provides` sequence of a
/// `repos.yaml` document, skipping any already present (same service and
/// key). Returns the declarations actually added. Other sections are left
/// untouched, so operator-written content survives the merge.
pub fn merge_provides(
    doc: &mut serde_yaml::Value,
    incoming: &[ProviderDecl],
) -> Result<Vec<ProviderDecl>, LainError> {
    let sequence = declaration_sequence(doc, "provides")?;
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

/// Append the aliases `incoming` to the `host_aliases` sequence of a
/// `repos.yaml` document, skipping any already present (same host, file
/// and service). Returns the aliases actually added. Other sections are
/// left untouched, so operator-written content survives the merge.
pub fn merge_host_aliases(
    doc: &mut serde_yaml::Value,
    incoming: &[HostAliasDecl],
) -> Result<Vec<HostAliasDecl>, LainError> {
    let sequence = declaration_sequence(doc, "host_aliases")?;
    let existing: Vec<HostAliasDecl> = sequence
        .iter()
        .map(|v| {
            serde_yaml::from_value(v.clone())
                .map_err(|e| LainError::Config(format!("host_aliases: {e}")))
        })
        .collect::<Result<_, _>>()?;

    let mut added = Vec::new();
    for decl in incoming {
        let present = existing
            .iter()
            .chain(added.iter())
            .any(|e: &HostAliasDecl| {
                e.host == decl.host && e.file == decl.file && e.service == decl.service
            });
        if !present {
            let value = serde_yaml::to_value(decl)
                .map_err(|e| LainError::Config(format!("host_aliases entry: {e}")))?;
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
    fn merge_appends_host_aliases_and_is_idempotent() {
        let mut d = doc("repos: []\n");
        let alias = HostAliasDecl {
            host: Some("UPSTREAM_HTTP_ORIGIN".into()),
            file: None,
            service: "nat".into(),
            source: Some("agent".into()),
        };
        let added = merge_host_aliases(&mut d, std::slice::from_ref(&alias)).unwrap();
        assert_eq!(added.len(), 1);
        assert_eq!(d["host_aliases"].as_sequence().unwrap().len(), 1);
        // The same alias again is a no-op.
        let again = merge_host_aliases(&mut d, std::slice::from_ref(&alias)).unwrap();
        assert!(again.is_empty());
        assert_eq!(d["host_aliases"].as_sequence().unwrap().len(), 1);
    }

    #[test]
    fn merge_into_nested_contract_block_when_active() {
        let mut d = doc("\
repos:
  - id: nat
contract:
  services:
    - name: nat
      repo: nat
  provides:
    - service: nat
      key: \"http:POST /chat\"
");
        let added = merge_provides(&mut d, &[decl("nat", "http:POST /chat/stream")]).unwrap();
        assert_eq!(added.len(), 1);
        assert!(
            d.get("provides").is_none(),
            "must not write a separate top-level provides list when contract.provides is active"
        );
        assert_eq!(d["contract"]["provides"].as_sequence().unwrap().len(), 2);

        let alias = HostAliasDecl {
            host: Some("UPSTREAM_HTTP_ORIGIN".into()),
            file: None,
            service: "nat".into(),
            source: Some("agent".into()),
        };
        let added_aliases = merge_host_aliases(&mut d, std::slice::from_ref(&alias)).unwrap();
        assert_eq!(added_aliases.len(), 1);
        assert!(d.get("host_aliases").is_none());
        assert_eq!(
            d["contract"]["host_aliases"].as_sequence().unwrap().len(),
            1
        );
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
