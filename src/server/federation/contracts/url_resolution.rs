//! URL / service-resolution helpers extracted from `joiner.rs`.
//!
//! The 2634-line `joiner.rs` (pre-refactor) owned a cluster of
//! functions that all answer the same question: "given a consumer
//! call's `HostPart` (or a `ClientDef` from the per-repo registry),
//! which `ServiceName` hosts it?". The four primary call sites —
//! `target_service_via_registry`, `target_service_from_env`,
//! `target_service_from_hosts`, and the merged `service_from_host`
//! helper — share a 60-line body of `HostPart` matching,
//! env-var resolution, and host-pattern globbing. `host_matches_pattern`
//! is a sibling.
//!
//! This module hoists those helpers out of `joiner.rs` so the
//! orchestrator module is leaner (R11, partial S3 fix). Pass #4
//! refactored the joiner into `consumer_http` / `consumer_protocol` /
//! `endpoints` / `confirmed`; the helpers remain the joiner's
//! internals; only the *location* changed. The signatures and
//! bodies are unchanged so every existing test stays green.
//!
//! Pass #4 R20 (review §D15) inlined the single-use
//! `compose_for_registry` helper at its only call site
//! (`consumer_http::target_service_via_registry`).

use std::collections::BTreeMap;

use crate::federation::contracts::config::ContractFederationConfig;
use crate::federation::contracts::model::{ConsumerFact, HostPart, ServiceName};
use crate::server::sensors::env_sensor::EnvBindingIndex;

/// Spec §5.3 / I6 — service-from-host dispatch. Combines
/// `target_service_from_env` (env-name match) and
/// `target_service_from_hosts` (host-pattern match) into a single
/// helper so tier 2 can call either without duplicating it.
pub(crate) fn service_from_host(
    host: &HostPart,
    config: &ContractFederationConfig,
) -> Option<ServiceName> {
    match host {
        HostPart::Env(names) => {
            for env in names {
                for s in &config.services {
                    if s.env.iter().any(|e| e == env) {
                        return Some(ServiceName(s.name.clone()));
                    }
                }
            }
            None
        }
        HostPart::Literal(h) => {
            for s in &config.services {
                if s.hosts.iter().any(|pat| host_matches_pattern(pat, h)) {
                    return Some(ServiceName(s.name.clone()));
                }
            }
            None
        }
        _ => None,
    }
}

/// Pre-Phase-C env-var-based service match: a `HostPart::Env([var])`
/// whose var is named in a service's `env` list binds to that
/// service. Phase C adds the env_sensor index path on top of
/// this; the existing direct match is preserved.
pub(crate) fn target_service_from_env(
    consumer: &ConsumerFact,
    config: &ContractFederationConfig,
) -> Option<ServiceName> {
    if let HostPart::Env(envs) = &consumer.url.host {
        for env in envs {
            for s in &config.services {
                if s.env.iter().any(|e| e == env) {
                    return Some(ServiceName(s.name.clone()));
                }
            }
        }
    }
    None
}

/// Pre-Phase-C host-pattern service match: a `HostPart::Literal(host)`
/// whose host matches a service's `hosts` pattern (exact or
/// `*.suffix` glob) binds to that service.
pub(crate) fn target_service_from_hosts(
    consumer: &ConsumerFact,
    config: &ContractFederationConfig,
) -> Option<ServiceName> {
    let HostPart::Literal(host) = &consumer.url.host else {
        return None;
    };
    for s in &config.services {
        if s.hosts.iter().any(|h| host_matches_pattern(h, host)) {
            return Some(ServiceName(s.name.clone()));
        }
    }
    None
}

