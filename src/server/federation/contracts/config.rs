//! Contract-federation configuration (`docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §7.1).
//!
//! `ContractFederationConfig` is the parsed form of the contract-federation
//! sections in `repos.yaml`: `services`, `http_clients`, `generic_keys`,
//! `schemas`, and `bindings`. All sections are optional and default to
//! empty, so existing `repos.yaml` files keep loading.
//!
//! Validation lives with the loader. Every §7.1 error is mapped to a
//! distinct `LainError::Config` message so an operator reading the
//! startup log can identify the rule that fired.
//!
//! `config_hash` is the blake3 hash of the canonical JSON form of the
//! five sections after parsing. The hash is exposed for diagnostics and
//! the future reload-tracking story (§5.3); the joiner itself does not
//! key on it.

use crate::error::LainError;
use crate::federation::contracts::model::service_name_is_valid;
use crate::federation::contracts::normalize;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// The five sections a contract-federation block can carry. All
/// default to empty when the YAML is silent on them. The block lives
/// inside the same `repos.yaml` as the existing `FederationConfig`,
/// loaded via `ContractFederationConfig::load`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractFederationConfig {
    #[serde(default)]
    pub services: Vec<ServiceDecl>,
    #[serde(default)]
    pub http_clients: Vec<HttpClientDecl>,
    #[serde(default)]
    pub generic_keys: Vec<String>,
    #[serde(default)]
    pub schemas: Vec<SchemaDecl>,
    #[serde(default)]
    pub bindings: Vec<ConfirmedBinding>,
    #[serde(default)]
    pub databases: Vec<DatabaseDecl>,
}

/// One database declaration in `repos.yaml#databases` (Gap 23).
/// Maps a named database to its owning service, associated tables,
/// and services it is shared with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DatabaseDecl {
    pub name: String,
    pub service: String,
    #[serde(default)]
    pub tables: Vec<String>,
    #[serde(default)]
    pub shared_with: Vec<String>,
}

/// One declared service. `repo` is a configured repo id; `paths` are
/// repo-relative prefixes (`[]` = whole repo).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceDecl {
    pub name: String,
    pub repo: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub hosts: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
    #[serde(default)]
    pub route_prefixes: Vec<RoutePrefix>,
}

/// One cross-file router mount. The provider declares a route under
/// `prefix`; the joiner prepends `prefix` to the provider template
/// before endpoint grouping (§6.2 + §7.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutePrefix {
    pub path: String,
    pub prefix: String,
}

/// One wrapper client declaration. `call` may contain the literal
/// `{method}` placeholder; the joiner matches by `expr.fn_name` (§7.3
/// rule 1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HttpClientDecl {
    pub call: String,
    pub service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_arg: Option<u8>,
}

/// One schema declaration. Consulted by the joiner to attach
/// event topic payload schemas to endpoints (§6.7, Gap 20).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaDecl {
    pub topic: String,
    pub repo: String,
    pub file: String,
}

