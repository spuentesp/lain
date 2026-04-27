//! Schema description for describe_schema tool
//!
//! Provides introspection of the graph schema for LLM consumption.

use crate::query::spec::QuerySpec;

/// Describe the current graph schema
pub fn describe_schema() -> SchemaDescription {
    SchemaDescription {
        node_types: vec![
            NodeTypeDesc {
                name: "Function".into(),
                description: "A function or method definition".into(),
                properties: vec!["name".into(), "path".into(), "signature".into()],
                labels: vec!["test".into(), "deprecated".into(), "async".into()],
            },
            NodeTypeDesc {
                name: "File".into(),
                description: "A source file".into(),
                properties: vec!["path".into(), "language".into()],
                labels: vec!["generated".into(), "test".into()],
            },
            NodeTypeDesc {
                name: "Module".into(),
                description: "A module or package".into(),
                properties: vec!["name".into(), "path".into()],
                labels: vec![],
            },
            NodeTypeDesc {
                name: "Class".into(),
                description: "A class or struct definition".into(),
                properties: vec!["name".into(), "path".into()],
                labels: vec!["test".into(), "deprecated".into()],
            },
            NodeTypeDesc {
                name: "Interface".into(),
                description: "An interface or trait definition".into(),
                properties: vec!["name".into(), "path".into()],
                labels: vec![],
            },
        ],
        edge_types: vec![
            EdgeTypeDesc {
                name: "Calls".into(),
                description: "Function calls another function".into(),
                source_types: vec!["Function".into()],
                target_types: vec!["Function".into()],
            },
            EdgeTypeDesc {
                name: "Uses".into(),
                description: "Code uses a value/entity".into(),
                source_types: vec!["Function".into(), "Class".into()],
                target_types: vec!["Function".into(), "Class".into(), "Interface".into()],
            },
            EdgeTypeDesc {
                name: "Import".into(),
                description: "File imports a module".into(),
                source_types: vec!["File".into()],
                target_types: vec!["Module".into()],
            },
            EdgeTypeDesc {
                name: "Defines".into(),
                description: "File/module defines a Function/Class".into(),
                source_types: vec!["File".into(), "Module".into()],
                target_types: vec!["Function".into(), "Class".into(), "Interface".into()],
            },
            EdgeTypeDesc {
                name: "Contains".into(),
                description: "Module contains other modules".into(),
                source_types: vec!["Module".into()],
                target_types: vec!["Module".into(), "Function".into()],
            },
            EdgeTypeDesc {
                name: "Inherits".into(),
                description: "Class inherits from another class".into(),
                source_types: vec!["Class".into()],
                target_types: vec!["Class".into(), "Interface".into()],
            },
            EdgeTypeDesc {
                name: "Implements".into(),
                description: "Class implements an interface".into(),
                source_types: vec!["Class".into()],
                target_types: vec!["Interface".into()],
            },
            EdgeTypeDesc {
                name: "TestedBy".into(),
                description: "Function is tested by a test function".into(),
                source_types: vec!["Function".into()],
                target_types: vec!["Function".into()],
            },
        ],
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
}
