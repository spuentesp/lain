//! Service references: URL literals in source that name a declared service.
//!
//! Phase 0 of the inter-repo model. A reference records a URL literal whose
//! host identifies a service. It says nothing about whether the code invokes
//! that service; a call is a sensor's claim, and a reference can be linked to
//! one when a sensor already detected it at the same path and line.
//!
//! Identity uses only what the deployment declares: a service's `hosts`
//! (exact names or `*.suffix` patterns), plus the host each service's `env`
//! variables take in the chosen environment. Environment values come from the
//! parameters lain is launched with, so the same host can resolve differently
//! per environment. Name similarity never links a reference to a service.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::federation::contracts::config::ServiceDecl;
use crate::federation::contracts::deployment::{parse_compose, producer_claims};
use crate::federation::contracts::model::ServiceName;
use crate::federation::contracts::url_resolution::{
    endpoint_from_parts, endpoint_matches, endpoint_pattern, Endpoint,
};

/// What the reference's host identifies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefOutcome {
    /// Exactly one declared service claims the host.
    Resolved(ServiceName),
    /// Several declared services claim the host; all candidates are kept.
    Ambiguous(Vec<ServiceName>),
    /// No declared service claims the host (external, typo, or undeclared).
    UnknownHost,
}

/// Where the literal lives, decided from the path alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    /// Shipped code.
    Production,
    /// Test, e2e, or mock code.
    Test,
    /// Deployment or environment configuration.
    Configuration,
    /// Prose: READMEs, docs, changelogs. A literal there is an example, not a call.
    Documentation,
}

/// One URL literal found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRef {
    pub path: String,
    /// 1-based line of the literal.
    pub line: u32,
    /// The scheme and host as written, e.g. `http://user`.
    pub value: String,
    /// The whole URL literal as written, path included, e.g.
    /// `http://user%s/customers`. Used to compare with the target's routes.
    pub url: String,
    pub host: String,
    pub outcome: RefOutcome,
    pub usage: Usage,
    /// The resolved service is the one that owns this file.
    pub self_reference: bool,
    /// A sensor already detected a call at this path and line.
    pub linked_to_call: bool,
}

/// The services and hosts that apply in one environment.
#[derive(Debug, Clone, Default)]
pub struct EnvScope {
    /// Declared hosts plus hosts taken from environment variables, each
    /// tagged with the service it identifies.
    claims: Vec<(String, ServiceName)>,
}

impl EnvScope {
    /// Build the scope for one environment. `params` are the environment
    /// variables lain was launched with. A service's `env` names whose value
    /// is a URL contribute that URL's host to the service.
    pub fn new(services: &[ServiceDecl], params: &BTreeMap<String, String>) -> Self {
        let mut claims = Vec::new();
        for s in services {
            let name = ServiceName(s.name.clone());
            for h in &s.hosts {
                claims.push((h.clone(), name.clone()));
            }
            for var in &s.env {
                if let Some(pattern) = params.get(var).and_then(|v| endpoint_pattern(v)) {
                    claims.push((pattern, name.clone()));
                }
            }
        }
        Self { claims }
    }

    /// Add claims derived from outside the service declarations, such as a
    /// compose file's published ports.
    fn add_claims(&mut self, claims: Vec<(String, ServiceName)>) {
        self.claims.extend(claims);
    }

    /// Services whose hosts (declared or from the environment) claim `ep`.
    fn claimants(&self, ep: &Endpoint) -> Vec<ServiceName> {
        let mut out: Vec<ServiceName> = Vec::new();
        for (pattern, name) in &self.claims {
            if endpoint_matches(pattern, ep) && !out.contains(name) {
                out.push(name.clone());
            }
        }
        out
    }
}

fn url_host_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // scheme, optional userinfo@, then a host made of DNS-like characters.
        // Stops at the first character that cannot appear in a host (`/`, `:`, quote, `%`).
        Regex::new(
            r#"https?://(?:[^\s"'`<>@/?#]+@)?(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9_][A-Za-z0-9_.\-]*)[^\s"'`<>]*"#,
        )
        .expect("valid regex")
    })
}

