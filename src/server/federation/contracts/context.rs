//! Runtime contract context: providers, aliases and confirmed bindings
//! declared into `repos.yaml` by an operator or agent without a restart.
//!
//! The write is the only side effect of this path, so it is narrow:
//! merge into the existing declaration sequences, validate the result, then
//! write. A rejected write never touches the file. A write that loses a
//! race with another writer is refused. A failed re-join is rolled back.

use std::path::{Path, PathBuf};

use crate::error::LainError;
use crate::federation::contracts::config::{ConfirmedBinding, HostAliasDecl, ProviderDecl};
use crate::federation::contracts::joiner::parse_contract_key;

/// Parse a binding key with the existing config/joiner grammar, including
/// legacy short HTTP forms. MCP validation must use this same parser.
pub fn parse_binding_key(
    raw: &str,
) -> Result<crate::federation::contracts::model::ContractKey, String> {
    parse_contract_key(raw)
}

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
    if incoming.is_empty() {
        return Ok(Vec::new());
    }
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
    if incoming.is_empty() {
        return Ok(Vec::new());
    }
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

/// Append existing-format confirmed bindings without replacing a destination.
/// Consumer and provider keys are compared after the same parsing used by the
/// joiner: equivalent key spellings must not evade idempotency or conflicts.
/// Existing YAML entries, including additional metadata, remain unchanged.
pub fn merge_bindings(
    doc: &mut serde_yaml::Value,
    incoming: &[ConfirmedBinding],
) -> Result<Vec<ConfirmedBinding>, LainError> {
    if incoming.is_empty() {
        return Ok(Vec::new());
    }
    let sequence = declaration_sequence(doc, "bindings")?;
    let existing: Vec<ConfirmedBinding> = sequence
        .iter()
        .map(|v| {
            serde_yaml::from_value(v.clone())
                .map_err(|e| LainError::Config(format!("bindings: {e}")))
        })
        .collect::<Result<_, _>>()?;
    let parse = |key: &str| {
        parse_contract_key(key).map_err(|e| LainError::Config(format!("bindings key '{key}': {e}")))
    };
    let mut added: Vec<ConfirmedBinding> = Vec::new();
    for binding in incoming {
        let consumer_key = parse(&binding.consumer.key)?;
        let provider_key = parse(&binding.provider.key)?;
        let mut present = false;
        for prior in existing.iter().chain(added.iter()) {
            if prior.consumer.repo != binding.consumer.repo
                || prior.consumer.path != binding.consumer.path
                || prior.consumer.symbol != binding.consumer.symbol
                || parse(&prior.consumer.key)? != consumer_key
                || matches!((&prior.consumer.site, &binding.consumer.site), (Some(a), Some(b)) if a != b)
            {
                continue;
            }
            if prior.provider.service != binding.provider.service
                || parse(&prior.provider.key)? != provider_key
            {
                return Err(LainError::Config(format!(
                    "binding conflict for {}/{}/{} key '{}': already assigned to service '{}' key '{}'",
                    binding.consumer.repo, binding.consumer.path, binding.consumer.symbol,
                    binding.consumer.key, prior.provider.service, prior.provider.key
                )));
            }
            present |=
                prior.consumer.site.is_none() || prior.consumer.site == binding.consumer.site;
        }
        if !present {
            sequence.push(
                serde_yaml::to_value(binding)
                    .map_err(|e| LainError::Config(format!("bindings entry: {e}")))?,
            );
            added.push(binding.clone());
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

/// A config transaction held across read, validation, write, rejoin and recovery.
/// Cooperating Lain writers share the existing OS advisory lock. External
/// editors need not honor it, so each mutation additionally checks the bytes.
/// Advisory locking cannot provide compare-and-swap against an editor that
/// ignores the lock and replaces the file between that check and the rename.
pub struct ContextTransaction {
    path: PathBuf,
    _lock: crate::server::state_lock::StateLock,
}

/// Acquire the config transaction, failing closed if another writer holds it.
/// The lock file is persistent; the kernel releases ownership on process exit.
pub fn acquire_transaction(path: &Path) -> Result<ContextTransaction, LainError> {
    let lock = crate::server::state_lock::acquire_with(path, 1000, 10, 0);
    if !lock.is_held() {
        return Err(LainError::Config(format!(
            "{} is busy or its config lock cannot be acquired; retry the request",
            path.display()
        )));
    }
    Ok(ContextTransaction {
        path: path.to_path_buf(),
        _lock: lock,
    })
}

impl ContextTransaction {
    /// Refuse a stale preview or an intervening external edit.
    pub fn verify(&self, expected: &[u8]) -> Result<(), LainError> {
        let current = std::fs::read(&self.path)
            .map_err(|e| LainError::Config(format!("read {}: {e}", self.path.display())))?;
        if current != expected {
            return Err(LainError::Config(format!(
                "{} changed since it was read or written; it was left alone; re-run the request",
                self.path.display()
            )));
        }
        Ok(())
    }

    /// Back up the expected content and atomically write the replacement.
    pub fn write_guarded(&self, expected: &[u8], new: &[u8]) -> Result<(), LainError> {
        self.verify(expected)?;
        // Write the already-checked bytes, rather than copying a file an
        // external editor might have replaced after verification.
        crate::cli::io::write_file_atomic(&backup_path(&self.path), expected)
            .map_err(|e| LainError::Config(format!("backup {}: {e}", self.path.display())))?;
        self.verify(expected)?;
        crate::cli::io::write_file_atomic(&self.path, new)
            .map_err(|e| LainError::Config(format!("write {}: {e}", self.path.display())))
    }

    /// Restore only our own bytes. Never overwrite another writer's edit.
    pub fn restore(&self, expected: &[u8], original: &[u8]) -> Result<(), LainError> {
        self.verify(expected)?;
        crate::cli::io::write_file_atomic(&self.path, original)
            .map_err(|e| LainError::Config(format!("restore {}: {e}", self.path.display())))
    }
}

/// Guarded one-shot write; multi-step callers should hold a transaction.
pub fn write_guarded(path: &Path, expected: &[u8], new: &[u8]) -> Result<(), LainError> {
    acquire_transaction(path)?.write_guarded(expected, new)
}

/// Guarded one-shot restore; multi-step callers should hold a transaction.
pub fn restore(path: &Path, expected: &[u8], original: &[u8]) -> Result<(), LainError> {
    acquire_transaction(path)?.restore(expected, original)
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

    fn binding(service: &str, consumer_key: &str, provider_key: &str) -> ConfirmedBinding {
        use crate::federation::contracts::config::{
            ConfirmedBindingConsumer, ConfirmedBindingProvider,
        };
        ConfirmedBinding {
            consumer: ConfirmedBindingConsumer {
                site: None,
                repo: "client".into(),
                path: "src/client.rs".into(),
                symbol: "call".into(),
                key: consumer_key.into(),
            },
            provider: ConfirmedBindingProvider {
                service: service.into(),
                key: provider_key.into(),
            },
        }
    }

    #[test]
    fn merge_bindings_preserves_nested_sections_and_existing_metadata() {
        let mut d = doc(r#"
repos: []
custom: {keep: true}
contract:
  provides: [{service: api, key: 'rpc:grpc:Orders:Get'}]
  host_aliases: [{host: upstream, service: api}]
  bindings:
    - consumer: {repo: client, path: src/client.rs, symbol: call, key: 'rpc:grpc:Orders:Get'}
      provider: {service: api, key: 'rpc:grpc:Orders:Get'}
      evidence: {note: retained}
"#);
        let before = d.clone();
        let rpc = binding("api", "rpc:grpc:Orders:Get", "rpc:grpc:Orders:Get");
        assert!(merge_bindings(&mut d, &[rpc]).unwrap().is_empty());
        assert_eq!(d, before);
        let mut other = binding("api", "rpc:grpc:Orders:List", "rpc:grpc:Orders:List");
        other.consumer.symbol = "list".into();
        assert_eq!(merge_bindings(&mut d, &[other]).unwrap().len(), 1);
        assert_eq!(d["contract"]["provides"], before["contract"]["provides"]);
        assert_eq!(
            d["contract"]["host_aliases"],
            before["contract"]["host_aliases"]
        );
        assert_eq!(d["custom"], before["custom"]);
        assert_eq!(
            d["contract"]["bindings"][0],
            before["contract"]["bindings"][0]
        );
        assert!(d.get("bindings").is_none());
    }

    #[test]
    fn merge_bindings_compares_normalized_keys_and_refuses_conflicts() {
        let mut d = doc("repos: []\n");
        let original = binding("api", "GET /orders/{}", "GET /orders/{}");
        assert_eq!(
            merge_bindings(&mut d, &[original.clone()]).unwrap().len(),
            1
        );
        let equivalent = binding("api", "http:GET /orders/{}", "http:GET /orders/{}");
        assert!(merge_bindings(&mut d, &[equivalent]).unwrap().is_empty());
        let conflict = binding("other", "http:GET /orders/{}", "http:GET /orders/{}");
        assert!(merge_bindings(&mut d, &[conflict])
            .unwrap_err()
            .to_string()
            .contains("binding conflict"));
        assert_eq!(d["bindings"].as_sequence().unwrap().len(), 1);

        let mut rpc_doc = doc("repos: []\n");
        let rpc = binding("api", "rpc:Orders/Get", "rpc:Orders/Get");
        assert_eq!(merge_bindings(&mut rpc_doc, &[rpc]).unwrap().len(), 1);
        let rpc_equivalent = binding("api", "rpc:grpc:Orders:Get", "rpc:grpc:Orders:Get");
        assert!(merge_bindings(&mut rpc_doc, &[rpc_equivalent])
            .unwrap()
            .is_empty());

        let mut empty = doc("repos: []\n");
        let second = binding("other", "GET /orders/{}", "GET /orders/{}");
        assert!(merge_bindings(&mut empty, &[original, second]).is_err());
    }

    #[test]
    fn empty_declarations_do_not_insert_sections() {
        let mut d = doc("repos: []\ncustom: yes\n");
        let before = d.clone();
        assert!(merge_provides(&mut d, &[]).unwrap().is_empty());
        assert!(merge_host_aliases(&mut d, &[]).unwrap().is_empty());
        assert!(merge_bindings(&mut d, &[]).unwrap().is_empty());
        assert_eq!(d, before);
    }

    #[test]
    fn failed_rejoin_restore_preserves_external_writer_and_original_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.yaml");
        let original = b"repos: []\n";
        let candidate = b"repos: [mine]\n";
        let external = b"repos: [other]\n";
        std::fs::write(&path, original).unwrap();
        let transaction = acquire_transaction(&path).unwrap();
        transaction.write_guarded(original, candidate).unwrap();
        std::fs::write(&path, external).unwrap();
        assert!(transaction.restore(candidate, original).is_err());
        assert!(transaction.verify(candidate).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), external);
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), original);
    }

    #[test]
    fn failed_rejoin_restore_recovers_config_without_rewriting_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.yaml");
        let original = b"repos: []\n";
        let candidate = b"repos: [mine]\n";
        std::fs::write(&path, original).unwrap();
        let transaction = acquire_transaction(&path).unwrap();
        transaction.write_guarded(original, candidate).unwrap();
        transaction.restore(candidate, original).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), original);
    }

    #[test]
    fn simultaneous_guarded_writers_cannot_both_replace_same_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.yaml");
        std::fs::write(&path, b"repos: []\n").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writers: Vec<_> = [b"repos: [one]\n", b"repos: [two]\n"]
            .into_iter()
            .map(|bytes| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    write_guarded(&path, b"repos: []\n", bytes).is_ok()
                })
            })
            .collect();
        let successes = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(successes, 1);
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"repos: []\n");
        let result = std::fs::read(&path).unwrap();
        assert!(result == b"repos: [one]\n" || result == b"repos: [two]\n");
    }
}
