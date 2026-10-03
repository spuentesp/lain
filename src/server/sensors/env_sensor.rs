//! Phase C env-var sensor (spec §6).
//!
//! Reads env-var → host bindings from configuration files in the
//! workspace and surfaces them to the joiner as a side channel:
//!
//! - `.env`, `.env.local`, `.env.development`, etc. — `KEY=value`
//!   lines (comments and blanks ignored).
//! - `docker-compose.yml`, `docker-compose.yaml`,
//!   `compose.yml`, `compose.yaml` — top-level `services[*].environment:`
//!   block (key→value strings).
//! - `helm/values.yaml`, `values.yaml` — top-level `env:` block
//!   (a map of `KEY: value`).
//! - `k8s/`, `manifests/`, `deploy/` — `env:` blocks under
//!   `containers:` in any `.yaml`/`.yml` file.
//!
//! One [`EnvBinding`] is emitted per `KEY=value` whose `value` looks
//! like a URL host. When a var resolves in multiple sources to
//! different hosts, the joiner records the conflict as `ambiguous`
//! (and binds nothing); per spec §6 "Emit at most one binding per
//! `var` per repo (multi-file conflict → ambiguous)".
//!
//! The sensor populates a process-wide index keyed by the workspace
//! basename (the same convention `codeowners_sensor` uses). The
//! joiner reads via [`env_bindings_for`] and reports back unmapped
//! vars through [`record_unmapped_var`] / [`take_unmapped_records`].
//!
//! Phase ordering: this sensor runs at phase 0 (the first phase
//! along with the protocol sensors). The spec calls for it to run
//! first so classifiers (`http_client_sensor`'s
//! `process.env.ORDERS_API_URL` scan) see the bindings immediately.
//! The sensor emits no graph nodes (the spec is explicit: env
//! bindings are a joiner-side axis, not graph content), so the count
//! it returns to `run_all` is the number of `EnvBinding`s it
//! discovered.

use crate::error::LainError;
use crate::federation::contracts::coverage::UnresolvedRecord;
use crate::graph::GraphDatabase;
use crate::schema::RepoNamespace;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

// ─── EnvBinding + EnvSource ───────────────────────────────────────────

/// One env-var → host binding discovered in a config file. The
/// joiner reads these from the sensor's per-repo index to resolve
/// `HostPart::Env([var])` against `services[].hosts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvBinding {
    pub var: String,
    pub host: String,
    pub source: EnvSource,
}

/// Where an `EnvBinding` came from. Order in the enum is the
/// per-source priority: the joiner treats them as equivalent
/// (ambiguous on conflict) but tools render this on the ledger for
/// the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvSource {
    DotEnv,
    DockerCompose,
    HelmValues,
    K8sEnv,
}

impl EnvSource {
    pub fn wire_name(self) -> &'static str {
        match self {
            EnvSource::DotEnv => ".env",
            EnvSource::DockerCompose => "docker-compose",
            EnvSource::HelmValues => "helm/values.yaml",
            EnvSource::K8sEnv => "k8s",
        }
    }
}

// ─── Per-repo index ──────────────────────────────────────────────────

/// Per-repo env bindings plus the per-var unmapped record (built
/// incrementally as the joiner reports vars it could not resolve).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RepoEnv {
    /// `var -> [host, source]` per binding. Multiple bindings for one
    /// var are kept so the joiner can detect the conflict; it
    /// resolves them to "ambiguous" or "unmapped" itself.
    bindings: BTreeMap<String, Vec<EnvBinding>>,
    /// vars the joiner asked about but could not find here.
    unmapped: BTreeMap<String, u32>,
}

/// Global index keyed by the workspace basename. `OnceLock` so a
/// test can install fixtures before the first scan runs (the same
/// shape as `codeowners_sensor`).
static INDEX: OnceLock<Mutex<BTreeMap<String, RepoEnv>>> = OnceLock::new();

fn global() -> &'static Mutex<BTreeMap<String, RepoEnv>> {
    INDEX.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn repo_key(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Snapshot of [`EnvBinding`]s the joiner reads when resolving
/// `HostPart::Env` consumers. Cloned (small) so callers can iterate
/// without holding the index lock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvBindingIndex {
    by_var: BTreeMap<String, Vec<EnvBinding>>,
}

