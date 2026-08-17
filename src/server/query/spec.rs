//! Query specification for graph operations
//!
//! JSON-based ops array interface for graph queries, designed for LLM-native construction.

use serde::de::Error as DeError;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::RangeInclusive;

// =============================================================================
// Query Mode & Configuration
// =============================================================================

/// Mode for query execution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryMode {
    /// Use the new ops-array style query
    Query,
    /// Delegate to legacy named tool handlers
    Tool,
    /// Auto-detect: try ops first, fallback to named
    Auto,
}

impl Default for QueryMode {
    fn default() -> Self {
        QueryMode::Auto
    }
}

/// Main query specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySpec {
    #[serde(default)]
    pub ops: Vec<GraphOp>,

    #[serde(default)]
    pub mode: QueryMode,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub named: Option<String>,
}

impl QuerySpec {
    pub fn new(ops: Vec<GraphOp>) -> Self {
        Self {
            ops,
            mode: QueryMode::Auto,
            named: None,
        }
    }

    /// Get a prebuilt query by name
    pub fn named(name: &str) -> Option<Self> {
        let spec = match name {
            "get_blast_radius" => QuerySpec::new(vec![
                GraphOp::Find(FindOp::default()),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Calls".into()),
                    direction: Direction::Outgoing,
                    depth: DepthSpec::Range { min: 1, max: 2 },
                    target: None,
                }),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Calls".into()),
                    direction: Direction::Incoming,
                    depth: DepthSpec::Range { min: 1, max: 2 },
                    target: None,
                }),
            ]),
            "get_call_chain" => QuerySpec::new(vec![
                GraphOp::Find(FindOp::default()),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Calls".into()),
                    direction: Direction::Outgoing,
                    depth: DepthSpec::Single(10),
                    target: None,
                }),
            ]),
            "get_file_functions" => QuerySpec::new(vec![
                GraphOp::Find(FindOp {
                    type_selector: Some(TypeSelector::Single("File".into())),
                    ..Default::default()
                }),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Defines".into()),
                    direction: Direction::Outgoing,
                    depth: DepthSpec::Single(1),
                    target: Some(Box::new(FindOp {
                        type_selector: Some(TypeSelector::Single("Function".into())),
                        ..Default::default()
                    })),
                }),
            ]),
            "get_function_imports" => QuerySpec::new(vec![
                GraphOp::Find(FindOp {
                    type_selector: Some(TypeSelector::Single("Function".into())),
                    ..Default::default()
                }),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Import".into()),
                    direction: Direction::Outgoing,
                    depth: DepthSpec::Single(1),
                    target: None,
                }),
            ]),
            "get_callers" => QuerySpec::new(vec![
                GraphOp::Find(FindOp::default()),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Calls".into()),
                    direction: Direction::Incoming,
                    depth: DepthSpec::Single(1),
                    target: None,
                }),
            ]),
            "get_callees" => QuerySpec::new(vec![
                GraphOp::Find(FindOp::default()),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Calls".into()),
                    direction: Direction::Outgoing,
                    depth: DepthSpec::Single(1),
                    target: None,
                }),
            ]),
            "get_module_functions" => QuerySpec::new(vec![
                GraphOp::Find(FindOp {
                    type_selector: Some(TypeSelector::Single("Module".into())),
                    ..Default::default()
                }),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("Contains".into()),
                    direction: Direction::Outgoing,
                    depth: DepthSpec::Single(2),
                    target: Some(Box::new(FindOp {
                        type_selector: Some(TypeSelector::Single("Function".into())),
                        ..Default::default()
                    })),
                }),
            ]),
            "get_test_coverage" => QuerySpec::new(vec![
                GraphOp::Find(FindOp {
                    type_selector: Some(TypeSelector::Single("Function".into())),
                    ..Default::default()
                }),
                GraphOp::Connect(ConnectOp {
                    edge: EdgeSelector::Single("TestedBy".into()),
                    direction: Direction::Incoming,
                    depth: DepthSpec::Single(1),
                    target: None,
                }),
            ]),
            "get_deprecated_functions" => QuerySpec::new(vec![GraphOp::Find(FindOp {
                type_selector: Some(TypeSelector::Single("Function".into())),
                label_selector: Some(LabelSelector::Single("deprecated".into())),
                ..Default::default()
            })]),
            _ => return None,
        };
        Some(spec)
    }
}

impl Default for QuerySpec {
    fn default() -> Self {
        Self::new(vec![])
    }
}

// =============================================================================
// Depth Specification
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DepthSpec {
    Single(u32),
    Range {
        #[serde(rename = "min")]
        min: u32,
        #[serde(rename = "max")]
        max: u32,
    },
}

