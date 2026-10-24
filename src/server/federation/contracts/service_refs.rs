//! Service references: URL literals in source that name a declared service.
//!
//! This is phase 0 of the inter-repo model. It records *references* — a URL
//! literal whose host identifies a service — and deliberately says nothing
//! about whether the code invokes that service. A reference is evidence of a
//! target, not of a call. Calls stay with the protocol sensors.
//!
//! Resolution uses only what the deployment config declares: a service's
//! `hosts` (exact names or `*.suffix` patterns). No host is equivalent to a
//! service by name similarity alone.

use std::sync::OnceLock;

use regex::Regex;

use crate::federation::contracts::config::ServiceDecl;
use crate::federation::contracts::model::ServiceName;
use crate::federation::contracts::url_resolution::host_matches_pattern;

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

/// One URL literal found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRef {
    pub path: String,
    /// 1-based line of the literal.
    pub line: u32,
    /// The literal exactly as it appears in the source, scheme included.
    pub value: String,
    pub host: String,
    pub outcome: RefOutcome,
}

fn url_host_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // scheme, then a host made of DNS-like characters. Stops at the
        // first character that cannot appear in a host (`/`, `:`, quote, `%`).
        Regex::new(r#"https?://([A-Za-z0-9_][A-Za-z0-9_.\-]*)"#).expect("valid regex")
    })
}

/// Services whose declared hosts claim `host`, in declaration order, without
/// duplicates.
fn claimants(host: &str, services: &[ServiceDecl]) -> Vec<ServiceName> {
    let mut out: Vec<ServiceName> = Vec::new();
    for s in services {
        if s.hosts
            .iter()
            .any(|pattern| host_matches_pattern(pattern, host))
        {
            let name = ServiceName(s.name.clone());
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

/// Every URL literal in `content` whose host is extracted and resolved.
pub fn extract_service_refs(
    path: &str,
    content: &str,
    services: &[ServiceDecl],
) -> Vec<ServiceRef> {
    let mut refs = Vec::new();
    for caps in url_host_re().captures_iter(content) {
        let whole = caps.get(0).expect("group 0 always present");
        let host = caps[1].to_string();
        let line = content[..whole.start()]
            .bytes()
            .filter(|b| *b == b'\n')
            .count() as u32
            + 1;
        let candidates = claimants(&host, services);
        let outcome = match candidates.len() {
            0 => RefOutcome::UnknownHost,
            1 => RefOutcome::Resolved(candidates.into_iter().next().expect("one candidate")),
            _ => RefOutcome::Ambiguous(candidates),
        };
        refs.push(ServiceRef {
            path: path.to_string(),
            line,
            value: whole.as_str().to_string(),
            host,
            outcome,
        });
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a service the way the deployment config does, so defaults stay
    /// whatever the real loader gives them.
    fn svc(name: &str, hosts: &[&str]) -> ServiceDecl {
        let hosts = hosts
            .iter()
            .map(|h| format!("\"{h}\""))
            .collect::<Vec<_>>()
            .join(", ");
        serde_yaml::from_str(&format!("name: {name}\nrepo: {name}\nhosts: [{hosts}]\n"))
            .expect("valid service declaration")
    }

    #[test]
    fn a_url_literal_resolves_to_the_service_that_declares_its_host() {
        let services = [svc("user", &["user"]), svc("carts", &["carts"])];
        let src = "const base = util.format(\"http://user%s\", domain);\n";
        let refs = extract_service_refs("endpoints.js", src, &services);
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
        let services = [svc("orders", &["orders"])];
        let src = "\n\nlet a = 1;\nconst u = \"http://orders/orders\";\n";
        let refs = extract_service_refs("x.js", src, &services);
        assert_eq!(refs[0].line, 4);
    }

    #[test]
    fn an_undeclared_host_is_unknown_not_guessed() {
        let services = [svc("user", &["user"])];
        let refs = extract_service_refs("x.js", "fetch(\"https://zipkin:9411/api\")", &services);
        assert_eq!(refs[0].host, "zipkin");
        assert_eq!(refs[0].outcome, RefOutcome::UnknownHost);
    }

    #[test]
    fn a_host_claimed_by_two_services_is_ambiguous_and_keeps_both() {
        let services = [svc("cache-a", &["cache"]), svc("cache-b", &["cache"])];
        let refs = extract_service_refs("x.js", "\"http://cache/x\"", &services);
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
        let services = [svc("api", &["*.saleor.internal"])];
        let refs =
            extract_service_refs("x.py", "URL = 'https://api.saleor.internal/v1'", &services);
        assert_eq!(
            refs[0].outcome,
            RefOutcome::Resolved(ServiceName("api".into()))
        );
    }

    #[test]
    fn a_reference_without_a_declared_host_is_never_resolved_by_name_similarity() {
        // The service is called `orders`, but it declares no host. Name
        // equality alone must not link `http://orders` to it.
        let services = [svc("orders", &[])];
        let refs = extract_service_refs("x.js", "\"http://orders/x\"", &services);
        assert_eq!(refs[0].outcome, RefOutcome::UnknownHost);
    }

    #[test]
    fn the_same_host_in_two_places_yields_two_references() {
        let services = [svc("user", &["user"])];
        let src = "a = \"http://user/a\"\nb = \"http://user/b\"\n";
        let refs = extract_service_refs("x.js", src, &services);
        assert_eq!(refs.len(), 2);
        assert_eq!((refs[0].line, refs[1].line), (1, 2));
    }
}

/// Scan a real workspace tree against a real config and print the outcome
/// breakdown. Run with `SR_CONFIG=<repos.yaml> SR_ROOT=<dir> cargo test --lib
/// service_refs_on_disk -- --ignored --nocapture`.
#[cfg(test)]
mod on_disk {
    use super::*;
    use crate::federation::contracts::config::ContractFederationConfig;

    #[test]
    #[ignore]
    fn service_refs_on_disk() {
        let config = std::env::var("SR_CONFIG").expect("set SR_CONFIG");
        let root = std::env::var("SR_ROOT").expect("set SR_ROOT");
        let text = std::fs::read_to_string(&config).unwrap();
        let services = ContractFederationConfig::load_from_str(&text)
            .unwrap()
            .services;
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        let mut by_host = std::collections::BTreeMap::<String, usize>::new();
        let mut files = 0usize;
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.path().components().any(|c| {
                c.as_os_str() == ".git"
                    || c.as_os_str() == "node_modules"
                    || c.as_os_str() == "target"
            }) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            files += 1;
            for r in extract_service_refs(&entry.path().display().to_string(), &content, &services)
            {
                let key = match &r.outcome {
                    RefOutcome::Resolved(s) => format!("resolved:{}", s.0),
                    RefOutcome::Ambiguous(_) => "ambiguous".to_string(),
                    RefOutcome::UnknownHost => "unknown".to_string(),
                };
                *counts.entry(key).or_default() += 1;
                *by_host.entry(r.host.clone()).or_default() += 1;
            }
        }
        println!("files read: {files}");
        for (k, v) in &counts {
            println!("{v:>6}  {k}");
        }
        println!("top hosts: {:?}", {
            let mut h: Vec<_> = by_host.into_iter().collect();
            h.sort_by(|a, b| b.1.cmp(&a.1));
            h.into_iter().take(12).collect::<Vec<_>>()
        });
    }
}