/// Phase C (spec §6) env-var resolution through the env_sensor's
/// per-repo bindings. Each var is resolved to a host
/// (distinct-hosts deduped). Three outcomes:
///
/// - All vars resolve to the same single host AND the host
///   matches a service's `hosts` list → `Some(Service(s))`.
/// - Vars resolve to multiple distinct hosts → `Some(Ambiguous(hosts))`.
/// - Any var has no binding → the var counters in
///   `unresolved_env_vars` are bumped and `Some(Unmapped)` is
///   returned (the caller emits `Unresolved { EnvUnmapped }`).
pub(crate) fn resolve_env_consumer(
    consumer: &ConsumerFact,
    env: &EnvBindingIndex,
    config: &ContractFederationConfig,
    unresolved_env_vars: &mut BTreeMap<String, u32>,
) -> Option<EnvResolution> {
    let HostPart::Env(vars) = &consumer.url.host else {
        return None;
    };
    if vars.is_empty() {
        return None;
    }
    let mut distinct_hosts: Vec<String> = Vec::new();
    let mut any_unmapped = false;
    for var in vars {
        let hosts = env.distinct_hosts(var);
        if hosts.is_empty() {
            any_unmapped = true;
            *unresolved_env_vars.entry(var.clone()).or_insert(0) += 1;
        } else {
            for h in hosts {
                if !distinct_hosts.contains(&h) {
                    distinct_hosts.push(h);
                }
            }
        }
    }
    if any_unmapped {
        return Some(EnvResolution::Unmapped);
    }
    if distinct_hosts.len() > 1 {
        return Some(EnvResolution::Ambiguous(distinct_hosts));
    }
    let host = &distinct_hosts[0];
    for s in &config.services {
        if s.hosts.iter().any(|h| host_matches_pattern(h, host)) {
            return Some(EnvResolution::Service(ServiceName(s.name.clone())));
        }
    }
    // The `no_service:` key prefix is the joiner's bookkeeping
    // for "the env var resolved to a host, but no
    // services[].hosts pattern claimed that host". The
    // orchestrator reads [`JoinOutput::unresolved_env_vars`]
    // and treats both the bare-var form and the `no_service:`
    // form as `EnvUnmapped` (pass #4 R23, review §D20). The
    // prefix is intentionally not a typed enum key — the
    // coverage ledger's per-(sensor, lang) bucket reason
    // `EnvUnmapped` already collapses both forms.
    *unresolved_env_vars
        .entry(format!("no_service:{}", host))
        .or_insert(0) += 1;
    Some(EnvResolution::Unmapped)
}

/// Phase C (spec §6): result of resolving a `HostPart::Env([var])`
/// consumer against the env_sensor's bindings. Three terminals per
/// the spec:
///
/// - `Service(s)`: every var resolves to the same host AND the
///   host matches a service's `hosts` list.
/// - `Ambiguous(hosts)`: the vars resolve to different hosts.
/// - `Unmapped`: at least one var has no binding.
pub(crate) enum EnvResolution {
    Service(ServiceName),
    Ambiguous(Vec<String>),
    Unmapped,
}

/// Host-pattern glob: `*.suffix` matches `suffix` and any
/// `*.suffix` subdomain; otherwise exact equality.
pub(crate) fn host_matches_pattern(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        host == suffix || host.ends_with(&format!(".{suffix}"))
    } else {
        pattern == host
    }
}

/// Where a URL points: its host (IPv6 kept in brackets) and effective port.
/// The effective port is the explicit one, else the scheme's default
/// (80 for http, 443 for https), else none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: Option<u16>,
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

/// Split `host[:port]` or `[v6][:port]` into its host and explicit port.
/// A trailing `:digits` is a port only when nothing else in the authority
/// could be a host separator, so `[::1]` has no port and `[::1]:8000` has one.
fn split_host_port(authority: &str) -> Option<(String, Option<u16>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = format!("[{}]", &rest[..end]);
        let after = &rest[end + 1..];
        return match after.strip_prefix(':') {
            None if after.is_empty() => Some((host, None)),
            Some(p) => Some((host, Some(p.parse().ok()?))),
            None => None,
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && !port.is_empty() => {
            Some((host.to_string(), Some(port.parse().ok()?)))
        }
        Some((_, _)) => None,
        None => Some((authority.to_string(), None)),
    }
}

/// The endpoint a URL literal addresses. `None` when the URL has no scheme or
/// its authority cannot be read.
pub fn endpoint_of_url(url: &str) -> Option<Endpoint> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end]
        .rsplit_once('@')
        .map_or(&rest[..end], |(_, a)| a);
    let (host, explicit) = split_host_port(authority)?;
    Some(endpoint_with_default(scheme, host, explicit))
}