impl DepthSpec {
    pub fn to_range(&self) -> RangeInclusive<u32> {
        match self {
            DepthSpec::Single(n) => *n..=*n,
            DepthSpec::Range { min, max } => *min..=*max,
        }
    }
}

impl Default for DepthSpec {
    fn default() -> Self {
        DepthSpec::Single(1)
    }
}

// =============================================================================
// Type & Label Selectors
// =============================================================================

/// Node type selector - supports single or OR
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TypeSelector {
    Single(String),
    Or(Vec<String>),
}

impl TypeSelector {
    pub fn matches(&self, node_type: &str) -> bool {
        match self {
            TypeSelector::Single(s) => s == node_type,
            TypeSelector::Or(types) => types.iter().any(|t| t == node_type),
        }
    }
}

/// Label selector - supports single, OR, or NOT
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LabelSelector {
    Single(String),
    Or(Vec<String>),
    Not(Vec<String>),
}

impl LabelSelector {
    pub fn matches(&self, node_label: Option<&str>) -> bool {
        match self {
            LabelSelector::Single(label) => node_label == Some(label),
            LabelSelector::Or(labels) => {
                let Some(l) = node_label else {
                    return false;
                };
                labels.iter().any(|label| label == l)
            }
            LabelSelector::Not(labels) => {
                let Some(l) = node_label else {
                    return true;
                };
                !labels.iter().any(|label| label == l)
            }
        }
    }
}

// =============================================================================
// Edge Selector
// =============================================================================

/// Edge selector - supports single, OR, or NOT
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EdgeSelector {
    Single(String),
    Or(Vec<String>),
    Not(Vec<String>),
}

impl EdgeSelector {
    pub fn matches(&self, edge_type: &str) -> bool {
        match self {
            EdgeSelector::Single(s) => s == edge_type,
            EdgeSelector::Or(types) => types.iter().any(|t| t == edge_type),
            EdgeSelector::Not(types) => !types.iter().any(|t| t == edge_type),
        }
    }
}

// =============================================================================
// Direction
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Outgoing,
    Incoming,
    Both,
}

impl Default for Direction {
    fn default() -> Self {
        Direction::Outgoing
    }
}

// =============================================================================
// Name Matching
// =============================================================================

/// Name matching strategy
#[derive(Debug, Clone)]
pub enum NameSelector {
    Exact(String),
    Glob(String),
    StartsWith(String),
    EndsWith(String),
}

impl Serialize for NameSelector {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            NameSelector::Exact(value) => serializer.serialize_str(value),
            NameSelector::Glob(value) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("glob", value)?;
                map.end()
            }
            NameSelector::StartsWith(value) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("starts_with", value)?;
                map.end()
            }
            NameSelector::EndsWith(value) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("ends_with", value)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for NameSelector {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(value) => Ok(NameSelector::Exact(value)),
            serde_json::Value::Object(object) if object.len() == 1 => {
                let (key, value) = object
                    .into_iter()
                    .next()
                    .ok_or_else(|| D::Error::custom("expected one name selector key"))?;
                let value = selector_string::<D::Error>(&key, value)?;
                match key.as_str() {
                    "exact" => Ok(NameSelector::Exact(value)),
                    "glob" => Ok(NameSelector::Glob(value)),
                    "starts_with" | "startsWith" => Ok(NameSelector::StartsWith(value)),
                    "ends_with" | "endsWith" => Ok(NameSelector::EndsWith(value)),
                    _ => Err(D::Error::custom(format!(
                        "unknown name selector `{key}`; expected exact, glob, starts_with, or ends_with"
                    ))),
                }
            }
            serde_json::Value::Object(_) => Err(D::Error::custom(
                "name selector object must contain exactly one key",
            )),
            _ => Err(D::Error::custom(
                "name selector must be a string or an object selector",
            )),
        }
    }
}

fn selector_string<E>(key: &str, value: serde_json::Value) -> Result<String, E>
where
    E: DeError,
{
    match value {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(E::custom(format!(
            "name selector `{key}` value must be a string"
        ))),
    }
}

impl NameSelector {
    pub fn matches(&self, name: &str) -> bool {
        match self {
            NameSelector::Exact(s) => name == s,
            NameSelector::StartsWith(s) => name.starts_with(s),
            NameSelector::EndsWith(s) => name.ends_with(s),
            NameSelector::Glob(pattern) => {
                let pattern = pattern.replace('*', ".*").replace('?', ".");
                regex::Regex::new(&format!("^{}$", pattern))
                    .map(|r| r.is_match(name))
                    .unwrap_or(false)
            }
        }
    }
}

