//! Query handler for graph operations
//!
//! Provides query_graph and describe_schema tools.

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::nlp::NlpEmbedder;
use crate::query::{Executor, QuerySpec};
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Execute a query against the graph using the ops array interface
pub fn query_graph(
    graph: &GraphDatabase,
    embedder: &NlpEmbedder,
    embedding_cache: &Arc<Mutex<HashMap<String, Vec<f32>>>>,
    arguments: Option<&Map<String, Value>>,
) -> Result<String, LainError> {
    let mut executor = Executor::new(graph, embedder, embedding_cache);

    // Parse query spec from arguments
    // Accepts both wrapped {"query": {...}} and unwrapped {...}
    let spec = if let Some(args) = arguments {
        if let Some(query_val) = args.get("query") {
            serde_json::from_value(query_val.clone()).map_err(|e| LainError::Json(e))?
        } else {
            // User provided unwrapped query spec directly as arguments
            serde_json::from_value(Value::Object(args.clone())).map_err(|e| LainError::Json(e))?
        }
    } else {
        QuerySpec::default()
    };

    let result = executor
        .execute(&spec)
        .map_err(|e| LainError::Graph(e.to_string()))?;

    serde_json::to_string_pretty(&result).map_err(|e| LainError::Json(e))
}

/// Describe the graph schema for LLM session initialization
pub fn describe_schema() -> Result<String, LainError> {
    let schema = crate::query::schema::describe_schema();
    serde_json::to_string_pretty(&schema).map_err(|e| LainError::Json(e))
}