impl EnvBindingIndex {
    /// `Some(hosts)` if `var` has at least one binding, else `None`.
    /// The joiner treats a `var` with multiple distinct hosts as
    /// ambiguous and binds nothing.
    pub fn resolve(&self, var: &str) -> Option<Vec<&EnvBinding>> {
        self.by_var.get(var).map(|v| v.iter().collect())
    }

    /// All distinct hosts for `var` (empty when unmapped or the only
    /// binding is an expression we cannot collapse).
    pub fn distinct_hosts(&self, var: &str) -> Vec<String> {
        let mut out: Vec<String> = match self.by_var.get(var) {
            Some(v) => v.iter().map(|b| b.host.clone()).collect(),
            None => return Vec::new(),
        };
        out.sort();
        out.dedup();
        out
    }

    pub fn is_empty(&self) -> bool {
        self.by_var.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_var.len()
    }
}

/// Snapshot the sensor's per-repo index for `root`. Returns an empty
/// index when the workspace has no env bindings (the common case
/// for a repo without any `.env` / compose / helm / k8s file).
pub fn env_bindings_for(root: &Path) -> EnvBindingIndex {
    let key = repo_key(root);
    let guard = match global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let Some(repo) = guard.get(&key) else {
        return EnvBindingIndex::default();
    };
    EnvBindingIndex {
        by_var: repo.bindings.clone(),
    }
}

/// Record that the joiner referenced `var` but the env_sensor has
/// no binding for it (in the per-repo index for `root`). The
/// orchestrator retrieves these via [`take_unmapped_records`] after
/// rejoin and feeds them into the coverage ledger's `unresolved`
/// bucket with `reason = EnvUnmapped`.
pub fn record_unmapped_var(root: &Path, var: &str) {
    let key = repo_key(root);
    let mut guard = match global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let entry = guard.entry(key).or_default();
    *entry.unmapped.entry(var.to_string()).or_insert(0) += 1;
}

/// Drain the per-repo unmapped-var list and return the
/// `UnresolvedRecord`s. Called by the orchestrator after rejoin to
/// fold the per-var counters into the coverage ledger.
pub fn take_unmapped_records(root: &Path) -> Vec<UnresolvedRecord> {
    let key = repo_key(root);
    let mut guard = match global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let Some(repo) = guard.get_mut(&key) else {
        return Vec::new();
    };
    let mut out: Vec<UnresolvedRecord> = Vec::new();
    for (var, count) in repo.unmapped.iter() {
        out.push(UnresolvedRecord {
            reason: crate::federation::contracts::coverage::UnresolvedReason::EnvUnmapped,
            count: *count as usize,
            sample_ids: vec![var.clone()],
        });
    }
    out.sort_by(|a, b| a.sample_ids.cmp(&b.sample_ids));
    repo.unmapped.clear();
    out
}

// ─── Workspace scan ──────────────────────────────────────────────────

/// Walk `root` and parse every supported env-var source. The
/// returned `Vec<EnvBinding>` is the per-repo result. The function
/// is pure (no I/O outside the workspace, no `&mut`); tests exercise
/// it directly.
pub fn scan(root: &Path) -> Vec<EnvBinding> {
    let mut bindings: Vec<EnvBinding> = Vec::new();
    for (rel, source) in env_file_candidates(root) {
        if let Ok(content) = std::fs::read_to_string(&rel) {
            for line in content.lines() {
                if let Some((var, value)) = parse_dotenv_line(line) {
                    if let Some(host) = url_host_from(&value) {
                        bindings.push(EnvBinding {
                            var,
                            host,
                            source,
                        });
                    }
                }
            }
        }
    }
    for (rel, source) in compose_file_candidates(root) {
        if let Ok(content) = std::fs::read_to_string(&rel) {
            for (var, value) in parse_compose_environment(&content) {
                if let Some(host) = url_host_from(&value) {
                    bindings.push(EnvBinding { var, host, source });
                }
            }
        }
    }
    for (rel, source) in helm_file_candidates(root) {
        if let Ok(content) = std::fs::read_to_string(&rel) {
            for (var, value) in parse_helm_env(&content) {
                if let Some(host) = url_host_from(&value) {
                    bindings.push(EnvBinding { var, host, source });
                }
            }
        }
    }
    for (rel, source) in k8s_file_candidates(root) {
        if let Ok(content) = std::fs::read_to_string(&rel) {
            for (var, value) in parse_k8s_env(&content) {
                if let Some(host) = url_host_from(&value) {
                    bindings.push(EnvBinding { var, host, source });
                }
            }
        }
    }
    bindings
}

