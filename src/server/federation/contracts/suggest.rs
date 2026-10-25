//! Suggested links between a URL reference and a target service's routes.
//!
//! Suggestions only. An embedding similarity never creates an edge or a
//! binding; it proposes a candidate with its score, and a deterministic check
//! confirms it. An exact path match needs no suggestion, so only references
//! whose path differs from every route of their service are considered.

use crate::federation::contracts::model::ServiceName;
use crate::federation::contracts::normalize::{normalize as normalize_url, UrlPart};
use crate::federation::contracts::service_refs::{RefOutcome, ServiceRef, Usage};

/// The route template of a URL literal, normalized the way served routes are
/// (see `normalize.rs`). A `%s` in the literal is a hole, as the scan records it.
pub fn template_of(url: &str) -> Option<String> {
    let mut parts = Vec::new();
    for (i, piece) in url.split("%s").enumerate() {
        if i > 0 {
            parts.push(UrlPart::Hole("%s".into()));
        }
        if !piece.is_empty() {
            parts.push(UrlPart::Literal(piece.into()));
        }
    }
    normalize_url(&parts).template
}

/// The template of a served route selector such as `GET /carts/{}`.
fn route_template(selector: &str) -> Option<String> {
    let path = selector.split_once(' ').map_or(selector, |(_, p)| p);
    normalize_url(&[UrlPart::Literal(path.into())]).template
}

/// One proposed route for a reference. Not a fact about the code.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteSuggestion {
    pub path: String,
    pub line: u32,
    pub url: String,
    pub service: ServiceName,
    /// The route the reference might target, as `METHOD /template`.
    pub route: String,
    /// Cosine similarity of the normalized paths, in [-1, 1].
    pub score: f32,
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
        let Some(wanted) = template_of(&r.url) else {
            continue;
        };
        if wanted == "/" {
            continue;
        }
        let own: Vec<&String> = routes
            .iter()
            .filter(|(s, _)| s == service)
            .map(|(_, sel)| sel)
            .collect();
        if own
            .iter()
            .any(|sel| route_template(sel).as_deref() == Some(&wanted))
        {
            continue;
        }
        let Some(query) = embed(&wanted) else {
            continue;
        };
        let mut scored: Vec<(f32, &String)> = own
            .iter()
            .filter_map(|sel| {
                let candidate = embed(&route_template(sel)?)?;
                Some((cosine(&query, &candidate)?, *sel))
            })
            .filter(|(score, _)| *score >= threshold)
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (score, sel) in scored.into_iter().take(top_k) {
            out.push(RouteSuggestion {
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
    fn a_reference_becomes_its_canonical_template() {
        assert_eq!(
            template_of("http://user%s/customers?x=1").as_deref(),
            Some("/customers")
        );
        assert_eq!(template_of("http://catalogue").as_deref(), Some("/"));
        assert_eq!(
            template_of("http://orders:8080/orders/{id}/items#top").as_deref(),
            Some("/orders/{}/items")
        );
        assert_eq!(
            template_of("http://carts%s/carts/%s/items/").as_deref(),
            Some("/carts/{}/items")
        );
    }

    #[test]
    fn a_served_route_and_a_reference_meet_in_the_same_form() {
        assert_eq!(
            route_template("GET /customers/:id").as_deref(),
            Some("/customers/{}")
        );
        assert_eq!(
            template_of("http://user%s/customers/%s").as_deref(),
            route_template("GET /customers/{id}").as_deref()
        );
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
        // The run parameters from the same repos.yaml: `params:` gives the env
        // vars that services declare their values, as `--param` does.
        let params: BTreeMap<String, String> =
            serde_yaml::from_str::<serde_yaml::Value>(&std::fs::read_to_string(&config).unwrap())
                .ok()
                .and_then(|doc| doc.get("params").cloned())
                .and_then(|p| serde_yaml::from_value(p).ok())
                .unwrap_or_default();
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
            for mut r in scan_workspace(&dir, &services, &params, Some(&owner), &HashSet::new()) {
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
