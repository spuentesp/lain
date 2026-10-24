//! The common record of an external interaction (phase 1).
//!
//! Every source of interactions, a sensor's call site or a declared route,
//! is mapped to the same record. The sensors that produce the sources keep
//! their own detection; this layer only gives their results one shape, so the
//! joiner and tools can reason about interactions without knowing which
//! sensor found them.
//!
//! A service reference is not an interaction. It stays a reference until a
//! call or a route is found at the same place.

use crate::federation::contracts::model::ServiceName;
use crate::federation::contracts::service_refs::{RefOutcome, ServiceRef};
use crate::schema::{GraphNode, NodeType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// This code calls out to another service.
    Invoke,
    /// This code serves a request from another service.
    Serve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
}

/// Where an invocation goes, or which service serves a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Service(ServiceName),
    Ambiguous(Vec<ServiceName>),
    /// The reason the destination is not known, in the words a tool can show.
    Unresolved(&'static str),
}

/// Where the interaction was seen. Each value names what backs the claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// A sensor detected a call at this place.
    CallSite,
    /// A sensor declared a route at this place.
    Route,
}

/// One interaction between this code and another service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interaction {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub operation: Operation,
    pub transport: Transport,
    /// The method and path template, as the sensor wrote it.
    pub selector: String,
    pub destination: Destination,
    pub evidence: Evidence,
}

/// The destination an outbound call resolves to, from the service reference
/// that names it at the same place. No reference at that place means no
/// destination is known.
fn destination_for_call(refs: &[ServiceRef], path: &str, line: u32) -> Destination {
    let Some(r) = refs.iter().find(|r| r.path == path && r.line == line) else {
        return Destination::Unresolved("no_reference");
    };
    match &r.outcome {
        RefOutcome::Resolved(s) => Destination::Service(s.clone()),
        RefOutcome::Ambiguous(all) => Destination::Ambiguous(all.clone()),
        RefOutcome::UnknownHost => Destination::Unresolved("unknown_host"),
    }
}

/// Interactions for one repo, from its graph nodes and its service references.
///
/// `nodes` are the repo's graph nodes (call sites and routes are read from
/// them); `refs` are the repo's service references; `owner` is the service
/// that owns the repo, which is the destination of its routes.
pub fn interactions_for_repo(
    repo: &str,
    nodes: &[GraphNode],
    refs: &[ServiceRef],
    owner: Option<&ServiceName>,
) -> Vec<Interaction> {
    let mut out = Vec::new();
    for n in nodes {
        match n.node_type {
            NodeType::HttpClientCall => {
                let line = n.line_start.unwrap_or(0);
                out.push(Interaction {
                    repo: repo.to_string(),
                    path: n.path.clone(),
                    line,
                    operation: Operation::Invoke,
                    transport: Transport::Http,
                    selector: n.name.clone(),
                    destination: destination_for_call(refs, &n.path, line),
                    evidence: Evidence::CallSite,
                });
            }
            NodeType::HttpRoute => {
                let destination = match owner {
                    Some(s) => Destination::Service(s.clone()),
                    None => Destination::Unresolved("no_owner_service"),
                };
                out.push(Interaction {
                    repo: repo.to_string(),
                    path: n.path.clone(),
                    line: n.line_start.unwrap_or(0),
                    operation: Operation::Serve,
                    transport: Transport::Http,
                    selector: n.name.clone(),
                    destination,
                    evidence: Evidence::Route,
                });
            }
            _ => {}
        }
    }
    out.sort_by(|a, b| {
        (a.path.as_str(), a.line, &a.selector).cmp(&(b.path.as_str(), b.line, &b.selector))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::service_refs::Usage;

    fn node(t: NodeType, name: &str, path: &str, line: u32) -> GraphNode {
        let mut n = GraphNode::new(t, name.into(), path.into());
        n.line_start = Some(line);
        n
    }

    fn reference(path: &str, line: u32, outcome: RefOutcome) -> ServiceRef {
        ServiceRef {
            path: path.into(),
            line,
            value: "http://x".into(),
            url: "http://x".into(),
            host: "x".into(),
            outcome,
            usage: Usage::Production,
            self_reference: false,
            linked_to_call: false,
        }
    }

    #[test]
    fn a_call_site_with_a_reference_at_its_line_names_the_destination() {
        let nodes = [node(
            NodeType::HttpClientCall,
            "GET /customers",
            "front/api.js",
            3,
        )];
        let refs = [reference(
            "front/api.js",
            3,
            RefOutcome::Resolved(ServiceName("user".into())),
        )];
        let got = interactions_for_repo("front-end", &nodes, &refs, None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].operation, Operation::Invoke);
        assert_eq!(
            got[0].destination,
            Destination::Service(ServiceName("user".into()))
        );
        assert_eq!(got[0].evidence, Evidence::CallSite);
    }

    #[test]
    fn a_call_site_without_a_reference_is_unresolved_not_guessed() {
        let nodes = [node(NodeType::HttpClientCall, "GET /x", "a.js", 1)];
        let got = interactions_for_repo("r", &nodes, &[], None);
        assert_eq!(got[0].destination, Destination::Unresolved("no_reference"));
    }

    #[test]
    fn an_ambiguous_reference_keeps_its_candidates() {
        let nodes = [node(NodeType::HttpClientCall, "GET /x", "a.js", 1)];
        let refs = [reference(
            "a.js",
            1,
            RefOutcome::Ambiguous(vec![ServiceName("a".into()), ServiceName("b".into())]),
        )];
        let got = interactions_for_repo("r", &nodes, &refs, None);
        assert_eq!(
            got[0].destination,
            Destination::Ambiguous(vec![ServiceName("a".into()), ServiceName("b".into())])
        );
    }

    #[test]
    fn a_route_is_served_by_the_repos_own_service() {
        let nodes = [node(NodeType::HttpRoute, "GET /users", "api.py", 9)];
        let got = interactions_for_repo("user", &nodes, &[], Some(&ServiceName("user".into())));
        assert_eq!(got[0].operation, Operation::Serve);
        assert_eq!(
            got[0].destination,
            Destination::Service(ServiceName("user".into()))
        );
        assert_eq!(got[0].evidence, Evidence::Route);
    }

    #[test]
    fn a_route_in_a_repo_with_no_service_has_no_destination() {
        let nodes = [node(NodeType::HttpRoute, "GET /x", "api.py", 1)];
        let got = interactions_for_repo("loose", &nodes, &[], None);
        assert_eq!(
            got[0].destination,
            Destination::Unresolved("no_owner_service")
        );
    }

    #[test]
    fn a_bare_reference_is_not_an_interaction() {
        let refs = [reference(
            "a.js",
            1,
            RefOutcome::Resolved(ServiceName("user".into())),
        )];
        assert!(interactions_for_repo("r", &[], &refs, None).is_empty());
    }
}