/// Classify a path by its components and file name.
pub fn classify_usage(path: &str) -> Usage {
    const TEST_DIRS: &[&str] = &["test", "tests", "__tests__", "spec", "e2e", "mock", "mocks"];
    // Folder names that mark test support: `testUtils`, `fixtures`, `testdata`.
    fn is_test_support_dir(c: &str) -> bool {
        let c = c.to_ascii_lowercase();
        c.starts_with("test") || c.contains("fixture") || c.contains("mock")
    }
    const CONFIG_EXTS: &[&str] = &["yml", "yaml", "env", "properties", "toml", "ini", "conf"];
    let p = std::path::Path::new(path);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if p.components().any(|c| {
        let c = c.as_os_str().to_str().unwrap_or("");
        TEST_DIRS.contains(&c) || is_test_support_dir(c)
    }) {
        return Usage::Test;
    }
    if name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.starts_with("test_")
    {
        return Usage::Test;
    }
    // `testcontainer.sh`, `endpoints_test`: the stem names a test.
    let stem = p
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if stem.starts_with("test") || stem.ends_with("test") || stem.contains("fixture") {
        return Usage::Test;
    }

    let doc_ext = matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "rst" | "txt" | "adoc")
    );
    if doc_ext || name.starts_with("readme") || name.starts_with("changelog") {
        return Usage::Documentation;
    }
    if name.starts_with("docker-compose")
        || name.starts_with("dockerfile")
        || name.starts_with(".env")
    {
        return Usage::Configuration;
    }
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if CONFIG_EXTS.contains(&ext.as_str()) {
        return Usage::Configuration;
    }
    Usage::Production
}

/// Every URL literal in `content` whose host is resolved in `scope`.
///
/// `owner` is the service that owns `path` (its repo), if known; it marks
/// self-references. `has_call_at(path, line)` reports whether a sensor already
/// detected a call there.
pub fn extract_service_refs(
    path: &str,
    content: &str,
    scope: &EnvScope,
    owner: Option<&ServiceName>,
    has_call_at: &dyn Fn(&str, u32) -> bool,
) -> Vec<ServiceRef> {
    let usage = classify_usage(path);
    let mut refs = Vec::new();
    for caps in url_host_re().captures_iter(content) {
        let whole = caps.get(0).expect("group 0 always present");
        let host_match = caps.get(1).expect("group 1 always present");
        let raw_host = host_match.as_str();
        let host = raw_host.to_ascii_lowercase();
        let line = content[..whole.start()]
            .bytes()
            .filter(|b| *b == b'\n')
            .count() as u32
            + 1;
        let literal = whole.as_str();
        let scheme = literal.split_once("://").map_or("http", |(s, _)| s);
        let after_host = &content[host_match.end()..whole.end()];
        let endpoint = endpoint_from_parts(scheme, &host, after_host);
        let candidates = scope.claimants(&endpoint);
        let self_reference = match (owner, candidates.as_slice()) {
            (Some(me), [only]) => only == me,
            _ => false,
        };
        let outcome = match candidates.len() {
            0 => RefOutcome::UnknownHost,
            1 => RefOutcome::Resolved(candidates.into_iter().next().expect("one candidate")),
            _ => RefOutcome::Ambiguous(candidates),
        };
        refs.push(ServiceRef {
            path: path.to_string(),
            line,
            value: format!("{scheme}://{raw_host}"),
            url: whole.as_str().to_string(),
            host,
            outcome,
            usage,
            self_reference,
            linked_to_call: has_call_at(path, line),
        });
    }
    refs
}

fn is_compose_file(path: &std::path::Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    (name.starts_with("docker-compose") || name.starts_with("compose"))
        && (name.ends_with(".yml") || name.ends_with(".yaml"))
}

/// Address claims from every compose file under `root`, for the repo's owner.
fn compose_producer_claims(
    root: &std::path::Path,
    owner: &ServiceName,
) -> Vec<(String, ServiceName)> {
    let mut claims = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_compose_file(e.path()))
    {
        let abs = entry.path();
        let Ok(rel) = abs.strip_prefix(root) else {
            continue;
        };
        if rel.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some(".git" | "node_modules" | "target")
            )
        }) {
            continue;
        }
        let dir = rel
            .parent()
            .map(|d| d.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let Ok(text) = std::fs::read_to_string(abs) else {
            continue;
        };
        if let Ok(svcs) = parse_compose(&text) {
            claims.extend(producer_claims(&dir, &svcs, owner));
        }
    }
    claims
}