/// The endpoint for a host that was already read from a URL, when the text
/// after it is known. Used where the literal itself carries template markers
/// (`http://user%s`) that are not part of the authority.
pub fn endpoint_from_parts(scheme: &str, host: &str, after_host: &str) -> Endpoint {
    let explicit = after_host
        .strip_prefix(':')
        .map(|r| {
            r.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
        })
        .and_then(|digits| digits.parse().ok());
    endpoint_with_default(scheme, host.to_string(), explicit)
}

fn endpoint_with_default(scheme: &str, host: String, explicit: Option<u16>) -> Endpoint {
    Endpoint {
        host,
        port: explicit.or_else(|| default_port(&scheme.to_ascii_lowercase())),
    }
}

/// The `host` or `host:port` form a service declares or an env var implies.
/// Used to compare against an [`Endpoint`].
pub fn endpoint_pattern(url: &str) -> Option<String> {
    let ep = endpoint_of_url(url)?;
    Some(match ep.port {
        Some(p) => format!("{}:{p}", ep.host),
        None => ep.host,
    })
}

/// Whether a declared `host` or `host:port` pattern matches an endpoint. A
/// pattern without a port matches any port, as before. A pattern with a port
/// matches only that effective port.
pub fn endpoint_matches(pattern: &str, ep: &Endpoint) -> bool {
    let Some((host, port)) = split_host_port(pattern) else {
        return false;
    };
    if let Some(p) = port {
        if ep.port != Some(p) {
            return false;
        }
    }
    host_matches_pattern(&host, &ep.host)
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    fn ep(url: &str) -> Endpoint {
        endpoint_of_url(url).unwrap()
    }

    #[test]
    fn explicit_port_is_the_effective_port() {
        assert_eq!(ep("http://localhost:8000/graphql/").port, Some(8000));
    }

    #[test]
    fn scheme_default_ports_apply_when_none_is_written() {
        assert_eq!(ep("http://localhost/graphql/").port, Some(80));
        assert_eq!(ep("https://localhost/graphql/").port, Some(443));
        assert_eq!(ep("ftp://localhost/x").port, None);
    }

    #[test]
    fn ipv6_hosts_keep_their_brackets_and_port() {
        assert_eq!(
            ep("http://[::1]:8000/x"),
            Endpoint {
                host: "[::1]".into(),
                port: Some(8000)
            }
        );
        assert_eq!(
            ep("http://[::1]/x"),
            Endpoint {
                host: "[::1]".into(),
                port: Some(80)
            }
        );
    }

    #[test]
    fn userinfo_is_not_part_of_the_host() {
        assert_eq!(ep("http://user:pw@orders:8080/x").host, "orders");
    }

    #[test]
    fn a_port_pattern_matches_only_that_effective_port() {
        assert!(endpoint_matches(
            "localhost:8000",
            &ep("http://localhost:8000/graphql/")
        ));
        assert!(!endpoint_matches(
            "localhost:8000",
            &ep("http://localhost/graphql/")
        ));
        assert!(endpoint_matches(
            "localhost:80",
            &ep("http://localhost/graphql/")
        ));
        assert!(!endpoint_matches(
            "localhost:80",
            &ep("https://localhost/graphql/")
        ));
        assert!(endpoint_matches(
            "localhost:443",
            &ep("https://localhost/graphql/")
        ));
    }

    #[test]
    fn a_host_pattern_without_a_port_matches_any_port() {
        assert!(endpoint_matches(
            "localhost",
            &ep("http://localhost:8000/x")
        ));
        assert!(endpoint_matches("localhost", &ep("http://localhost/x")));
    }

    #[test]
    fn ipv6_patterns_match_with_and_without_a_port() {
        assert!(endpoint_matches("[::1]:8000", &ep("http://[::1]:8000/x")));
        assert!(!endpoint_matches("[::1]:8000", &ep("http://[::1]:9000/x")));
        assert!(endpoint_matches("[::1]", &ep("http://[::1]:9000/x")));
    }

    #[test]
    fn a_pattern_built_from_a_url_round_trips() {
        let pat = endpoint_pattern("http://localhost:8000/graphql/").unwrap();
        assert_eq!(pat, "localhost:8000");
        assert!(endpoint_matches(&pat, &ep("http://localhost:8000/other")));
    }
}
