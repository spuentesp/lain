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
                indexed: t.is_indexed(),
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
                indexed: e.is_indexed(),
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
                        // `Defines` is not an EdgeType and never was. The
                        // node and edge lists above were rebuilt from the
                        // enums to stop exactly this drift, but the examples
                        // stayed hand-written, so `describe_schema` — the
                        // tool an agent calls to learn the query language —
                        // handed out a canned query that silently matched
                        // nothing. File -> Symbol is `Contains`.
                        edge: crate::query::spec::EdgeSelector::Single("Contains".into()),
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
    /// False when no indexer in this build emits this type — querying it
    /// will always return zero results, whatever the codebase contains.
    pub indexed: bool,
}

/// Description of an edge type
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EdgeTypeDesc {
    pub name: String,
    pub description: String,
    pub source_types: Vec<String>,
    pub target_types: Vec<String>,
    /// False when no indexer in this build emits this edge — traversing
    /// it will always return zero results.
    pub indexed: bool,
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

#[cfg(test)]
mod example_validity_tests {
    use super::*;
    use crate::server::schema::{EdgeType, NodeType};
    use std::collections::HashSet;

    /// Walk a serialized example and collect every value stored under
    /// `key`, at any depth.
    fn collect(value: &serde_json::Value, key: &str, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    if k == key {
                        match v {
                            serde_json::Value::String(s) => out.push(s.clone()),
                            serde_json::Value::Array(items) => {
                                for i in items {
                                    if let serde_json::Value::String(s) = i {
                                        out.push(s.clone());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    collect(v, key, out);
                }
            }
            serde_json::Value::Array(items) => {
                for i in items {
                    collect(i, key, out);
                }
            }
            _ => {}
        }
    }

    /// `node_types` and `edge_types` are derived from the enums, but the
    /// `examples` are hand-written — so they drifted independently. The
    /// `file_functions` example connected over `"Defines"`, an edge that
    /// has never existed, and `query_graph` answers an unknown edge with
    /// `count: 0` rather than an error. The result was a canned query,
    /// handed to an agent at session start by the very tool meant to
    /// teach it the schema, that silently matched nothing.
    #[test]
    fn every_edge_named_in_an_example_is_a_real_edge_type() {
        let valid: HashSet<String> = EdgeType::all().iter().map(|e| e.to_string()).collect();
        for ex in describe_schema().examples {
            let json = serde_json::to_value(&ex.query).expect("example serializes");
            let mut edges = Vec::new();
            collect(&json, "edge", &mut edges);
            for e in edges {
                assert!(
                    valid.contains(&e),
                    "example `{}` uses edge `{}`, which is not an EdgeType. Valid: {:?}",
                    ex.name,
                    e,
                    {
                        let mut v: Vec<_> = valid.iter().cloned().collect();
                        v.sort();
                        v
                    }
                );
            }
        }
    }

    #[test]
    fn every_type_named_in_an_example_is_a_real_node_type() {
        let valid: HashSet<String> = NodeType::all().iter().map(|t| t.to_string()).collect();
        for ex in describe_schema().examples {
            let json = serde_json::to_value(&ex.query).expect("example serializes");
            let mut types = Vec::new();
            collect(&json, "type", &mut types);
            for t in types {
                assert!(
                    valid.contains(&t),
                    "example `{}` uses node type `{}`, which is not a NodeType",
                    ex.name,
                    t
                );
            }
        }
    }
}

#[cfg(test)]
mod indexed_flag_tests {
    use crate::server::schema::{EdgeType, NodeType};

    /// Whether the ingest pipeline calls into `server::sensors`.
    ///
    /// This is the fact that decides whether `HttpRoute`, `CallsHttp` and
    /// `Implements` can ever appear in a real graph. The sensors are
    /// written and (now) compiled, but nothing in `ingest/` calls them, so
    /// the types they emit are unreachable in practice. Greppable, unlike
    /// "is this type constructed somewhere in the source" — which is true
    /// of every one of them and tells you nothing.
    fn sensors_are_wired_into_ingestion() -> bool {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                    out.push(p);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root.join("server").join("ingest"), &mut files);
        walk(&root.join("cli"), &mut files);

        files.iter().any(|p| {
            std::fs::read_to_string(p)
                .map(|t| {
                    t.lines()
                        .filter(|l| !l.trim_start().starts_with("//"))
                        .any(|l| {
                            l.contains("sensors::")
                                || l.contains("scan_workspace_routes")
                                || l.contains("enrich_with_openapi")
                                || l.contains("enrich_with_proto")
                        })
                })
                .unwrap_or(false)
        })
    }

    /// The sensor-emitted types must be advertised as available exactly
    /// when the sensors can actually run.
    ///
    /// Wiring `server::sensors` into the ingest pipeline is the one change
    /// that makes these reachable; when someone makes it, this test fails
    /// and says to flip the flags, so `describe_schema` cannot keep
    /// under-reporting a feature that now works.
    #[test]
    fn sensor_types_are_available_exactly_when_the_sensors_are_wired() {
        let wired = sensors_are_wired_into_ingestion();
        for (name, indexed) in [
            ("NodeType::HttpRoute", NodeType::HttpRoute.is_indexed()),
            ("EdgeType::CallsHttp", EdgeType::CallsHttp.is_indexed()),
            ("EdgeType::Implements", EdgeType::Implements.is_indexed()),
        ] {
            assert_eq!(
                indexed, wired,
                "{name}.is_indexed() == {indexed} but sensors wired == {wired}. \
                 These types are only reachable if the ingest pipeline calls \
                 `server::sensors`; keep the flag and the wiring in agreement."
            );
        }
    }

    /// The types with no producer anywhere in the codebase must stay
    /// marked unavailable until someone writes one. `Imports` in
    /// particular reads like a core relationship and has never been
    /// emitted by any indexer.
    #[test]
    fn the_known_fictions_stay_marked_unavailable() {
        for t in [NodeType::Topic, NodeType::Resource, NodeType::Schema] {
            assert!(!t.is_indexed(), "{t} has no producer in this codebase");
        }
        for e in [
            EdgeType::Imports,
            EdgeType::Produces,
            EdgeType::Consumes,
            EdgeType::DeployedTo,
        ] {
            assert!(!e.is_indexed(), "{e} has no producer in this codebase");
        }
    }

    /// The core structural types carry the whole product; if any of these
    /// is ever marked unavailable something has gone badly wrong.
    #[test]
    fn the_core_graph_types_stay_available() {
        for t in [
            NodeType::File,
            NodeType::Function,
            NodeType::Method,
            NodeType::Struct,
            NodeType::Trait,
        ] {
            assert!(t.is_indexed(), "{t} is core and must be advertised");
        }
        for e in [
            EdgeType::Contains,
            EdgeType::Calls,
            EdgeType::Uses,
            EdgeType::CoChangedWith,
        ] {
            assert!(e.is_indexed(), "{e} is core and must be advertised");
        }
    }
}