/// One person-confirmed binding (`repos.yaml#bindings[<i>]`). The
/// consumer matches every `HttpClientCall` in `repo/path` whose
/// enclosing symbol's name (or `Container.name`) is `symbol` and
/// whose `ContractKey` equals `key`. The provider is the endpoint
/// `(service, key)`. The `key` for both sides is the same wire
/// grammar (`<METHOD> <template>`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmedBinding {
    pub consumer: ConfirmedBindingConsumer,
    pub provider: ConfirmedBindingProvider,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmedBindingConsumer {
    pub repo: String,
    pub path: String,
    pub symbol: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfirmedBindingProvider {
    pub service: String,
    pub key: String,
}

/// The built-in generic keys (§7.1). The joiner's rule 6 skips these
/// so the `/health` style endpoints never bind a consumer call.
pub const BUILTIN_GENERIC_KEYS: &[&str] = &[
    "GET /",
    "GET /health",
    "GET /healthz",
    "GET /ready",
    "GET /readyz",
    "GET /live",
    "GET /livez",
    "GET /ping",
    "GET /status",
    "GET /version",
    "GET /metrics",
    "GET /favicon.ico",
];

/// Maximum `path_arg` value (§7.1: "path_arg greater than 5"). Six or
/// higher is rejected at parse time.
pub const MAX_PATH_ARG: u8 = 5;

impl ContractFederationConfig {
    /// Load the contract-federation block from a `repos.yaml` path.
    /// The block is optional: a file with no `services:`,
    /// `http_clients:`, etc. keys loads to a default config (all
    /// sections empty).
    pub fn load(path: &Path) -> Result<Self, LainError> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| LainError::Io(format!("read contract config: {e}")))?;
        Self::load_from_str(&s)
    }

    /// Parse the contract-federation block from a YAML string. The
    /// block is optional: a string with no relevant keys loads to a
    /// default config.
    pub fn load_from_str(s: &str) -> Result<Self, LainError> {
        // The contract block is optional. `serde_yaml::from_str` errors
        // out on an unknown top-level type, so we walk the keys
        // ourselves and reject anything outside the contract block —
        // the federation's existing loader owns the other top-level
        // keys.
        let value: serde_yaml::Value = serde_yaml::from_str(s)
            .map_err(|e| LainError::Config(format!("contract config yaml: {e}")))?;
        let mapping = match value.as_mapping() {
            Some(m) => m,
            None => return Ok(Self::default()),
        };
        let mut out = Self::default();
        for (k, v) in mapping {
            let key_str = match k.as_str() {
                Some(s) => s,
                None => {
                    return Err(LainError::Config(
                        "non-string top-level key in contract config".to_string(),
                    ))
                }
            };
            match key_str {
                "services" => {
                    out.services = serde_yaml::from_value(v.clone())
                        .map_err(|e| LainError::Config(format!("services: {e}")))?
                }
                "http_clients" => {
                    out.http_clients = serde_yaml::from_value(v.clone())
                        .map_err(|e| LainError::Config(format!("http_clients: {e}")))?
                }
                "generic_keys" => {
                    out.generic_keys = serde_yaml::from_value(v.clone())
                        .map_err(|e| LainError::Config(format!("generic_keys: {e}")))?
                }
                "schemas" => {
                    out.schemas = serde_yaml::from_value(v.clone())
                        .map_err(|e| LainError::Config(format!("schemas: {e}")))?
                }
                "bindings" => {
                    out.bindings = serde_yaml::from_value(v.clone())
                        .map_err(|e| LainError::Config(format!("bindings: {e}")))?
                }
                "databases" => {
                    out.databases = serde_yaml::from_value(v.clone())
                        .map_err(|e| LainError::Config(format!("databases: {e}")))?
                }
                "contract" => {
                    let nested: ContractFederationConfig = serde_yaml::from_value(v.clone())
                        .map_err(|e| LainError::Config(format!("contract: {e}")))?;
                    if out.services.is_empty() {
                        out.services = nested.services;
                    }
                    if out.http_clients.is_empty() {
                        out.http_clients = nested.http_clients;
                    }
                    if out.generic_keys.is_empty() {
                        out.generic_keys = nested.generic_keys;
                    }
                    if out.schemas.is_empty() {
                        out.schemas = nested.schemas;
                    }
                    if out.bindings.is_empty() {
                        out.bindings = nested.bindings;
                    }
                    if out.databases.is_empty() {
                        out.databases = nested.databases;
                    }
                }
                // Other top-level keys are the existing
                // `FederationConfig`'s. They're handled by the
                // federation loader, not by us; here we just skip
                // them so a single load can serve both.
                _ => {}
            }
        }
        Ok(out)
    }

    /// Run every §7.1 validation rule. The caller must supply the set
    /// of configured repo ids (`repos.yaml#repos[].id`) so the
    /// "unknown repo" rule can fire.
    pub fn validate(&self, repo_ids: &[String]) -> Result<(), LainError> {
        // Rule 2: duplicate service name.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for s in &self.services {
            if !seen.insert(s.name.as_str()) {
                return Err(LainError::Config(format!(
                    "duplicate service name '{name}'",
                    name = s.name
                )));
            }
        }
        // Rule 11: service name regex.
        for s in &self.services {
            if !service_name_is_valid(&s.name) {
                return Err(LainError::Config(format!(
                    "service name '{}' does not match ^[a-z0-9][a-z0-9_-]*$",
                    s.name
                )));
            }
        }
        // Rule 1: unknown repo (services).
        for s in &self.services {
            if !repo_ids.iter().any(|r| r == &s.repo) {
                return Err(LainError::Config(format!(
                    "service '{}' references unknown repo '{}'",
                    s.name, s.repo
                )));
            }
        }
        // Rule 3: service name equal to another repo id.
        for s in &self.services {
            if repo_ids.iter().any(|r| r == &s.name && r != &s.repo) {
                return Err(LainError::Config(format!(
                    "service name '{}' collides with a configured repo id",
                    s.name
                )));
            }
        }
        // Rule 4: overlapping paths within one repo. Two paths overlap
        // when one is a prefix of the other AND either equal or
        // followed by a `/` — the trailing slash ensures
        // `services/shipping` and `services/shipping-api` don't
        // overlap.
        for s in &self.services {
            for (i, a) in s.paths.iter().enumerate() {
                for b in s.paths.iter().skip(i + 1) {
                    if path_prefixes_overlap(a, b) {
                        return Err(LainError::Config(format!(
                            "service '{}' has overlapping paths '{a}' and '{b}'",
                            s.name
                        )));
                    }
                }
            }
        }
        for (i, s1) in self.services.iter().enumerate() {
            for s2 in self.services.iter().skip(i + 1) {
                if s1.repo == s2.repo {
                    for a in &s1.paths {
                        for b in &s2.paths {
                            if path_prefixes_overlap(a, b) {
                                return Err(LainError::Config(format!(
                                    "services '{}' and '{}' in repo '{}' have overlapping paths '{a}' and '{b}'",
                                    s1.name, s2.name, s1.repo
                                )));
                            }
                        }
                    }
                }
            }
        }
        // Rule 10: hosts lower-case only (already lower-cased per the
        // §7.1 docs but we reject explicitly).
        for s in &self.services {
            for h in &s.hosts {
                if h.chars().any(|c| c.is_ascii_uppercase()) {
                    return Err(LainError::Config(format!(
                        "service '{}' host '{}' contains upper-case characters",
                        s.name, h
                    )));
                }
            }
        }
        // Rule 6: the same `env` name listed by two services.
        let mut env_owners: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for s in &self.services {
            for e in &s.env {
                if let Some(prev) = env_owners.insert(e.as_str(), s.name.as_str()) {
                    return Err(LainError::Config(format!(
                        "env var '{e}' is listed by both service '{prev}' and '{cur}'",
                        e = e,
                        prev = prev,
                        cur = s.name
                    )));
                }
            }
        }
        // Rule 7: the same exact `hosts` entry listed by two services.
        let mut host_owners: std::collections::HashMap<&str, &str> =
            std::collections::HashMap::new();
        for s in &self.services {
            for h in &s.hosts {
                if let Some(prev) = host_owners.insert(h.as_str(), s.name.as_str()) {
                    return Err(LainError::Config(format!(
                        "host '{h}' is listed by both service '{prev}' and '{cur}'",
                        h = h,
                        prev = prev,
                        cur = s.name
                    )));
                }
            }
        }
        // Implicit service names: every repo id counts as an implicit
        // service name (§4.1). Build the set once.
        let implicit_services: std::collections::HashSet<&str> =
            repo_ids.iter().map(String::as_str).collect();
        let known_services: std::collections::HashSet<&str> = self
            .services
            .iter()
            .map(|s| s.name.as_str())
            .chain(implicit_services.iter().copied())
            .collect();

        // Rule 9: path_arg > 5 (also covered below for http_clients).
        for c in &self.http_clients {
            if let Some(p) = c.path_arg {
                if p > MAX_PATH_ARG {
                    return Err(LainError::Config(format!(
                        "http_clients entry '{call}' path_arg {p} > {MAX_PATH_ARG}",
                        call = c.call
                    )));
                }
            }
        }
        // Rule 5 (http_clients.service): known or implicitly declared.
        for c in &self.http_clients {
            if !known_services.contains(c.service.as_str()) {
                return Err(LainError::Config(format!(
                    "http_clients entry '{call}' references unknown service '{svc}'",
                    call = c.call,
                    svc = c.service
                )));
            }
        }

        // Rule 8: malformed key — every entry that carries one must
        // parse as `<METHOD> <template>` and survive normalization.
        for b in &self.bindings {
            validate_key("bindings.consumer.key", &b.consumer.key)?;
            validate_key("bindings.provider.key", &b.provider.key)?;
        }
        // Generic keys follow the same grammar (§7.1: "it must parse
        // as <METHOD> <template> and survive normalization
        // unchanged").
        for g in &self.generic_keys {
            validate_key("generic_keys", g)?;
        }
        // Rule 5 (bindings.provider.service).
        for b in &self.bindings {
            if !known_services.contains(b.provider.service.as_str()) {
                return Err(LainError::Config(format!(
                    "binding provider references unknown service '{svc}'",
                    svc = b.provider.service
                )));
            }
        }
        // Bindings' consumer.repo must be a configured repo.
        for b in &self.bindings {
            if !repo_ids.iter().any(|r| r == &b.consumer.repo) {
                return Err(LainError::Config(format!(
                    "binding consumer references unknown repo '{repo}'",
                    repo = b.consumer.repo
                )));
            }
        }

        // SchemaDecl entries (stretch) also need a known repo, so we
        // do not emit garbage when PR 15 lands.
        for s in &self.schemas {
            if !repo_ids.iter().any(|r| r == &s.repo) {
                return Err(LainError::Config(format!(
                    "schemas entry '{topic}' references unknown repo '{repo}'",
                    topic = s.topic,
                    repo = s.repo
                )));
            }
            // Task 4: also require a *configured* service whose
            // `repo` matches. An implicit service (a repo id with
            // no `services[]` entry) cannot own a payload schema
            // — `field_join` has no `service_decl` to read
            // `base_path` from, and the payload would silently
            // attach to a wrong endpoint. Reject at validate() so
            // the operator sees the misconfig rather than
            // discovering it via a missing `FieldRemoved` verdict.
            if !self.services.iter().any(|svc| svc.repo == s.repo) {
                return Err(LainError::Config(format!(
                    "schemas entry '{topic}' references repo '{repo}' with no configured service",
                    topic = s.topic,
                    repo = s.repo
                )));
            }
        }

        // Database declarations validation (Gap 23).
        for db in &self.databases {
            if db.name.is_empty() {
                return Err(LainError::Config(
                    "database declaration name cannot be empty".to_string(),
                ));
            }
            if !self.services.iter().any(|s| s.name == db.service) {
                return Err(LainError::Config(format!(
                    "database '{}' references unknown service '{}'",
                    db.name, db.service
                )));
            }
            for shared in &db.shared_with {
                if !self.services.iter().any(|s| &s.name == shared) {
                    return Err(LainError::Config(format!(
                        "database '{}' shared_with references unknown service '{}'",
                        db.name, shared
                    )));
                }
            }
        }

        Ok(())
    }

    /// The full set of generic keys (built-in plus configured). The
    /// joiner consults this list during rule 6 to skip `/health`-style
    /// endpoints.
    pub fn all_generic_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = BUILTIN_GENERIC_KEYS.iter().map(|s| s.to_string()).collect();
        for g in &self.generic_keys {
            if !out.iter().any(|x| x == g) {
                out.push(g.clone());
            }
        }
        // Stable order for determinism.
        out.sort();
        out
    }

    /// blake3 hash of the canonical JSON form of the five sections
    /// after parsing. Used for diagnostics.
    pub fn config_hash(&self) -> String {
        let canonical = serde_json::json!({
            "services": self.services,
            "http_clients": self.http_clients,
            "generic_keys": self.generic_keys,
            "schemas": self.schemas,
            "bindings": self.bindings,
            "databases": self.databases,
        });
        let s = canonical.to_string();
        blake3::hash(s.as_bytes()).to_hex().to_string()
    }
}

