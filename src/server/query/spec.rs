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
                    // `Defines` is not an EdgeType. File -> Symbol is
                    // `Contains`. The documented schema was purged of
                    // `Defines`/`Import`/`TestedBy` once; this prebuilt
                    // table kept them, so the named queries below shipped
                    // matching nothing.
                    edge: EdgeSelector::Single("Contains".into()),
                    direction: Direction::Outgoing,
                    depth: DepthSpec::Single(1),
                    target: Some(Box::new(FindOp {
                        type_selector: Some(TypeSelector::Single("Function".into())),
                        ..Default::default()
                    })),
                }),
            ]),
            // "get_function_imports" was removed. It connected over
            // `Import`, which is not an EdgeType at all; the closest real
            // one, `Imports`, has no producer in any indexer, so the query
            // could only ever return nothing.
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
            // "get_test_coverage" was removed. It connected over
            // `TestedBy`, which is not an EdgeType and has no equivalent —
            // nothing in the graph records a test-to-subject relationship.
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

    /// Every edge name this selector mentions, in any variant.
    pub fn named_types(&self) -> &[String] {
        match self {
            EdgeSelector::Single(s) => std::slice::from_ref(s),
            EdgeSelector::Or(types) | EdgeSelector::Not(types) => types,
        }
    }

    /// Names that are not real [`EdgeType`]s.
    ///
    /// Edge names are matched as raw strings against
    /// `edge.edge_type.to_string()`, so a name that does not exist simply
    /// never matches and the traversal returns `count: 0` — indistinguishable
    /// from a correct query over a part of the graph that happens to be
    /// empty. That is how `describe_schema`'s own `file_functions` example
    /// shipped connecting over `"Defines"`, an edge that has never existed:
    /// nothing ever failed, it just always answered nothing. In a `Not`
    /// selector a typo is worse than useless — it silently *widens* the
    /// match to every edge in the graph.
    pub fn unknown_types(&self) -> Vec<String> {
        let valid: std::collections::HashSet<String> = crate::server::schema::EdgeType::all()
            .iter()
            .map(|e| e.to_string())
            .collect();
        self.named_types()
            .iter()
            .filter(|n| !valid.contains(*n))
            .cloned()
            .collect()
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
    /// Set when the default result cap trimmed the answer. A query that
    /// specifies its own `limit` never sets this.
    ///
    /// The cap has to be visible: silently returning 100 of 1500 matches
    /// is indistinguishable from a codebase that only has 100, which is
    /// the same failure as an edge type nothing produces.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub truncated: bool,
    /// How many nodes matched before the default cap was applied. Only
    /// present when `truncated` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_before_limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryGroup {
    pub key: String,
    pub nodes: Vec<GraphNodeRef>,
    pub count: usize,
}

// `QueryExplanation` was only ever constructed by `Executor::explain`,
// which had no caller and no test. With that gone the type could not be
// produced by anything, so it went with it rather than staying as a
// re-exported shape no code path can return.

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

#[cfg(test)]
mod named_query_validity_tests {
    use super::*;
    use crate::server::schema::{EdgeType, NodeType};
    use std::collections::HashSet;

    /// Every prebuilt query must name real, actually-produced types.
    ///
    /// `describe_schema`'s type lists were rebuilt from the enums to stop
    /// them advertising `Defines`, `Import` and `TestedBy` — none of which
    /// exist. This table was not, and kept all three: `get_file_functions`
    /// connected over `Defines`, `get_function_imports` over `Import`, and
    /// `get_test_coverage` over `TestedBy`. Each shipped as a named,
    /// documented, prebuilt query that could only ever match nothing.
    #[test]
    fn every_named_query_uses_real_indexed_edges() {
        let names = [
            "get_blast_radius",
            "get_call_chain",
            "get_file_functions",
            "get_callers",
            "get_callees",
            "get_module_functions",
            "get_deprecated_functions",
        ];

        let valid: HashSet<String> = EdgeType::all().iter().map(|e| e.to_string()).collect();
        let indexed: HashSet<String> = EdgeType::all()
            .iter()
            .filter(|e| e.is_indexed())
            .map(|e| e.to_string())
            .collect();

        for name in names {
            let spec = QuerySpec::named(name)
                .unwrap_or_else(|| panic!("named query `{name}` should exist"));
            for op in &spec.ops {
                if let GraphOp::Connect(c) = op {
                    for edge in c.edge.named_types() {
                        assert!(
                            valid.contains(edge),
                            "named query `{name}` connects over `{edge}`, which is not an EdgeType"
                        );
                        assert!(
                            indexed.contains(edge),
                            "named query `{name}` connects over `{edge}`, which no indexer \
                             produces — it can only ever return nothing"
                        );
                    }
                }
            }
        }
    }

    /// Node types named by prebuilt queries must be real too.
    #[test]
    fn every_named_query_uses_real_node_types() {
        let valid: HashSet<String> = NodeType::all().iter().map(|t| t.to_string()).collect();
        for name in [
            "get_file_functions",
            "get_module_functions",
            "get_deprecated_functions",
        ] {
            let spec = QuerySpec::named(name).unwrap();
            let json = serde_json::to_value(&spec).unwrap();
            fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
                match v {
                    serde_json::Value::Object(m) => {
                        for (k, val) in m {
                            if k == "type" {
                                if let serde_json::Value::String(s) = val {
                                    out.push(s.clone());
                                }
                            }
                            walk(val, out);
                        }
                    }
                    serde_json::Value::Array(a) => a.iter().for_each(|i| walk(i, out)),
                    _ => {}
                }
            }
            let mut types = Vec::new();
            walk(&json, &mut types);
            for t in types {
                assert!(
                    valid.contains(&t),
                    "named query `{name}` uses node type `{t}`, which is not a NodeType"
                );
            }
        }
    }

    /// The three removed queries must stay gone rather than being
    /// reinstated pointing at the same fictional edges.
    #[test]
    fn the_queries_built_on_fictional_edges_are_not_reinstated() {
        for gone in ["get_function_imports", "get_test_coverage"] {
            assert!(
                QuerySpec::named(gone).is_none(),
                "`{gone}` connected over an edge that does not exist; if it is \
                 brought back it needs a real, produced edge behind it"
            );
        }
    }
}
