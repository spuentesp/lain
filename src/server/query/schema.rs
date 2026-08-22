//! Schema description for describe_schema tool
//!
//! Provides introspection of the graph schema for LLM consumption.

use crate::query::spec::QuerySpec;

/// Describe the current graph schema
pub fn describe_schema() -> SchemaDescription {
    SchemaDescription {
        // Built from `NodeType::all()` so the documented schema
        // cannot drift from what the indexer actually emits. It had:
        // five types were listed by hand while eighteen existed, and
        // `Method` — where most Rust code lives — was among the
        // missing, so schema-following queries silently returned
        // nothing.
        node_types: crate::server::schema::NodeType::all()
            .iter()
            .map(|t| NodeTypeDesc {
                name: t.to_string(),
                description: t.description().into(),
                properties: t.properties().iter().map(|p| (*p).to_string()).collect(),
                labels: t.labels().iter().map(|l| (*l).to_string()).collect(),
            })
            .collect(),
        // Built from `EdgeType::all()` for the same reason the node
        // list is: the hand-written version advertised four edge kinds
        // that do not exist and omitted eight that do, `CoChangedWith`
        // among them.
        edge_types: crate::server::schema::EdgeType::all()
            .iter()
            .map(|e| EdgeTypeDesc {
                name: e.to_string(),
                description: e.description().into(),
                source_types: e.source_types().iter().map(|t| t.to_string()).collect(),
                target_types: e.target_types().iter().map(|t| t.to_string()).collect(),
            })
            .collect(),
        examples: vec![
            ExampleQuery {
                name: "blast_radius".into(),
                description: "Find all functions that call or are called by foo, within 2 hops".into(),
                query: QuerySpec::new(vec![
                    crate::query::spec::GraphOp::Find(crate::query::spec::FindOp::new().r#type("Function").name("foo")),
                    crate::query::spec::GraphOp::Connect(crate::query::spec::ConnectOp {
                        edge: crate::query::spec::EdgeSelector::Single("Calls".into()),
                        direction: crate::query::spec::Direction::Outgoing,
                        depth: crate::query::spec::DepthSpec::Range { min: 1, max: 2 },
                        target: None,
                    }),
                ]),
            },
            ExampleQuery {
                name: "call_chain".into(),
                description: "Trace all functions called by foo".into(),
                query: QuerySpec::new(vec![
                    crate::query::spec::GraphOp::Find(crate::query::spec::FindOp::new().r#type("Function").name("foo")),
                    crate::query::spec::GraphOp::Connect(crate::query::spec::ConnectOp {
                        edge: crate::query::spec::EdgeSelector::Single("Calls".into()),
                        direction: crate::query::spec::Direction::Outgoing,
                        depth: crate::query::spec::DepthSpec::Range { min: 1, max: 10 },
                        target: None,
                    }),
                ]),
            },
            ExampleQuery {
                name: "callers".into(),
                description: "Find all functions that call foo".into(),
                query: QuerySpec::new(vec![
                    crate::query::spec::GraphOp::Find(crate::query::spec::FindOp::new().r#type("Function").name("foo")),
                    crate::query::spec::GraphOp::Connect(crate::query::spec::ConnectOp {
                        edge: crate::query::spec::EdgeSelector::Single("Calls".into()),
                        direction: crate::query::spec::Direction::Incoming,
                        depth: crate::query::spec::DepthSpec::Single(1),
                        target: None,
                    }),
                ]),
            },
            ExampleQuery {
                name: "file_functions".into(),
                description: "List all functions defined in a file".into(),
                query: QuerySpec::new(vec![
                    crate::query::spec::GraphOp::Find(crate::query::spec::FindOp::new().r#type("File").name("src/main.rs")),
                    crate::query::spec::GraphOp::Connect(crate::query::spec::ConnectOp {
                        edge: crate::query::spec::EdgeSelector::Single("Defines".into()),
                        direction: crate::query::spec::Direction::Outgoing,
                        depth: crate::query::spec::DepthSpec::Single(1),
                        target: Some(Box::new(crate::query::spec::FindOp::new().r#type("Function"))),
                    }),
                ]),
            },
            ExampleQuery {
                name: "deprecated_functions".into(),
                description: "Find all deprecated functions".into(),
                query: QuerySpec::new(vec![
                    crate::query::spec::GraphOp::Find(crate::query::spec::FindOp::new().r#type("Function").label("deprecated")),
                ]),
            },
        ],
    }
}

// =============================================================================
// Schema Types (mirrored from spec for use in schema description)
// =============================================================================

/// Schema description for describe_schema tool
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SchemaDescription {
    /// Available node types
    pub node_types: Vec<NodeTypeDesc>,
    /// Available edge types
    pub edge_types: Vec<EdgeTypeDesc>,
    /// Example queries
    pub examples: Vec<ExampleQuery>,
}

/// Description of a node type
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NodeTypeDesc {
    pub name: String,
    pub description: String,
    pub properties: Vec<String>,
    pub labels: Vec<String>,
}

/// Description of an edge type
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EdgeTypeDesc {
    pub name: String,
    pub description: String,
    pub source_types: Vec<String>,
    pub target_types: Vec<String>,
}

/// Example query for docs
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExampleQuery {
    pub name: String,
    pub description: String,
    pub query: QuerySpec,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_describe_schema() {
        let schema = describe_schema();
        assert!(!schema.node_types.is_empty());
        assert!(!schema.edge_types.is_empty());
        assert!(!schema.examples.is_empty());
    }

    /// The documented schema must cover every type the indexer can
    /// emit. It advertised five of eighteen, so an agent that followed
    /// `describe_schema` and queried `type: "Function"` for a method
    /// got `count: 0` while `find_anchors` ranked that same method
    /// first — two tools, opposite answers, in one session.
    #[test]
    fn describe_schema_covers_every_node_type() {
        use crate::server::schema::NodeType;
        let schema = describe_schema();
        let described: std::collections::HashSet<String> =
            schema.node_types.iter().map(|n| n.name.clone()).collect();
        for t in NodeType::all() {
            assert!(
                described.contains(&t.to_string()),
                "describe_schema omits {t}, which the graph can contain"
            );
        }
        assert!(
            described.contains("Method"),
            "Method is where most impl-language code lives; it must be documented"
        );
    }

    /// The edge list drifted the same way the node list did: four
    /// advertised kinds did not exist and eight real ones were missing,
    /// `CoChangedWith` among them — the temporal coupling lain
    /// advertises as a headline feature was absent from its own schema.
    #[test]
    fn describe_schema_covers_every_edge_type() {
        use crate::server::schema::EdgeType;
        let schema = describe_schema();
        let described: std::collections::HashSet<String> =
            schema.edge_types.iter().map(|e| e.name.clone()).collect();
        for e in EdgeType::all() {
            assert!(
                described.contains(&e.to_string()),
                "describe_schema omits {e}, which the graph can contain"
            );
        }
        assert_eq!(
            described.len(),
            EdgeType::all().len(),
            "describe_schema must not advertise edge types the graph has no variant for"
        );
    }

    /// Descriptions are what an LLM reads to pick a type; an empty one
    /// is worse than no entry.
    #[test]
    fn every_node_type_has_a_description() {
        use crate::server::schema::NodeType;
        for t in NodeType::all() {
            assert!(!t.description().is_empty(), "{t} has no description");
        }
        for e in crate::server::schema::EdgeType::all() {
            assert!(!e.description().is_empty(), "{e} has no description");
        }
    }
}
