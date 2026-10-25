//! Suggested links between a URL reference and a target service's routes.
//!
//! Suggestions only. An embedding similarity never creates an edge or a
//! binding; it proposes a candidate with its score, and a deterministic check
//! confirms it. An exact path match needs no suggestion, so only references
//! whose path differs from every route of their service are considered.

use crate::federation::contracts::model::ServiceName;
use crate::federation::contracts::service_refs::{RefOutcome, ServiceRef, Usage};

/// One proposed route for a reference. Not a fact about the code.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSuggestion {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub url: String,
    pub service: ServiceName,
    /// The route the reference might target, as `METHOD /template`.
    pub route: String,
    /// Cosine similarity of the normalized paths, in [-1, 1].
    pub score: f32,
}

/// The path of a URL literal, without scheme, host, query or fragment.
pub fn path_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let path_start = after_scheme.find('/').unwrap_or(after_scheme.len());
    let path = &after_scheme[path_start..];
    let end = path.find(['?', '#']).unwrap_or(path.len());
    &path[..end]
}

/// Normalize a path for comparison: placeholders become `{}`, a trailing
/// slash is dropped.
pub fn normalize(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '%' if chars.peek() == Some(&'s') => {
                chars.next();
                out.push_str("{}");
            }
            '{' => {
                for inner in chars.by_ref() {
                    if inner == '}' {
                        break;
                    }
                }
                out.push_str("{}");
            }
            _ => out.push(c),
        }
    }
    if out.len() > 1 {
        out = out.trim_end_matches('/').to_string();
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na * nb))
}

/// The route path of a `METHOD /template` selector.
fn route_path(selector: &str) -> &str {
    selector.split_once(' ').map(|(_, p)| p).unwrap_or(selector)
}