/// The sensor's `scan` entry point. Reads the workspace, updates
/// the per-repo index, and returns the number of bindings it
/// discovered. The return value is what `run_all` adds to the
/// `SensorCounts` (we map it onto the reserved `EntryPoints` bucket
/// — the env sensor is not a graph producer).
pub fn scan_workspace_env(
    _graph: &GraphDatabase,
    root: &Path,
    _namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    let key = repo_key(root);
    let bindings = scan(root);
    let mut guard = match global().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let mut by_var: BTreeMap<String, Vec<EnvBinding>> = BTreeMap::new();
    for b in bindings {
        by_var.entry(b.var.clone()).or_default().push(b);
    }
    let count: usize = by_var.values().map(|v| v.len()).sum();
    if by_var.is_empty() {
        guard.remove(&key);
    } else {
        let entry = guard.entry(key).or_default();
        entry.bindings = by_var;
    }
    Ok(count)
}

/// The `env` sensor. Submits itself via `register_sensor!`. Phase
/// 0 so it runs first; the spec calls for env bindings to be
/// available to every classifier (the http_client_sensor in
/// particular sees `process.env.ORDERS_API_URL` and the joiner
/// resolves the var via the env_sensor's index).
pub struct EnvSensor;

crate::server::sensors::register_sensor!(
    EnvSensor,
    "env",
    EntryPoints,
    0,
    scan_workspace_env
);

// ─── File discovery ──────────────────────────────────────────────────

fn env_file_candidates(root: &Path) -> Vec<(std::path::PathBuf, EnvSource)> {
    let mut out: Vec<(std::path::PathBuf, EnvSource)> = Vec::new();
    for name in [".env", ".env.local", ".env.development", ".env.production"] {
        let p = root.join(name);
        if p.is_file() {
            out.push((p, EnvSource::DotEnv));
        }
    }
    out
}

fn compose_file_candidates(root: &Path) -> Vec<(std::path::PathBuf, EnvSource)> {
    let mut out: Vec<(std::path::PathBuf, EnvSource)> = Vec::new();
    for name in [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let p = root.join(name);
        if p.is_file() {
            out.push((p, EnvSource::DockerCompose));
        }
    }
    out
}

fn helm_file_candidates(root: &Path) -> Vec<(std::path::PathBuf, EnvSource)> {
    let mut out: Vec<(std::path::PathBuf, EnvSource)> = Vec::new();
    for name in ["helm/values.yaml", "values.yaml"] {
        let p = root.join(name);
        if p.is_file() {
            out.push((p, EnvSource::HelmValues));
        }
    }
    out
}

fn k8s_file_candidates(root: &Path) -> Vec<(std::path::PathBuf, EnvSource)> {
    let mut out: Vec<(std::path::PathBuf, EnvSource)> = Vec::new();
    for dir in ["k8s", "manifests", "deploy"] {
        let d = root.join(dir);
        if !d.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "yaml" || ext == "yml" {
                out.push((path, EnvSource::K8sEnv));
            }
        }
    }
    out
}

// ─── Parsers ────────────────────────────────────────────────────────

/// Parse one `.env` line. Returns `Some((var, value))` for a
/// well-formed `KEY=value`; `None` for blanks, comments, and
/// `export FOO=bar` lines (which we accept by stripping the prefix).
/// Trailing comments (after a non-quoted `#`) and surrounding
/// single/double quotes are stripped.
pub fn parse_dotenv_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let stripped = trimmed
        .strip_prefix("export ")
        .unwrap_or(trimmed)
        .trim();
    let (key, raw_value) = stripped.split_once('=')?;
    let key = key.trim().to_string();
    if key.is_empty() || !is_valid_env_name(&key) {
        return None;
    }
    let raw_value = raw_value.trim();
    // Strip trailing comment (`KEY=foo # bar`).
    let value = match find_unquoted_hash(raw_value) {
        Some(idx) => raw_value[..idx].trim(),
        None => raw_value,
    };
    // Strip surrounding single/double quotes.
    let value = unquote(value);
    if value.is_empty() {
        return None;
    }
    Some((key, value.to_string()))
}

