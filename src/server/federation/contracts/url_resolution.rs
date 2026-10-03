//! URL / service-resolution helpers extracted from `joiner.rs`.
//!
//! The 2634-line `joiner.rs` (pre-refactor) owned a cluster of
//! functions that all answer the same question: "given a consumer
//! call's `HostPart` (or a `ClientDef` from the per-repo registry),
//! which `ServiceName` hosts it?". The four primary call sites —
//! `target_service_via_registry`, `target_service_from_env`,
//! `target_service_from_hosts`, and the merged `service_from_host`
//! helper — share a 60-line body of `HostPart` matching,
//! env-var resolution, and host-pattern globbing, and `host_matches_pattern`
//! / `compose_for_registry` are siblings.
//!
//! This module hoists those six helpers out of `joiner.rs` so the
//! orchestrator module is leaner (R11, partial S3 fix). The full
//! joiner split into `consumer_http` / `consumer_protocol` /
//! `endpoints` is deferred (the R11 spec is explicit: "A full
//! split is risky; instead, identify one or two large helper
//! functions"). The helpers remain the joiner's internals; only
//! the *location* changed. The signatures and bodies are
//! unchanged so every existing test stays green.

use std::collections::BTreeMap;

use crate::federation::contracts::clients::{
    compose_and_normalize, ClientDef, UrlPart as RegistryUrlPart,
};
use crate::federation::contracts::config::ContractFederationConfig;
use crate::federation::contracts::model::{ConsumerFact, HostPart, NormalizedUrl, ServiceName};
use crate::server::sensors::env_sensor::EnvBindingIndex;

/// Compose `def.base ++ call.url` into a single [`NormalizedUrl`].
/// Used by `target_service_via_registry` (spec §5.3 tier 2 +
/// §5.2 "Final URL = `normalize(base_parts ++ call_path_parts)`").
/// The call_path is the URL the consumer was emitted with; we
/// render its template as a single `Literal` part and prepend
/// `def.base`. The result feeds the existing `host_for` /
/// `target_service_from_env` / `target_service_from_hosts`
/// dispatch.
pub(crate) fn compose_for_registry(consumer: &ConsumerFact, def: &ClientDef) -> NormalizedUrl {
    let template = consumer.url.template.clone().unwrap_or_default();
    let path_part = if template.is_empty() {
        RegistryUrlPart::Literal("/".into())
    } else {
        RegistryUrlPart::Literal(template)
    };
    compose_and_normalize(&[path_part], &def.base)
}

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