/// Suggest, for each resolved reference whose path matches no route of its
/// service exactly, the routes of that service that score at least
/// `threshold`, best first, at most `top_k` per reference.
///
/// `routes` are `(service, selector)` pairs for served routes. `embed` returns
/// a vector for a normalized path, or `None` when no model is loaded.
pub fn suggest_routes(
    refs: &[ServiceRef],
    routes: &[(ServiceName, String)],
    embed: &dyn Fn(&str) -> Option<Vec<f32>>,
    threshold: f32,
    top_k: usize,
) -> Vec<RouteSuggestion> {
    let mut out = Vec::new();
    for r in refs {
        let RefOutcome::Resolved(service) = &r.outcome else {
            continue;
        };
        // Test and configuration literals describe fixtures and deployments,
        // not calls the shipped code makes; they would only add noise here.
        if r.usage != Usage::Production {
            continue;
        }
        let path = path_of(&r.url);
        if path.is_empty() || path == "/" {
            continue;
        }
        let wanted = normalize(path);
        let own: Vec<&String> = routes
            .iter()
            .filter(|(s, _)| s == service)
            .map(|(_, sel)| sel)
            .collect();
        if own.iter().any(|sel| normalize(route_path(sel)) == wanted) {
            continue;
        }
        let Some(query) = embed(&wanted) else {
            continue;
        };
        let mut scored: Vec<(f32, &String)> = own
            .iter()
            .filter_map(|sel| {
                let candidate = embed(&normalize(route_path(sel)))?;
                Some((cosine(&query, &candidate)?, *sel))
            })
            .filter(|(score, _)| *score >= threshold)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (score, sel) in scored.into_iter().take(top_k) {
            out.push(RouteSuggestion {
                repo: String::new(),
                path: r.path.clone(),
                line: r.line,
                url: r.url.clone(),
                service: service.clone(),
                route: sel.clone(),
                score,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::service_refs::Usage;

    fn reference(url: &str, service: &str) -> ServiceRef {
        ServiceRef {
            path: "front/api.js".into(),
            line: 4,
            value: url.split('/').take(3).collect::<Vec<_>>().join("/"),
            url: url.into(),
            host: "user".into(),
            outcome: RefOutcome::Resolved(ServiceName(service.into())),
            usage: Usage::Production,
            self_reference: false,
            linked_to_call: false,
        }
    }

    #[test]
    fn path_is_what_follows_the_host_without_query() {
        assert_eq!(path_of("http://user%s/customers?x=1"), "/customers");
        assert_eq!(path_of("http://catalogue"), "");
        assert_eq!(
            path_of("http://orders:8080/orders/{}/items#top"),
            "/orders/{}/items"
        );
    }

    #[test]
    fn normalization_unifies_placeholders_and_trailing_slashes() {
        assert_eq!(normalize("/carts/%s/items/"), "/carts/{}/items");
        assert_eq!(normalize("/customers/{id}"), "/customers/{}");
        assert_eq!(normalize("/"), "/");
    }

    /// Vectors per normalized path, so the similarity is known exactly.
    fn table<'a>(entries: &'a [(&'a str, [f32; 2])]) -> impl Fn(&str) -> Option<Vec<f32>> + 'a {
        move |p: &str| {
            entries
                .iter()
                .find(|(k, _)| *k == p)
                .map(|(_, v)| v.to_vec())
        }
    }

    #[test]
    fn a_similar_route_is_suggested_with_its_score_and_never_as_a_fact() {
        let refs = [reference("http://user%s/customer", "user")];
        let routes = [
            (ServiceName("user".into()), "GET /customers/{}".to_string()),
            (ServiceName("user".into()), "GET /cards".to_string()),
        ];
        let embed = table(&[
            ("/customer", [1.0, 0.0]),
            ("/customers/{}", [0.9, 0.1]),
            ("/cards", [0.0, 1.0]),
        ]);
        let got = suggest_routes(&refs, &routes, &embed, 0.8, 3);
        assert_eq!(got.len(), 1, "only the similar route clears the threshold");
        assert_eq!(got[0].route, "GET /customers/{}");
        assert!(got[0].score > 0.8);
    }

    #[test]
    fn an_exact_match_needs_no_suggestion() {
        let refs = [reference("http://user/customers", "user")];
        let routes = [(ServiceName("user".into()), "GET /customers".to_string())];
        let embed = table(&[("/customers", [1.0, 0.0])]);
        assert!(suggest_routes(&refs, &routes, &embed, 0.0, 3).is_empty());
    }

    #[test]
    fn no_model_means_no_suggestions_not_guesses() {
        let refs = [reference("http://user/x", "user")];
        let routes = [(ServiceName("user".into()), "GET /y".to_string())];
        let no_model = |_: &str| None;
        assert!(suggest_routes(&refs, &routes, &no_model, 0.0, 3).is_empty());
    }

    #[test]
    fn test_and_configuration_references_get_no_suggestion() {
        let mut in_test = reference("http://user/customer", "user");
        in_test.usage = Usage::Test;
        let refs = [in_test];
        let routes = [(ServiceName("user".into()), "GET /customers/{}".to_string())];
        let embed = table(&[("/customer", [1.0, 0.0]), ("/customers/{}", [1.0, 0.0])]);
        assert!(suggest_routes(&refs, &routes, &embed, 0.0, 3).is_empty());
    }

    #[test]
    fn suggestions_stay_within_the_resolved_service() {
        let refs = [reference("http://user/x", "user")];
        let routes = [(ServiceName("orders".into()), "GET /x-similar".to_string())];
        let embed = table(&[("/x", [1.0, 0.0]), ("/x-similar", [1.0, 0.0])]);
        assert!(suggest_routes(&refs, &routes, &embed, 0.0, 3).is_empty());
    }
}

/// Suggestions on a real workspace with a real model. Run with
/// `SUG_CONFIG=<repos.yaml> SUG_ROOT=<workspace dir> SUG_ROUTES=<routes.json>
/// SUG_MODEL=<dir with model.onnx and tokenizer.json> cargo test --lib
/// suggest_on_disk -- --ignored --nocapture`. `SUG_ROUTES` is a JSON list of
/// `{"service": ..., "selector": "METHOD /template"}`.
#[cfg(test)]
mod on_disk {
    use super::*;
    use crate::federation::contracts::config::ContractFederationConfig;
    use crate::federation::contracts::service_refs::scan_workspace;
    use crate::server::nlp::NlpEmbedder;
    use std::collections::{BTreeMap, HashSet};

    #[test]
    #[ignore]
    fn suggest_on_disk() {
        let config = std::env::var("SUG_CONFIG").expect("set SUG_CONFIG");
        let root = std::path::PathBuf::from(std::env::var("SUG_ROOT").expect("set SUG_ROOT"));
        let routes_path = std::env::var("SUG_ROUTES").expect("set SUG_ROUTES");
        let model = std::path::PathBuf::from(std::env::var("SUG_MODEL").expect("set SUG_MODEL"));
        let services =
            ContractFederationConfig::load_from_str(&std::fs::read_to_string(&config).unwrap())
                .unwrap()
                .services;
        let (m, t) = NlpEmbedder::resolve_model_paths(&model);
        if let Err(e) = NlpEmbedder::with_max_threads(&m, &t, 4) {
            panic!("load error: {e}");
        }
        let embedder = NlpEmbedder::load_or_stub(Some(&model), 4);
        assert!(!embedder.is_stub(), "the model did not load from {model:?}");

        let routes: Vec<(ServiceName, String)> = serde_json::from_str::<Vec<serde_json::Value>>(
            &std::fs::read_to_string(&routes_path).unwrap(),
        )
        .unwrap()
        .iter()
        .map(|r| {
            (
                ServiceName(r["service"].as_str().unwrap().to_string()),
                r["selector"].as_str().unwrap().to_string(),
            )
        })
        .collect();

        let mut refs = Vec::new();
        for s in &services {
            let dir = root.join(&s.repo);
            if !dir.is_dir() {
                continue;
            }
            let owner = ServiceName(s.name.clone());
            for mut r in scan_workspace(
                &dir,
                &services,
                &BTreeMap::new(),
                Some(&owner),
                &HashSet::new(),
            ) {
                r.path = format!("{}/{}", s.repo, r.path);
                refs.push(r);
            }
        }
        let embed = |t: &str| embedder.embed(t).ok();
        let threshold = std::env::var("SUG_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.6);
        let resolved_production = refs
            .iter()
            .filter(|r| {
                matches!(r.outcome, RefOutcome::Resolved(_)) && r.usage == Usage::Production
            })
            .count();
        if std::env::var_os("SUG_DUMP").is_some() {
            for r in refs.iter().filter(|r| r.usage == Usage::Production) {
                if let RefOutcome::Resolved(svc) = &r.outcome {
                    println!("DUMP {}:{}  {}  ->  {}", r.path, r.line, r.url, svc.0);
                }
            }
        }
        let got = suggest_routes(&refs, &routes, &embed, threshold, 2);
        println!(
            "references: {}, resolved production: {}, threshold {threshold}, suggestions: {}",
            refs.len(),
            resolved_production,
            got.len()
        );
        for g in &got {
            println!(
                "{:.3}  {}:{}  {}  ->  {} {}",
                g.score, g.path, g.line, g.url, g.service.0, g.route
            );
        }
    }
}