/// Every service reference in one repo's workspace.
///
/// Paths in the result are relative to `root`, the form the graph uses, so a
/// reference can be matched with a sensor's call at the same path and line.
/// `owner` is the service that owns this repo. `call_sites` are the
/// `(relative path, line)` pairs where a sensor already detected a call.
pub fn scan_workspace(
    root: &std::path::Path,
    services: &[ServiceDecl],
    params: &BTreeMap<String, String>,
    owner: Option<&ServiceName>,
    call_sites: &std::collections::HashSet<(String, u32)>,
) -> Vec<ServiceRef> {
    let mut scope = EnvScope::new(services, params);
    // Producer addresses from this repo's compose files come first, so every
    // reference is resolved against them whatever the walk order.
    if let Some(owner) = owner {
        scope.add_claims(compose_producer_claims(root, owner));
    }
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path();
        let Ok(rel) = abs.strip_prefix(root) else {
            continue;
        };
        if rel.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some(".git" | "node_modules" | "target")
            )
        }) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(abs) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let has_call_at = |path: &str, line: u32| call_sites.contains(&(path.to_string(), line));
        out.extend(extract_service_refs(
            &rel_str,
            &content,
            &scope,
            owner,
            &has_call_at,
        ));
    }
    out.sort_by(|a, b| (a.path.as_str(), a.line).cmp(&(b.path.as_str(), b.line)));
    out
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn prose_is_documentation_not_production() {
        assert_eq!(classify_usage("user/README.md"), Usage::Documentation);
        assert_eq!(classify_usage("docs/API.rst"), Usage::Documentation);
        assert_eq!(classify_usage("CHANGELOG.md"), Usage::Documentation);
    }

    #[test]
    fn a_test_named_script_is_test() {
        assert_eq!(classify_usage("user/scripts/testcontainer.sh"), Usage::Test);
        assert_eq!(classify_usage("src/endpoints_test.go"), Usage::Test);
        assert_eq!(
            classify_usage("saleor-dashboard/src/orders/fixtures.ts"),
            Usage::Test
        );
        assert_eq!(
            classify_usage("saleor-dashboard/testUtils/setup.ts"),
            Usage::Test
        );
    }

    #[test]
    fn shipped_code_and_compose_are_unchanged() {
        assert_eq!(
            classify_usage("front-end/api/endpoints.js"),
            Usage::Production
        );
        assert_eq!(classify_usage("docker-compose.yml"), Usage::Configuration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a service the way the deployment config does.
    fn svc(name: &str, hosts: &[&str], env: &[&str]) -> ServiceDecl {
        let list = |xs: &[&str]| {
            xs.iter()
                .map(|x| format!("\"{x}\""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        serde_yaml::from_str(&format!(
            "name: {name}\nrepo: {name}\nhosts: [{}]\nenv: [{}]\n",
            list(hosts),
            list(env)
        ))
        .expect("valid service declaration")
    }

    fn scope(services: &[ServiceDecl]) -> EnvScope {
        EnvScope::new(services, &BTreeMap::new())
    }

    fn none(_: &str, _: u32) -> bool {
        false
    }

    #[test]
    fn a_url_literal_resolves_to_the_service_that_declares_its_host() {
        let services = [svc("user", &["user"], &[]), svc("carts", &["carts"], &[])];
        let src = "const base = util.format(\"http://user%s\", domain);\n";
        let refs = extract_service_refs("endpoints.js", src, &scope(&services), None, &none);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].host, "user");
        assert_eq!(refs[0].line, 1);
        assert_eq!(
            refs[0].outcome,
            RefOutcome::Resolved(ServiceName("user".into()))
        );
    }

    #[test]
    fn line_numbers_count_from_the_literal_not_the_file_start() {
        let services = [svc("orders", &["orders"], &[])];
        let src = "\n\nlet a = 1;\nconst u = \"http://orders/orders\";\n";
        let refs = extract_service_refs("x.js", src, &scope(&services), None, &none);
        assert_eq!(refs[0].line, 4);
    }

    #[test]
    fn an_undeclared_host_is_unknown_not_guessed() {
        let services = [svc("user", &["user"], &[])];
        let refs = extract_service_refs(
            "x.js",
            "fetch(\"https://zipkin:9411/api\")",
            &scope(&services),
            None,
            &none,
        );
        assert_eq!(refs[0].host, "zipkin");
        assert_eq!(refs[0].outcome, RefOutcome::UnknownHost);
    }

    #[test]
    fn a_host_claimed_by_two_services_is_ambiguous_and_keeps_both() {
        let services = [
            svc("cache-a", &["cache"], &[]),
            svc("cache-b", &["cache"], &[]),
        ];
        let refs =
            extract_service_refs("x.js", "\"http://cache/x\"", &scope(&services), None, &none);
        assert_eq!(
            refs[0].outcome,
            RefOutcome::Ambiguous(vec![
                ServiceName("cache-a".into()),
                ServiceName("cache-b".into())
            ])
        );
    }

    #[test]
    fn a_wildcard_host_pattern_matches_its_subdomains() {
        let services = [svc("api", &["*.saleor.internal"], &[])];
        let refs = extract_service_refs(
            "x.py",
            "URL = 'https://api.saleor.internal/v1'",
            &scope(&services),
            None,
            &none,
        );
        assert_eq!(
            refs[0].outcome,
            RefOutcome::Resolved(ServiceName("api".into()))
        );
    }

    #[test]
    fn a_service_without_a_declared_host_is_never_linked_by_name() {
        let services = [svc("orders", &[], &[])];
        let refs = extract_service_refs(
            "x.js",
            "\"http://orders/x\"",
            &scope(&services),
            None,
            &none,
        );
        assert_eq!(refs[0].outcome, RefOutcome::UnknownHost);
    }

    #[test]
    fn the_same_host_in_two_places_yields_two_references() {
        let services = [svc("user", &["user"], &[])];
        let src = "a = \"http://user/a\"\nb = \"http://user/b\"\n";
        let refs = extract_service_refs("x.js", src, &scope(&services), None, &none);
        assert_eq!((refs[0].line, refs[1].line), (1, 2));
    }

    #[test]
    fn the_same_host_resolves_per_environment_from_launch_parameters() {
        // The service declares its host through an env var, not a fixed name.
        let services = [svc("orders", &[], &["ORDERS_URL"])];
        let src = "const u = \"http://orders-blue:8080/x\";";
        let mut dev = BTreeMap::new();
        dev.insert(
            "ORDERS_URL".to_string(),
            "http://orders-blue:8080".to_string(),
        );
        let prod = BTreeMap::new();
        let in_dev =
            extract_service_refs("x.js", src, &EnvScope::new(&services, &dev), None, &none);
        let in_prod =
            extract_service_refs("x.js", src, &EnvScope::new(&services, &prod), None, &none);
        assert_eq!(
            in_dev[0].outcome,
            RefOutcome::Resolved(ServiceName("orders".into()))
        );
        assert_eq!(
            in_prod[0].outcome,
            RefOutcome::UnknownHost,
            "no parameter, no identity"
        );
    }

    #[test]
    fn usage_follows_the_path() {
        assert_eq!(
            classify_usage("front-end/api/endpoints.js"),
            Usage::Production
        );
        assert_eq!(
            classify_usage("front-end/test/e2e/cart_test.js"),
            Usage::Test
        );
        assert_eq!(
            classify_usage("orders/test/json-server/server.py"),
            Usage::Test
        );
        assert_eq!(
            classify_usage("user/docker-compose-zipkin.yml"),
            Usage::Configuration
        );
        assert_eq!(
            classify_usage("svc/config/app.properties"),
            Usage::Configuration
        );
    }

    #[test]
    fn a_reference_to_the_owning_service_is_a_self_reference() {
        let services = [svc("user", &["user"], &[]), svc("carts", &["carts"], &[])];
        let owner = ServiceName("user".into());
        let refs = extract_service_refs(
            "user/docker-compose.yml",
            "\"http://user:8084/health\"",
            &scope(&services),
            Some(&owner),
            &none,
        );
        assert!(refs[0].self_reference);
        let other = extract_service_refs(
            "user/app.js",
            "\"http://carts/x\"",
            &scope(&services),
            Some(&owner),
            &none,
        );
        assert!(!other[0].self_reference);
    }

    #[test]
    fn a_reference_is_linked_only_where_a_sensor_saw_a_call() {
        let services = [svc("user", &["user"], &[])];
        let seen = |p: &str, l: u32| p == "app.js" && l == 2;
        let refs = extract_service_refs(
            "app.js",
            "a\n\"http://user/x\"\n",
            &scope(&services),
            None,
            &seen,
        );
        assert!(refs[0].linked_to_call);
        let unlinked = extract_service_refs(
            "app.js",
            "\"http://user/x\"\n",
            &scope(&services),
            None,
            &seen,
        );
        assert!(!unlinked[0].linked_to_call);
    }

    #[test]
    fn a_workspace_scan_reports_repo_relative_paths_and_links_known_calls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/app.js"), "a\n\"http://user/x\"\n").unwrap();
        let services = [svc("user", &["user"], &[]), svc("carts", &["carts"], &[])];
        let calls: std::collections::HashSet<(String, u32)> =
            [("src/app.js".to_string(), 2u32)].into_iter().collect();
        let refs = scan_workspace(dir.path(), &services, &BTreeMap::new(), None, &calls);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].path, "src/app.js");
        assert_eq!(refs[0].line, 2);
        assert!(refs[0].linked_to_call);
    }

    #[test]
    fn userinfo_prefix_is_skipped_when_extracting_the_authority_host() {
        let services = [svc("orders", &["orders"], &[])];
        let external = extract_service_refs(
            "x.js",
            "\"http://orders@external.example/api\"",
            &scope(&services),
            None,
            &none,
        );
        assert_eq!(external.len(), 1);
        assert_eq!(external[0].host, "external.example");
        assert_eq!(external[0].outcome, RefOutcome::UnknownHost);

        let internal = extract_service_refs(
            "x.js",
            "\"http://orders:secret@orders:8080/api\"",
            &scope(&services),
            None,
            &none,
        );
        assert_eq!(internal.len(), 1);
        assert_eq!(internal[0].host, "orders");
        assert_eq!(
            internal[0].outcome,
            RefOutcome::Resolved(ServiceName("orders".into()))
        );
    }

    #[test]
    fn uppercase_hosts_normalize_to_lowercase_for_resolution() {
        let services = [svc("orders", &["orders"], &[])];
        let refs = extract_service_refs(
            "x.js",
            "\"https://ORDERS/api\"",
            &scope(&services),
            None,
            &none,
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].host, "orders");
        assert_eq!(refs[0].value, "https://ORDERS");
        assert_eq!(
            refs[0].outcome,
            RefOutcome::Resolved(ServiceName("orders".into()))
        );
    }
}