/// Two repo-relative path prefixes `a` and `b` overlap iff one is
/// a prefix of the other, and either they are equal or the longer
/// one continues with `/`. The `/` boundary rules out coincidental
/// prefix matches like `services/shipping` and
/// `services/shipping-api`.
fn path_prefixes_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.len() < b.len() && b.starts_with(a) {
        // b is longer and starts with a; overlap iff a ends with
        // `/` or b's next char is `/`.
        return a.ends_with('/') || b.as_bytes().get(a.len()) == Some(&b'/');
    }
    if b.len() < a.len() && a.starts_with(b) {
        return b.ends_with('/') || a.as_bytes().get(b.len()) == Some(&b'/');
    }
    false
}

/// Validate that `key` parses as `<METHOD> <template>` and that the
/// template survives normalization unchanged. Accepts the wire form
/// `http:<METHOD> <template>` and the shortened form
/// `<METHOD> <template>` used by `generic_keys` and the consumer key
/// inside `bindings`. The shortened form is the §7.1 config shape;
/// the full form is what `ContractKey::from_str` requires.
fn validate_key(field: &str, key: &str) -> Result<(), LainError> {
    // Strip the optional `http:` prefix that the wire grammar
    // uses. Config keys are written without it.
    let body = key.strip_prefix("http:").unwrap_or(key);
    // Parse "<METHOD> <template>" by splitting on the first space.
    let (method, template) = body
        .split_once(' ')
        .ok_or_else(|| LainError::Config(format!("{field} '{key}': missing space")))?;
    if method.is_empty() || template.is_empty() {
        return Err(LainError::Config(format!(
            "{field} '{key}': method or template empty"
        )));
    }
    // Validate the method label.
    let _ = crate::federation::contracts::model::ContractKey::from_str(&format!("http:{body}"))
        .map_err(|e| LainError::Config(format!("{field} '{key}': {e}")))?;
    // Validate the template via the normalizer.
    let url = normalize::normalize(&[normalize::UrlPart::Literal(template.to_string())]);
    match url.template {
        Some(t) if t == template => Ok(()),
        Some(t) => Err(LainError::Config(format!(
            "{field} '{key}': template normalized to '{t}'"
        ))),
        None => Err(LainError::Config(format!(
            "{field} '{key}': template became dynamic after normalization"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_ids() -> Vec<String> {
        // The test repo ids deliberately don't collide with the
        // service names we use elsewhere in the suite; §7.1
        // forbids a service name equal to another repo id.
        vec!["alpha".into(), "beta".into(), "gamma".into()]
    }

    #[test]
    fn empty_config_validates() {
        let cfg = ContractFederationConfig::default();
        cfg.validate(&repo_ids()).unwrap();
    }

    #[test]
    fn minimal_config_validates() {
        let yaml = r#"
services:
  - name: orders
    repo: alpha
    paths: []
    hosts: [orders.svc]
    env: [ORDERS_URL]
generic_keys: ["GET /internal/ping"]
bindings: []
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        cfg.validate(&repo_ids()).unwrap();
        // Two built-ins plus the configured one.
        let all = cfg.all_generic_keys();
        assert!(all.contains(&"GET /health".to_string()));
        assert!(all.contains(&"GET /internal/ping".to_string()));
    }

    #[test]
    fn rejects_unknown_repo_in_services() {
        let yaml = r#"
services:
  - name: orders
    repo: does-not-exist
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(err.to_string().contains("unknown repo"), "got: {err}");
    }

    #[test]
    fn rejects_duplicate_service_name() {
        let yaml = r#"
services:
  - name: orders
    repo: alpha
  - name: orders
    repo: beta
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(
            err.to_string().contains("duplicate service name"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_service_name_equal_to_repo_id() {
        let yaml = r#"
services:
  - name: alpha
    repo: beta
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(
            err.to_string()
                .contains("collides with a configured repo id"),
            "got: {err}"
        );
    }

    #[test]
    fn allows_service_name_equal_to_own_repo_id() {
        let yaml = r#"
services:
  - name: alpha
    repo: alpha
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        assert!(cfg.validate(&repo_ids()).is_ok());
    }

    #[test]
    fn allows_single_path_in_service() {
        let yaml = r#"
services:
  - name: shipping
    repo: alpha
    paths: ["services/shipping/"]
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        assert!(cfg.validate(&repo_ids()).is_ok());
    }

    #[test]
    fn allows_disjoint_paths_in_service() {
        let yaml = r#"
services:
  - name: shipping
    repo: alpha
    paths: ["services/shipping/", "services/inventory/"]
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        assert!(cfg.validate(&repo_ids()).is_ok());
    }

    #[test]
    fn rejects_overlapping_paths_in_one_repo() {
        let yaml = r#"
services:
  - name: shipping
    repo: alpha
    paths: ["services/shipping/", "services/shipping/api/"]
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(err.to_string().contains("overlapping paths"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_service_in_http_clients() {
        let yaml = r#"
services:
  - name: orders
    repo: alpha
http_clients:
  - call: "ordersClient.{method}"
    service: nope
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(
            err.to_string().contains("unknown service 'nope'"),
            "got: {err}"
        );
    }

    #[test]
    fn http_clients_can_target_implicit_service() {
        let yaml = r#"
http_clients:
  - call: "alphaClient.{method}"
    service: alpha
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        cfg.validate(&repo_ids()).unwrap();
    }

    #[test]
    fn rejects_duplicate_env_name_across_services() {
        let yaml = r#"
services:
  - name: a
    repo: alpha
    env: [SHARED]
  - name: b
    repo: beta
    env: [SHARED]
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(
            err.to_string()
                .contains("env var 'SHARED' is listed by both"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_duplicate_exact_host_entry() {
        let yaml = r#"
services:
  - name: a
    repo: alpha
    hosts: [api.example.com]
  - name: b
    repo: beta
    hosts: [api.example.com]
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(
            err.to_string()
                .contains("host 'api.example.com' is listed by both"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_malformed_key_missing_space() {
        let yaml = r#"
generic_keys:
  - "GET/no_space"
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(err.to_string().contains("missing space"), "got: {err}");
    }

    #[test]
    fn rejects_malformed_key_does_not_survive_normalization() {
        let yaml = r#"
generic_keys:
  - "GET /api//orders//"
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        // Empty segments collapse, so this normalizes to /api/orders.
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(err.to_string().contains("normalized to"), "got: {err}");
    }

    #[test]
    fn rejects_path_arg_over_5() {
        let yaml = r#"
services:
  - name: orders
    repo: alpha
http_clients:
  - call: "api.fetch"
    service: orders
    path_arg: 6
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(err.to_string().contains("path_arg 6 > 5"), "got: {err}");
    }

    #[test]
    fn rejects_uppercase_host() {
        let yaml = r#"
services:
  - name: orders
    repo: alpha
    hosts: [Orders.SVC]
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(err.to_string().contains("upper-case"), "got: {err}");
    }

    #[test]
    fn rejects_service_name_with_uppercase() {
        let yaml = r#"
services:
  - name: Orders
    repo: alpha
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(
            err.to_string().contains("does not match ^[a-z0-9]"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_binding_unknown_service() {
        let yaml = r#"
bindings:
  - consumer: { repo: beta, path: src/orders_api.py, symbol: create_order, key: "POST /api/orders" }
    provider: { service: nope, key: "POST /api/orders" }
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(err.to_string().contains("unknown service"), "got: {err}");
    }

    #[test]
    fn rejects_binding_unknown_consumer_repo() {
        let yaml = r#"
bindings:
  - consumer: { repo: nope, path: src/orders_api.py, symbol: create_order, key: "POST /api/orders" }
    provider: { service: alpha, key: "POST /api/orders" }
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let err = cfg.validate(&repo_ids()).unwrap_err();
        assert!(err.to_string().contains("unknown repo"), "got: {err}");
    }

    #[test]
    fn config_hash_is_stable_across_calls() {
        let yaml = r#"
services:
  - name: orders
    repo: alpha
    hosts: [orders.svc]
generic_keys: ["GET /internal/ping"]
"#;
        let cfg = ContractFederationConfig::load_from_str(yaml).unwrap();
        let h1 = cfg.config_hash();
        let h2 = cfg.config_hash();
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }
}
