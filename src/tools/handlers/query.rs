//! Query handler for graph operations
//!
//! Provides query_graph and describe_schema tools.

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::query::{Executor, QuerySpec};
use serde_json::{Map, Value};

/// Execute a query against the graph using the ops array interface
pub fn query_graph(graph: &GraphDatabase, arguments: Option<&Map<String, Value>>) -> Result<String, LainError> {
    let mut executor = Executor::new(graph);

    // Parse query spec from arguments
    let spec = if let Some(args) = arguments {
        if let Some(query_val) = args.get("query") {
            serde_json::from_value(query_val.clone())
                .map_err(|e| LainError::Json(e))?
        } else {
            QuerySpec::default()
        }
    } else {
        QuerySpec::default()
    };

    let result = executor.execute(&spec)
        .map_err(|e| LainError::Graph(e.to_string()))?;

    serde_json::to_string_pretty(&result)
        .map_err(|e| LainError::Json(e))
}

/// Describe the graph schema for LLM session initialization
pub fn describe_schema() -> Result<String, LainError> {
    let schema = crate::query::schema::describe_schema();
    serde_json::to_string_pretty(&schema)
        .map_err(|e| LainError::Json(e))
}