// =============================================================================
// Find Operation
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindOp {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_selector: Option<TypeSelector>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<NameSelector>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(rename = "label", skip_serializing_if = "Option::is_none")]
    pub label_selector: Option<LabelSelector>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl FindOp {
    pub fn new() -> Self {
        Self {
            type_selector: None,
            name: None,
            id: None,
            label_selector: None,
            path: None,
        }
    }

    pub fn r#type(mut self, ty: impl Into<String>) -> Self {
        self.type_selector = Some(TypeSelector::Single(ty.into()));
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(NameSelector::Exact(name.into()));
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label_selector = Some(LabelSelector::Single(label.into()));
        self
    }
}

impl Default for FindOp {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Connect Operation
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectOp {
    pub edge: EdgeSelector,

    #[serde(default)]
    pub direction: Direction,

    #[serde(default)]
    pub depth: DepthSpec,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Box<FindOp>>,
}

impl Default for ConnectOp {
    fn default() -> Self {
        Self {
            edge: EdgeSelector::Single("Calls".into()),
            direction: Direction::Outgoing,
            depth: DepthSpec::Single(1),
            target: None,
        }
    }
}

// =============================================================================
// Filter Operation
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterOp {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_filter: Option<TypeSelector>,

    #[serde(rename = "label", skip_serializing_if = "Option::is_none")]
    pub label_filter: Option<LabelSelector>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<NameSelector>,
}

impl Default for FilterOp {
    fn default() -> Self {
        Self {
            type_filter: None,
            label_filter: None,
            name: None,
        }
    }
}

// =============================================================================
// Group Operation
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupBy {
    Type,
    Label,
    Name,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupOp {
    pub by: GroupBy,
}

impl Default for GroupOp {
    fn default() -> Self {
        Self { by: GroupBy::Type }
    }
}

// =============================================================================
// Sort Operation
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortField {
    Name,
    Type,
    Label,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Asc,
    Desc,
}

impl Default for SortDirection {
    fn default() -> Self {
        SortDirection::Asc
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SortOp {
    pub by: SortField,
    #[serde(default)]
    pub direction: SortDirection,
}

impl Default for SortOp {
    fn default() -> Self {
        Self {
            by: SortField::Name,
            direction: SortDirection::Asc,
        }
    }
}

// =============================================================================
// Limit Operation
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitOp {
    pub count: usize,
    #[serde(default)]
    pub offset: usize,
}

impl Default for LimitOp {
    fn default() -> Self {
        Self {
            count: 100,
            offset: 0,
        }
    }
}

// =============================================================================
// Semantic Filter Operation
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticFilterOp {
    /// The natural language query to match semantically
    pub like: String,

    /// Minimum similarity threshold (0.0 to 1.0), defaults to 0.3
    #[serde(default = "default_semantic_threshold")]
    pub threshold: f32,
}

fn default_semantic_threshold() -> f32 {
    0.3
}

impl Default for SemanticFilterOp {
    fn default() -> Self {
        Self {
            like: String::new(),
            threshold: 0.3,
        }
    }
}

// =============================================================================
// Graph Operations
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum GraphOp {
    Find(FindOp),
    Connect(ConnectOp),
    Filter(FilterOp),
    #[serde(rename = "semantic_filter")]
    SemanticFilter(SemanticFilterOp),
    Group(GroupOp),
    Sort(SortOp),
    Limit(LimitOp),
}

// =============================================================================
// Query Result Types
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<GraphNodeRef>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<GraphEdgeRef>,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<GraphPath>,

    pub count: usize,
    pub legacy: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<QueryMeta>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<QueryGroup>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeRef {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdgeRef {
    pub id: String,
    #[serde(rename = "type")]
    pub edge_type: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphPath {
    pub nodes: Vec<GraphNodeRef>,
    pub edges: Vec<GraphEdgeRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryMeta {
    pub exec_us: u64,
    pub nodes_visited: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryGroup {
    pub key: String,
    pub nodes: Vec<GraphNodeRef>,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryExplanation {
    pub plan: String,
    pub steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_selector_or() {
        let selector = TypeSelector::Or(vec!["Function".into(), "Method".into()]);
        assert!(selector.matches("Function"));
        assert!(selector.matches("Method"));
        assert!(!selector.matches("Class"));
    }

    #[test]
    fn test_label_selector_not() {
        let selector = LabelSelector::Not(vec!["test".into()]);
        assert!(selector.matches(None));
        assert!(!selector.matches(Some("test")));
        assert!(selector.matches(Some("deprecated")));
    }

    #[test]
    fn test_named_query() {
        let spec = QuerySpec::named("get_blast_radius").unwrap();
        assert!(!spec.ops.is_empty());
    }
}