fn is_valid_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn find_unquoted_hash(s: &str) -> Option<usize> {
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in s.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return Some(i),
            _ => {}
        }
    }
    None
}

fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
        {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// Extract the host from a URL-shaped value (`http://host:8080`,
/// `https://api.example.com`, `host:port`, `host`). Anything that
/// doesn't look URL-like returns `None` — non-URL env vars don't
/// affect the joiner.
pub fn url_host_from(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    // Strip a scheme prefix if present.
    let after_scheme = if let Some(idx) = v.find("://") {
        &v[idx + 3..]
    } else {
        v
    };
    // The host ends at the first `/`, `:`, `?`, or `#` after the
    // scheme strip.
    let end = after_scheme
        .find(|c: char| c == '/' || c == ':' || c == '?' || c == '#')
        .unwrap_or(after_scheme.len());
    let host = &after_scheme[..end];
    if host.is_empty() {
        return None;
    }
    // A valid host has no whitespace. We don't try to validate
    // the full RFC 1123 grammar (a hostname can be `*.svc.cluster.local`,
    // an IPv4, or `[::1]`-style IPv6), we just reject anything
    // that contains a space / tab / newline so a misconfigured
    // env var like `ORDERS_URL=not a host` doesn't slip through.
    if host.chars().any(char::is_whitespace) {
        return None;
    }
    Some(host.to_string())
}

/// Parse `docker-compose` `services[*].environment:` blocks. We
/// accept both shapes:
///
/// - `KEY=value` (a scalar string under `environment:`)
/// - `KEY` (a bare key, treated as `KEY=` and ignored — the spec
///   only counts `KEY=value`)
///
/// The YAML is parsed as a generic `serde_yaml::Value` and we walk
/// it in `Mapping` / `Sequence` order; unknown types are skipped
/// silently so a malformed compose file does not abort ingestion.
pub fn parse_compose_environment(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let value: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(_) => return out,
    };
    let Some(services) = value.get("services").and_then(|v| v.as_mapping()) else {
        return out;
    };
    for (_name, svc) in services {
        let Some(env) = svc.get("environment") else {
            continue;
        };
        collect_env_mapping(env, &mut out);
    }
    out
}

fn collect_env_mapping(value: &serde_yaml::Value, out: &mut Vec<(String, String)>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                let Some(key) = k.as_str() else { continue };
                if !is_valid_env_name(key) {
                    continue;
                }
                // `KEY` (no value) is the bare-reference form
                // (`environment: [KEY]`); we don't bind that.
                let Some(s) = v.as_str() else { continue };
                if s.is_empty() {
                    continue;
                }
                out.push((key.to_string(), s.to_string()));
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for entry in seq {
                if let Some(s) = entry.as_str() {
                    // `KEY=value` form (a scalar under `-`).
                    if let Some((k, v)) = parse_dotenv_line(s) {
                        out.push((k, v));
                    }
                } else if let Some(map) = entry.as_mapping() {
                    for (k, v) in map {
                        let Some(key) = k.as_str() else { continue };
                        let Some(s) = v.as_str() else { continue };
                        if is_valid_env_name(key) && !s.is_empty() {
                            out.push((key.to_string(), s.to_string()));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Parse a Helm `values.yaml` (or top-level `env:` block). Walks
/// the entire document collecting any `env:` map whose entries look
/// like `KEY: value`. The spec only requires the top-level `env:`
/// block, but nested `env:` blocks under `containers:` etc. are
/// also accepted so a Helm `values.yaml` mirroring a k8s manifest
/// works.
pub fn parse_helm_env(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let value: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(_) => return out,
    };
    collect_env_blocks(&value, &mut out);
    out
}

fn collect_env_blocks(value: &serde_yaml::Value, out: &mut Vec<(String, String)>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                if k.as_str() == Some("env") {
                    collect_env_mapping(v, out);
                }
                collect_env_blocks(v, out);
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for entry in seq {
                collect_env_blocks(entry, out);
            }
        }
        _ => {}
    }
}

/// Parse k8s manifests: walk the YAML document and collect every
/// `env:` block under `containers:` (the standard k8s shape) or
/// `initContainers:`.
pub fn parse_k8s_env(content: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let value: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(_) => return out,
    };
    collect_containers_env(&value, &mut out);
    out
}

fn collect_containers_env(value: &serde_yaml::Value, out: &mut Vec<(String, String)>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                if matches!(k.as_str(), Some("containers") | Some("initContainers")) {
                    if let serde_yaml::Value::Sequence(seq) = v {
                        for c in seq {
                            if let Some(env) = c.get("env") {
                                collect_k8s_env_entries(env, out);
                            }
                        }
                    }
                }
                // Recurse so a multi-document (`---`-separated) manifest
                // is still scanned.
                collect_containers_env(v, out);
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for entry in seq {
                collect_containers_env(entry, out);
            }
        }
        _ => {}
    }
}

