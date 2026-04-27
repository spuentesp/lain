//! Cross-runtime protocol handler
//!
//! Finds callers at the protocol level: HTTP routes, gRPC services,
//! GraphQL resolvers that reference a given handler.

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::EdgeType;
use crate::error::LainError;

/// Find protocol-level callers (HTTP routes, gRPC services, etc.) for a symbol
pub fn get_cross_runtime_callers(
    graph: &GraphDatabase,
    _overlay: &VolatileOverlay,
    node_id: &str,
) -> Result<String, LainError> {
    let node = graph.get_node(node_id)?
        .ok_or_else(|| LainError::NotFound(format!("Node {} not found", node_id)))?;

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
    output.push_str("### HTTP Routes\n");
    if http_incoming.is_empty() {
        output.push_str("- No HTTP routes call this handler\n");
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
        output.push_str("- No gRPC services implement this handler\n");
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