/// Scan a real workspace tree against a real config and print the outcome
/// breakdown. Run with `SR_CONFIG=<repos.yaml> SR_ROOT=<dir> SR_ENV=<file>
/// cargo test --lib service_refs_on_disk -- --ignored --nocapture`. Each
/// repo's owning service is the service declared with that repo's directory name.
#[cfg(test)]
mod on_disk {
    use super::*;
    use crate::federation::contracts::config::ContractFederationConfig;

    #[test]
    #[ignore]
    fn service_refs_on_disk() {
        let config = std::env::var("SR_CONFIG").expect("set SR_CONFIG");
        let root = std::path::PathBuf::from(std::env::var("SR_ROOT").expect("set SR_ROOT"));
        let text = std::fs::read_to_string(&config).unwrap();
        let services = ContractFederationConfig::load_from_str(&text)
            .unwrap()
            .services;
        let params: BTreeMap<String, String> = match std::env::var("SR_ENV") {
            Ok(path) => std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .collect(),
            Err(_) => BTreeMap::new(),
        };
        let scope = EnvScope::new(&services, &params);
        let mut counts = BTreeMap::<String, usize>::new();
        let mut files = 0usize;
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            if p.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some(".git" | "node_modules" | "target")
                )
            }) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(p) else {
                continue;
            };
            files += 1;
            let owner = p
                .strip_prefix(&root)
                .ok()
                .and_then(|r| r.components().next())
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .and_then(|dir| {
                    services
                        .iter()
                        .find(|s| s.repo == dir)
                        .map(|s| ServiceName(s.name.clone()))
                });
            for r in extract_service_refs(
                &p.display().to_string(),
                &content,
                &scope,
                owner.as_ref(),
                &|_, _| false,
            ) {
                let key = match &r.outcome {
                    RefOutcome::Resolved(s) => format!("resolved:{}", s.0),
                    RefOutcome::Ambiguous(_) => "ambiguous".to_string(),
                    RefOutcome::UnknownHost => "unknown".to_string(),
                };
                let u = format!(
                    "{:?}{}",
                    r.usage,
                    if r.self_reference { "+self" } else { "" }
                );
                *counts.entry(format!("{key}  [{u}]")).or_default() += 1;
            }
        }
        println!("files read: {files}");
        for (k, v) in &counts {
            println!("{v:>6}  {k}");
        }
    }
}