/// Pull the k8s `env:` shape (a list of `{name, value}` pairs) into
/// `(var, value)` tuples. The `value` field can be a literal string
/// or a `valueFrom: { secretKeyRef / configMapKeyRef }` — we only
/// accept the literal `value:` form; the `valueFrom` case is
/// recorded as an unmapped var by the joiner (the env_sensor
/// has nothing to bind it to).
fn collect_k8s_env_entries(value: &serde_yaml::Value, out: &mut Vec<(String, String)>) {
    let serde_yaml::Value::Sequence(seq) = value else {
        return;
    };
    for entry in seq {
        let serde_yaml::Value::Mapping(map) = entry else {
            continue;
        };
        let name = map
            .get(serde_yaml::Value::String("name".into()))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let Some(name) = name else { continue };
        if !is_valid_env_name(&name) {
            continue;
        }
        let value = map
            .get(serde_yaml::Value::String("value".into()))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(v) = value {
            if !v.is_empty() {
                out.push((name, v));
            }
        }
        // `valueFrom` is a dynamic lookup (secret/configmap); the
        // sensor cannot resolve it. We leave the var unmapped so
        // the joiner records `EnvUnmapped` for it.
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    fn test_lock() -> MutexGuard<'static, ()> {
        match TEST_LOCK.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    fn fixed_workspace(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lain_env_sensor_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn clear_index_for(root: &Path) {
        let key = repo_key(root);
        let mut guard = match global().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.remove(&key);
    }

    #[test]
    fn parse_dotenv_handles_comments_and_quotes() {
        assert_eq!(
            parse_dotenv_line("ORDERS_API_URL=http://orders:8080"),
            Some(("ORDERS_API_URL".into(), "http://orders:8080".into()))
        );
        assert_eq!(
            parse_dotenv_line("export BILLING_URL=\"https://api.billing\""),
            Some(("BILLING_URL".into(), "https://api.billing".into()))
        );
        assert!(parse_dotenv_line("# comment").is_none());
        assert!(parse_dotenv_line("").is_none());
        // Trailing comment is stripped.
        assert_eq!(
            parse_dotenv_line("FOO=http://x # comment"),
            Some(("FOO".into(), "http://x".into()))
        );
    }

    #[test]
    fn parse_dotenv_rejects_invalid_names() {
        assert!(parse_dotenv_line("1FOO=http://x").is_none());
        assert!(parse_dotenv_line("=value").is_none());
    }

    #[test]
    fn url_host_from_strips_scheme_path_and_port() {
        assert_eq!(
            url_host_from("http://orders:8080"),
            Some("orders".into())
        );
        assert_eq!(
            url_host_from("https://api.billing/v1"),
            Some("api.billing".into())
        );
        assert_eq!(url_host_from("host-only"), Some("host-only".into()));
        assert_eq!(url_host_from(""), None);
        // No whitespace allowed in the host segment.
        assert_eq!(url_host_from("not a host"), None);
    }

    #[test]
    fn compose_environment_extracts_key_value_pairs() {
        let content = r#"
services:
  orders:
    environment:
      ORDERS_API_URL: http://orders:8080
      LOG_LEVEL: info
  billing:
    environment:
      - BILLING_URL=http://billing:9000
      - DEBUG
"#;
        let pairs = parse_compose_environment(content);
        assert!(pairs.iter().any(|(k, v)| k == "ORDERS_API_URL" && v == "http://orders:8080"));
        assert!(pairs.iter().any(|(k, v)| k == "BILLING_URL" && v == "http://billing:9000"));
        // Non-URL value (LOG_LEVEL=info) survives parse but is
        // filtered out by the host extraction in `scan`.
        assert!(pairs.iter().any(|(k, v)| k == "LOG_LEVEL" && v == "info"));
        // Bare key (DEBUG) is dropped at the parser.
        assert!(!pairs.iter().any(|(k, _)| k == "DEBUG"));
    }

    #[test]
    fn helm_values_extracts_env_block() {
        let content = r#"
image:
  repository: nginx
env:
  ORDERS_API_URL: http://orders:8080
  LOG_LEVEL: warn
"#;
        let pairs = parse_helm_env(content);
        assert!(pairs.iter().any(|(k, v)| k == "ORDERS_API_URL" && v == "http://orders:8080"));
        assert!(pairs.iter().any(|(k, v)| k == "LOG_LEVEL" && v == "warn"));
    }

    #[test]
    fn k8s_manifest_extracts_containers_env() {
        let content = r#"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: orders
spec:
  template:
    spec:
      containers:
        - name: orders
          env:
            - name: ORDERS_API_URL
              value: http://orders:8080
            - name: LOG_LEVEL
              value: info
"#;
        let pairs = parse_k8s_env(content);
        assert!(pairs.iter().any(|(k, v)| k == "ORDERS_API_URL" && v == "http://orders:8080"));
        assert!(pairs.iter().any(|(k, v)| k == "LOG_LEVEL" && v == "info"));
    }

    #[test]
    fn scan_workspace_reads_all_sources() {
        let _g = test_lock();
        let root = fixed_workspace("all_sources");
        clear_index_for(&root);
        std::fs::write(root.join(".env"), "ORDERS_API_URL=http://orders:8080\n").unwrap();
        std::fs::write(
            root.join("docker-compose.yml"),
            "services:\n  app:\n    environment:\n      BILLING_URL: http://billing:9000\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("helm")).unwrap();
        std::fs::write(
            root.join("helm/values.yaml"),
            "env:\n  REPORTS_URL: http://reports:7000\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("k8s")).unwrap();
        std::fs::write(
            root.join("k8s/deploy.yaml"),
            r#"
spec:
  template:
    spec:
      containers:
        - name: app
          env:
            - name: CATALOG_URL
              value: http://catalog:6000
"#,
        )
        .unwrap();

        let bindings = scan(&root);
        let vars: std::collections::BTreeSet<_> =
            bindings.iter().map(|b| b.var.clone()).collect();
        assert!(vars.contains("ORDERS_API_URL"));
        assert!(vars.contains("BILLING_URL"));
        assert!(vars.contains("REPORTS_URL"));
        assert!(vars.contains("CATALOG_URL"));

        // Round-trip through the global index.
        let g = GraphDatabase::new(&root.join("db.bin")).unwrap();
        let n = scan_workspace_env(&g, &root, &RepoNamespace::for_test()).unwrap();
        assert_eq!(n, bindings.len());
        let idx = env_bindings_for(&root);
        assert_eq!(idx.len(), 4);

        clear_index_for(&root);
    }

    #[test]
    fn record_unmapped_var_drains_into_unresolved_records() {
        let _g = test_lock();
        let root = fixed_workspace("unmapped");
        clear_index_for(&root);
        record_unmapped_var(&root, "MISSING_VAR");
        record_unmapped_var(&root, "MISSING_VAR");
        record_unmapped_var(&root, "OTHER");
        let recs = take_unmapped_records(&root);
        assert_eq!(recs.len(), 2);
        let by_var: std::collections::BTreeMap<_, _> = recs
            .iter()
            .map(|r| (r.sample_ids[0].clone(), r.count))
            .collect();
        assert_eq!(by_var["MISSING_VAR"], 2);
        assert_eq!(by_var["OTHER"], 1);
        // Drain is idempotent.
        assert!(take_unmapped_records(&root).is_empty());
    }

    #[test]
    fn distinct_hosts_dedupes_conflicts() {
        let _g = test_lock();
        let root = fixed_workspace("distinct");
        clear_index_for(&root);
        std::fs::write(
            root.join(".env"),
            "ORDERS_API_URL=http://a:80\nORDERS_API_URL=http://b:80\n",
        )
        .unwrap();
        let _ = scan_workspace_env(
            &GraphDatabase::new(&root.join("db.bin")).unwrap(),
            &root,
            &RepoNamespace::for_test(),
        )
        .unwrap();
        let idx = env_bindings_for(&root);
        let hosts = idx.distinct_hosts("ORDERS_API_URL");
        assert_eq!(hosts, vec!["a".to_string(), "b".to_string()]);
        clear_index_for(&root);
    }
}
