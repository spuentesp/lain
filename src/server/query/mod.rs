//! Query module for Lain graph operations
//!
//! Provides a JSON-based ops array interface for graph queries,
//! designed for LLM-native query construction.
//!
//! # Structure
//!
//! - `spec.rs` - Query specification types and prebuilt queries
//! - `executor.rs` - Query execution against the graph database
//! - `schema.rs` - Schema description for describe_schema tool
//!
//! # Usage
//!
//! The main entry point is [`QuerySpec`](spec::QuerySpec):
//!
//! ```json
//! {
//!   "ops": [
//!     { "op": "find", "type": "Function", "name": "foo" },
//!     { "op": "connect", "edge": "Calls", "depth": { "min": 1, "max": 3 } },
//!     { "op": "filter", "label": "test" }
//!   ],
//!   "mode": "auto"
//! }
//! ```

pub mod spec;
pub mod executor;
pub mod schema;

#[cfg(test)]
mod spec_tests;
#[cfg(test)]
mod executor_tests;

pub use spec::{QuerySpec, GraphOp, QueryResult, QueryExplanation};
pub use executor::Executor;
pub use schema::describe_schema;
