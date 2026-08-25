//! Cross-runtime protocol handler
//!
//! Finds callers at the protocol level: HTTP routes, gRPC services,
//! GraphQL resolvers that reference a given handler.

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::EdgeType;
use crate::error::LainError;

/// Find protocol-level callers (HTTP routes, gRPC services, etc.) for a symbol
/// `node_id` accepts a symbol *name* as well as a raw graph id.
///
/// The tool is documented as taking "a symbol", and every other tool on
/// this surface resolves names — but this one looked up the id directly,
/// so passing the documented thing returned `Node <name> not found`.
/// `resolve_node` handles id, name, and path alike.
pub fn get_cross_runtime_callers(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    node_id: &str,
) -> Result<String, LainError> {
    let node = crate::server::tools::utils::resolve_node(graph, overlay, node_id)?;
    let node_id = node.id.as_str();

    let mut output = format!("## Cross-Runtime Callers for: {}\n\n", node.name);

    // Find incoming CallsHttp edges (HTTP routes calling this handler)
    let http_incoming: Vec<_> = graph.get_edges_to(node_id)?
        .into_iter()
        .filter(|e| matches!(e.edge_type, EdgeType::CallsHttp))
        .collect();

    // Find incoming Implements edges (gRPC service methods implemented by this handler)
    let grpc_incoming: Vec<_> = graph.get_edges_to(node_id)?
        .into_iter()
        .filter(|e| matches!(e.edge_type, EdgeType::Implements))
        .collect();

    // Find incoming Uses edges from GraphQL nodes
    let gql_incoming: Vec<_> = graph.get_edges_to(node_id)?
        .into_iter()
        .filter(|e| matches!(e.edge_type, EdgeType::Uses))
        .collect();

    // HTTP routes
    //
    // "No HTTP routes call this handler" is a claim about the codebase.
    // When nothing emits `CallsHttp` it is not that claim at all — it is
    // "lain cannot see HTTP routes in this build" — and an agent has no
    // way to tell the two apart from an empty list. Say which one it is.
    output.push_str("### HTTP Routes\n");
    if http_incoming.is_empty() {
        if EdgeType::CallsHttp.is_indexed() {
            output.push_str("- No HTTP routes call this handler\n");
        } else {
            output.push_str(
                "- Not indexed in this build: no HTTP route extractor runs \
                 during ingestion, so this is a blind spot rather than an \
                 absence of routes\n",
            );
        }
    } else {
        for edge in http_incoming {
            if let Ok(source) = graph.get_node(&edge.source_id) {
                if let Some(n) = source {
                    output.push_str(&format!("- **{}** ({})\n", n.name, n.path));
                }
            }
        }
    }

    // gRPC services
    output.push_str("\n### gRPC Services\n");
    if grpc_incoming.is_empty() {
        if EdgeType::Implements.is_indexed() {
            output.push_str("- No gRPC services implement this handler\n");
        } else {
            output.push_str(
                "- Not indexed in this build: no `Implements` edges are \
                 produced during ingestion, so this is a blind spot rather \
                 than an absence of services\n",
            );
        }
    } else {
        for edge in grpc_incoming {
            if let Ok(source) = graph.get_node(&edge.source_id) {
                if let Some(n) = source {
                    output.push_str(&format!("- **{}** ({})\n", n.name, n.path));
                }
            }
        }
    }

    // GraphQL resolvers
    output.push_str("\n### GraphQL Resolvers\n");
    if gql_incoming.is_empty() {
        output.push_str("- No GraphQL fields use this resolver\n");
    } else {
        for edge in gql_incoming {
            if let Ok(source) = graph.get_node(&edge.source_id) {
                if let Some(n) = source {
                    output.push_str(&format!("- **{}** ({})\n", n.name, n.path));
                }
            }
        }
    }

    Ok(output)
}
#[cfg(test)]
mod blind_spot_tests {
    use super::*;
    use crate::schema::{GraphNode, NodeType};

    /// An empty section must not read as a fact about the user's code when
    /// the edge type behind it is never produced. `CallsHttp` and
    /// `Implements` come only from `server::sensors`, which the ingest
    /// pipeline does not call, so "No HTTP routes call this handler" was
    /// asserted for every symbol in every codebase — including ones with
    /// hundreds of routes.
    #[test]
    fn unindexed_sections_report_a_blind_spot_not_an_absence() {
        let tmp = std::env::temp_dir().join("cross_runtime_blind_spot");
        let _ = std::fs::remove_dir_all(&tmp);
        let graph = GraphDatabase::new(&tmp).unwrap();
        let overlay = VolatileOverlay::new();

        graph
            .upsert_node(GraphNode::new(
                NodeType::Function,
                "list_users".into(),
                "src/api.rs".into(),
            ))
            .unwrap();

        let out = get_cross_runtime_callers(&graph, &overlay, "list_users").unwrap();

        if EdgeType::CallsHttp.is_indexed() {
            assert!(out.contains("No HTTP routes call this handler"));
        } else {
            assert!(
                out.contains("Not indexed in this build"),
                "an unindexed edge must be reported as a blind spot: {out}"
            );
            assert!(
                !out.contains("No HTTP routes call this handler"),
                "must not assert an absence it cannot know: {out}"
            );
        }
    }

    /// The GraphQL section reads `Uses`, which the indexer does produce,
    /// so it keeps making a real claim — this is what keeps the tool worth
    /// advertising at all rather than filtering it from `tools/list`.
    #[test]
    fn the_graphql_section_still_makes_a_real_claim() {
        assert!(
            EdgeType::Uses.is_indexed(),
            "the GraphQL section reads Uses edges; if those stopped being \
             produced the whole tool would be inert and should be filtered \
             from tools/list like semantic_search"
        );
    }
}
