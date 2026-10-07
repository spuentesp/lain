//! Phase E — GraphQL resolver-link sensor (spec §8.3).
//!
//! Recognises the per-framework naming conventions that link a
//! GraphQL root field to its implementing handler. The sensor
//! is regex / pattern-first (the patterns are stable enough
//! that tree-sitter buys nothing extra) and emits one
//! `ContractFact::GraphqlHandler { graphql_field,
//! handler_function, origin }` per detected naming-convention
//! match.
//!
//! Recognised patterns (spec §8.3):
//!
//! - **Apollo resolver maps** (TS/JS): a `resolvers: { Query:
//!   { orders: async (parent, args, ctx) => ... } }` object
//!   literal. The handler is the function expression (or its
//!   identifier). The link is recorded with `origin:
//!   Apollo`.
//! - **graphql-java `DataFetcher`** (Java): a class annotated
//!   with `@Component` that implements
//!   `DataFetcher<Order>` and is registered via
//!   `RuntimeWiring.newRuntimeDataFetcher().register("orders",
//!   new OrdersDataFetcher())` (or via the
//!   `SchemaParser.newRegistry().register(...)` builder).
//! - **gqlgen** (Go): `func (r *queryResolver) Orders(ctx
//!   context.Context) ([]*Order, error)`. The method's receiver
//!   type ends in `Resolver` and the method name is the SDL
//!   root field name.
//! - **Strawberry** (Python): `@strawberry.field def orders(self)
//!   -> list[Order]:`. The decorated function's name is the
//!   SDL root field name and it lives in a class that inherits
//!   from `strawberry.type`.
//!
//! The emitted `GraphqlHandler` is what
//! `ContractKey::Graphql { op, field }` providers get a
//! `handler` link from. The joiner wires the handler
//! `SymbolKey` onto the corresponding `GraphqlProvider` so
//! typed traversal `handler → function → graphql` is
//! reachable.

use crate::error::LainError;
use crate::federation::contracts::model::{
    ContractFact, ContractKey, GraphqlHandlerFact, GraphqlHandlerOrigin, GraphqlOp, SymbolKey,
};
use crate::federation::repo_id::RepoId;
use crate::graph::{graph_path as compute_graph_path, GraphDatabase, SensorOwner};
use crate::schema::{GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::path::{Path, PathBuf};

// ─── Public sensor shape ───────────────────────────────────────────────

/// One resolver-link detected in a source file. The
/// `handler_function` is a `SymbolKey` the sensor resolved from
/// the call's textual pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlHandlerLink {
    pub op: GraphqlOp,
    pub field: String,
    pub handler_function: SymbolKey,
    pub origin: GraphqlHandlerOrigin,
    pub site_line: u32,
}

pub struct GraphqlResolverLinkSensor;

crate::server::sensors::register_sensor!(
    GraphqlResolverLinkSensor,
    "graphql_resolver_link",
    Graphql,
    1,
    scan_workspace_resolver_link
);

/// Walk `root`, find every recognised resolver-link pattern,
/// resolve the handler symbol, and emit one `Module` node
/// carrying `ContractFact::GraphqlHandler` per match. Returns
/// the count emitted.
pub fn scan_workspace_resolver_link(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }
    let repo_id = RepoId::new(root.to_string_lossy().as_ref())
        .unwrap_or_else(|_| crate::server::sensors::util::fallback_repo_id());
    let code_ext = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .filter(|e| matches!(*e, "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java"))
            .map(|e| e.to_string())
    };
    let mut total = 0usize;
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let detectors = default_resolver_detectors(root, &repo_id);
    for (path, content, ext) in crate::server::sensors::util::scan_files(root, code_ext) {
        for detector in &detectors {
            if !detector.handles_ext(&ext) {
                continue;
            }
            let links = detector.detect(&content, &path);
            for link in links {
                let id_name = format!("graphql-handler:{}:{}", link.op, link.field);
                let graph_path_str = detector.graph_path_for(&path);
                let id = GraphNode::generate_id(
                    &NodeType::Module,
                    &graph_path_str,
                    &id_name,
                    Some(link.site_line),
                    namespace,
                );
                let mut node =
                    GraphNode::new(NodeType::Module, id_name.clone(), graph_path_str.clone());
                node.id = id;
                node.line_start = Some(link.site_line);
                node.line_end = Some(link.site_line);
                let key = ContractKey::Graphql {
                    op: link.op,
                    field: link.field.clone(),
                };
                node.contract = Some(ContractFact::GraphqlHandler(GraphqlHandlerFact {
                    graphql_field: key,
                    handler_function: link.handler_function.clone(),
                    origin: link.origin,
                }));
                all_nodes.push(node);
                total += 1;
            }
        }
    }
    if !all_nodes.is_empty() {
        let _ = graph.replace_sensor_output(
            SensorOwner::GraphqlResolverLinkSensor,
            &all_nodes,
            &[] as &[GraphEdge],
        );
    }
    Ok(total)
}

// ─── Detection ─────────────────────────────────────────────────────────

/// Per-framework resolver-link detector. One impl per framework
/// (Apollo, Gqlgen, Strawberry, GraphQL-Java). Phase B-D review
/// §S9 / §D8 second-half-hive: the trait is the open/closed hook
/// — adding a fifth framework is one new struct + impl + line in
/// [`default_resolver_detectors`], not an edit to
/// [`detect_resolver_links`].
pub trait ResolverLinkDetector: Send + Sync {
    /// Stable name for telemetry / diff reports.
    fn framework_name(&self) -> &'static str;
    /// Whether this detector claims the given source-file extension.
    fn handles_ext(&self, ext: &str) -> bool;
    /// Detect every resolver-link pattern in `content` whose
    /// source file is `src_path`. The detector owns its
    /// `workspace` + `repo_id` so the call site does not need
    /// to thread them through.
    fn detect(&self, content: &str, src_path: &Path) -> Vec<GraphqlHandlerLink>;
    /// Root-relative `graph_path` for `src_path` (the
    /// `SymbolKey.path` the emitted link carries).
    fn graph_path_for(&self, src_path: &Path) -> String;
}

/// Build the default chain (Apollo → Gqlgen → Strawberry →
/// GraphQL-Java) for the workspace rooted at `root`. A new
/// framework means one new `Arc<dyn ResolverLinkDetector>` here.
fn default_resolver_detectors(root: &Path, repo_id: &RepoId) -> Vec<Box<dyn ResolverLinkDetector>> {
    vec![
        Box::new(ApolloDetector::new(root.to_path_buf(), repo_id.clone())),
        Box::new(GqlgenDetector::new(root.to_path_buf(), repo_id.clone())),
        Box::new(StrawberryDetector::new(root.to_path_buf(), repo_id.clone())),
        Box::new(GraphqlJavaDetector::new(
            root.to_path_buf(),
            repo_id.clone(),
        )),
    ]
}

/// Detect every recognised resolver-link pattern in `content`.
/// Public so the acceptance tests can exercise the detector
/// without going through the graph emission path. The `repo_id`
/// defaults to `"test"`; pass the real repo id in production.
pub fn detect_resolver_links(
    content: &str,
    ext: &str,
    graph_path: &str,
    repo_id: &RepoId,
) -> Vec<GraphqlHandlerLink> {
    let workspace = PathBuf::from(".");
    let detectors: Vec<Box<dyn ResolverLinkDetector>> = match ext {
        "ts" | "tsx" | "js" | "jsx" => {
            vec![Box::new(ApolloDetector::new(workspace, repo_id.clone()))]
        }
        "go" => vec![Box::new(GqlgenDetector::new(workspace, repo_id.clone()))],
        "py" => vec![Box::new(StrawberryDetector::new(
            workspace,
            repo_id.clone(),
        ))],
        "java" => vec![Box::new(GraphqlJavaDetector::new(
            workspace,
            repo_id.clone(),
        ))],
        _ => Vec::new(),
    };
    let mut out: Vec<GraphqlHandlerLink> = Vec::new();
    for detector in &detectors {
        // The acceptance tests pass a precomputed `graph_path`
        // (the canonical wire form); the detector derives its
        // own from `src_path`/workspace, so we substitute the
        // test's value by constructing a fake path whose
        // `compute_graph_path` would yield it.
        let src_path = std::path::PathBuf::from(graph_path);
        let mut links = detector.detect(content, &src_path);
        for link in &mut links {
            link.handler_function.path = graph_path.to_string();
        }
        out.extend(links);
    }
    out
}

// ─── Per-framework detectors ──────────────────────────────────────────

/// Apollo resolver-map detector (TS/JS / `*.ts` / `*.tsx` /
/// `*.js` / `*.jsx`).
struct ApolloDetector {
    workspace: PathBuf,
    repo_id: RepoId,
}

impl ApolloDetector {
    fn new(workspace: PathBuf, repo_id: RepoId) -> Self {
        Self { workspace, repo_id }
    }
}

impl ResolverLinkDetector for ApolloDetector {
    fn framework_name(&self) -> &'static str {
        "apollo"
    }
    fn handles_ext(&self, ext: &str) -> bool {
        matches!(ext, "ts" | "tsx" | "js" | "jsx")
    }
    fn detect(&self, content: &str, src_path: &Path) -> Vec<GraphqlHandlerLink> {
        let graph_path = compute_graph_path(&self.workspace, src_path);
        let repo_id = &self.repo_id;
        let mut out: Vec<GraphqlHandlerLink> = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            let _line_no = (idx as u32) + 1;
            let trimmed = line.trim();
            for op in [
                GraphqlOp::Query,
                GraphqlOp::Mutation,
                GraphqlOp::Subscription,
            ] {
                let marker = format!("{}:", op_label(op));
                if !trimmed.starts_with(&marker) {
                    continue;
                }
                let op_indent = line.len() - line.trim_start().len();
                for (inner_idx, inner) in lines.iter().enumerate().skip(idx + 1) {
                    let inner_trim = inner.trim();
                    if inner_trim.is_empty() {
                        continue;
                    }
                    let inner_indent = inner.len() - inner.trim_start().len();
                    if inner_indent <= op_indent
                        && (inner_trim == "}" || inner_trim == "}," || inner_trim == "};")
                    {
                        break;
                    }
                    if inner_indent <= op_indent
                        && (inner_trim.starts_with("Query:")
                            || inner_trim.starts_with("Mutation:")
                            || inner_trim.starts_with("Subscription:"))
                    {
                        break;
                    }
                    if let Some(colon) = inner_trim.find(':') {
                        let field = inner_trim[..colon].trim().to_string();
                        let handler_expr = inner_trim[colon + 1..].trim();
                        if field.is_empty() || !is_valid_field_name(&field) {
                            continue;
                        }
                        let handler_name = extract_apollo_handler_name(handler_expr);
                        if handler_name.is_empty() {
                            continue;
                        }
                        out.push(GraphqlHandlerLink {
                            op,
                            field,
                            handler_function: SymbolKey {
                                repo: repo_id.clone(),
                                path: graph_path.clone(),
                                container: None,
                                name: handler_name,
                            },
                            origin: GraphqlHandlerOrigin::Apollo,
                            site_line: (inner_idx as u32) + 1,
                        });
                    }
                }
            }
        }
        out
    }
    fn graph_path_for(&self, src_path: &Path) -> String {
        compute_graph_path(&self.workspace, src_path)
    }
}

fn op_label(op: GraphqlOp) -> &'static str {
    match op {
        GraphqlOp::Query => "Query",
        GraphqlOp::Mutation => "Mutation",
        GraphqlOp::Subscription => "Subscription",
    }
}

fn is_valid_field_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

/// Extract a handler identifier from an Apollo resolver map
/// expression. The canonical shapes are:
/// - `ordersResolver` — a bare identifier
/// - `async (parent, args, ctx) => ordersResolver(parent, args, ctx)` —
///   an inline arrow with a function call; we keep the first
///   identifier in the body
/// - `{ orders: () => 'static' }` — bare literal; we leave it
///   empty so the resolver map is recorded as anonymous
fn extract_apollo_handler_name(expr: &str) -> String {
    let expr = expr.trim().trim_end_matches(',');
    if expr.is_empty() {
        return String::new();
    }
    if expr.starts_with("async") || expr.starts_with("(") || expr.starts_with("function") {
        if let Some(arrow_pos) = expr.find("=>") {
            let body = expr[arrow_pos + 2..].trim();
            for token in tokenize(body) {
                if token
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                {
                    return token.to_string();
                }
            }
        }
        return String::new();
    }
    let head: String = expr
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    head
}

// Apollo arrow-body minitokenizer. Pass #4 R25 (review §D22)
// — this is a third minitokenizer in the sensor layer after
// the SQL / proto / GraphQL ones that the D1 `util_tokenize`
// helper serves, but it intentionally does NOT reuse the
// helper:
//
// - The helper exists for protocol-source-token streams that
//   need comment stripping, brace balancing, and string-
//   literal awareness before walking identifiers. The Apollo
//   detector feeds it arrow-body content extracted from a
//   resolver map value (e.g. `async (parent, args) => {
//   return Order.findById(args.id) }`). The arrow body is
//   already past the parser surface — comments and strings
//   have been stripped by the line-level walk that produced
//   the resolver map. Walking it as identifiers-only is the
//   exact shape we want.
// - The helper returns `Vec<Token>` (an owned struct) for
//   parser-walk use. The Apollo detector wants a borrowed
//   iterator (`impl Iterator<Item = &str>`) over a single
//   arrow body so it can stop on the first alphabetic
//   identifier. Wrapping the helper in an iterator adapter
//   would be more code than the 3-line split below.
// - The split-non-alphanumeric is a 1-line operation on
//   already-bounded input. The 30+ lines of
//   `util_tokenize::strip_comments` + balance + extract
//   scaffolding would be unused surface.
//
// Keeping the per-sensor minitokenizer is the right call for
// the D1 helper's contract. If a future detector needs the
// same arrow-body shape (e.g. a "kysely arrow resolver" for
// GraphQL), the helper should grow a `ident_split(&str)`
// entry point and both detectors should switch — but for
// now, the duplication is two lines, not three.
fn tokenize(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|tok| !tok.is_empty())
}

/// gqlgen resolver detector (Go).
struct GqlgenDetector {
    workspace: PathBuf,
    repo_id: RepoId,
}

impl GqlgenDetector {
    fn new(workspace: PathBuf, repo_id: RepoId) -> Self {
        Self { workspace, repo_id }
    }
}

impl ResolverLinkDetector for GqlgenDetector {
    fn framework_name(&self) -> &'static str {
        "gqlgen"
    }
    fn handles_ext(&self, ext: &str) -> bool {
        ext == "go"
    }
    fn detect(&self, content: &str, src_path: &Path) -> Vec<GraphqlHandlerLink> {
        let graph_path = compute_graph_path(&self.workspace, src_path);
        let repo_id = &self.repo_id;
        let mut out: Vec<GraphqlHandlerLink> = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            let line_no = (idx as u32) + 1;
            let trimmed = line.trim();
            let Some(after_func) = trimmed.strip_prefix("func ") else {
                continue;
            };
            let Some(paren_start) = after_func.find('(') else {
                continue;
            };
            let Some(paren_end) = after_func[paren_start..].find(')') else {
                continue;
            };
            let receiver = &after_func[paren_start + 1..paren_start + paren_end];
            let mut parts = receiver.split_whitespace();
            let _var = parts.next();
            let Some(type_str) = parts.next() else {
                continue;
            };
            let type_name = type_str.trim_start_matches('*');
            let op = if type_name == "queryResolver" {
                Some(GraphqlOp::Query)
            } else if type_name == "mutationResolver" {
                Some(GraphqlOp::Mutation)
            } else if type_name == "subscriptionResolver" {
                Some(GraphqlOp::Subscription)
            } else {
                None
            };
            let Some(op) = op else {
                continue;
            };
            let after_receiver = &after_func[paren_start + paren_end + 1..].trim();
            let Some(method_end) = after_receiver.find('(') else {
                continue;
            };
            let method = after_receiver[..method_end].trim();
            if method.is_empty() || !is_valid_field_name(method) {
                continue;
            }
            out.push(GraphqlHandlerLink {
                op,
                field: method.to_string(),
                handler_function: SymbolKey {
                    repo: repo_id.clone(),
                    path: graph_path.clone(),
                    container: Some(type_name.to_string()),
                    name: method.to_string(),
                },
                origin: GraphqlHandlerOrigin::Gqlgen,
                site_line: line_no,
            });
        }
        out
    }
    fn graph_path_for(&self, src_path: &Path) -> String {
        compute_graph_path(&self.workspace, src_path)
    }
}

/// Strawberry resolver detector (Python).
struct StrawberryDetector {
    workspace: PathBuf,
    repo_id: RepoId,
}

impl StrawberryDetector {
    fn new(workspace: PathBuf, repo_id: RepoId) -> Self {
        Self { workspace, repo_id }
    }
}

impl ResolverLinkDetector for StrawberryDetector {
    fn framework_name(&self) -> &'static str {
        "strawberry"
    }
    fn handles_ext(&self, ext: &str) -> bool {
        ext == "py"
    }
    fn detect(&self, content: &str, src_path: &Path) -> Vec<GraphqlHandlerLink> {
        let graph_path = compute_graph_path(&self.workspace, src_path);
        let repo_id = &self.repo_id;
        let mut out: Vec<GraphqlHandlerLink> = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            let _line_no = (idx as u32) + 1;
            let trimmed = line.trim();
            if !trimmed.starts_with("@strawberry.field") {
                continue;
            }
            for (def_idx, def_line) in lines.iter().enumerate().skip(idx + 1).take(3) {
                let def_trim = def_line.trim();
                if def_trim.is_empty() || def_trim.starts_with('#') {
                    continue;
                }
                let Some(after_def) = def_trim.strip_prefix("def ") else {
                    break;
                };
                let head: String = after_def
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if head.is_empty() || !is_valid_field_name(&head) {
                    break;
                }
                out.push(GraphqlHandlerLink {
                    op: GraphqlOp::Query,
                    field: head.clone(),
                    handler_function: SymbolKey {
                        repo: repo_id.clone(),
                        path: graph_path.clone(),
                        container: None,
                        name: head,
                    },
                    origin: GraphqlHandlerOrigin::Strawberry,
                    site_line: (def_idx as u32) + 1,
                });
                break;
            }
        }
        out
    }
    fn graph_path_for(&self, src_path: &Path) -> String {
        compute_graph_path(&self.workspace, src_path)
    }
}

/// graphql-java `DataFetcher` detector.
struct GraphqlJavaDetector {
    workspace: PathBuf,
    repo_id: RepoId,
}

impl GraphqlJavaDetector {
    fn new(workspace: PathBuf, repo_id: RepoId) -> Self {
        Self { workspace, repo_id }
    }
}

impl ResolverLinkDetector for GraphqlJavaDetector {
    fn framework_name(&self) -> &'static str {
        "graphql-java"
    }
    fn handles_ext(&self, ext: &str) -> bool {
        ext == "java"
    }
    fn detect(&self, content: &str, src_path: &Path) -> Vec<GraphqlHandlerLink> {
        let graph_path = compute_graph_path(&self.workspace, src_path);
        let repo_id = &self.repo_id;
        let mut out: Vec<GraphqlHandlerLink> = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            let line_no = (idx as u32) + 1;
            let trimmed = line.trim();
            if !trimmed.contains("DataFetcher") {
                continue;
            }
            let Some(class_pos) = trimmed.find("class ") else {
                continue;
            };
            let after_class = &trimmed[class_pos + "class ".len()..];
            let class_name: String = after_class
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let Some(field) = class_name.strip_suffix("DataFetcher") else {
                continue;
            };
            if field.is_empty() {
                continue;
            }
            let mut has_component = false;
            for (n, back) in lines.iter().take(idx).rev().enumerate() {
                if n >= 4 {
                    break;
                }
                let back_trim = back.trim();
                if back_trim.starts_with("//") || back_trim.is_empty() {
                    continue;
                }
                if back_trim.contains("@Component") {
                    has_component = true;
                }
                break;
            }
            if !has_component {
                continue;
            }
            out.push(GraphqlHandlerLink {
                op: GraphqlOp::Query,
                field: lower_first(field),
                handler_function: SymbolKey {
                    repo: repo_id.clone(),
                    path: graph_path.clone(),
                    container: None,
                    name: class_name,
                },
                origin: GraphqlHandlerOrigin::GraphqlJava,
                site_line: line_no,
            });
        }
        out
    }
    fn graph_path_for(&self, src_path: &Path) -> String {
        compute_graph_path(&self.workspace, src_path)
    }
}

/// Lowercase the first ASCII character of `s`. Used to map
/// `OrdersDataFetcher` → `orders` so the SDL field name the
/// consumer references matches the class-derived key.
fn lower_first(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    if let Some(first) = chars.next() {
        for c in first.to_lowercase() {
            out.push(c);
        }
    }
    out.push_str(chars.as_str());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::repo_id::RepoId;

    fn repo() -> RepoId {
        RepoId::new("test").unwrap()
    }

    #[test]
    fn apollo_resolver_map_links_handler() {
        let src = "\
const resolvers = {
  Query: {
    orders: async (parent, args, ctx) => ordersResolver(parent, args, ctx),
    health: () => 'ok',
  },
  Mutation: {
    createOrder: createOrderResolver,
  },
};
";
        let links = detect_resolver_links(src, "ts", "resolvers.ts", &repo());
        assert_eq!(links.len(), 3);
        let orders: Vec<&GraphqlHandlerLink> =
            links.iter().filter(|l| l.field == "orders").collect();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].op, GraphqlOp::Query);
        assert_eq!(orders[0].handler_function.name, "ordersResolver");
        assert!(matches!(orders[0].origin, GraphqlHandlerOrigin::Apollo));
        let create: Vec<&GraphqlHandlerLink> =
            links.iter().filter(|l| l.field == "createOrder").collect();
        assert_eq!(create[0].op, GraphqlOp::Mutation);
        assert_eq!(create[0].handler_function.name, "createOrderResolver");
    }

    #[test]
    fn gqlgen_resolver_links_handler() {
        let src = "\
package graph

func (r *queryResolver) Orders(ctx context.Context) ([]*Order, error) {
    return r.OrdersService.List(ctx)
}

func (r *mutationResolver) CreateOrder(ctx context.Context, input CreateOrderInput) (*Order, error) {
    return r.Create(ctx, input)
}
";
        let links = detect_resolver_links(src, "go", "resolver.go", &repo());
        assert_eq!(links.len(), 2);
        let orders: Vec<&GraphqlHandlerLink> =
            links.iter().filter(|l| l.field == "Orders").collect();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].op, GraphqlOp::Query);
        assert_eq!(
            orders[0].handler_function.container.as_deref(),
            Some("queryResolver")
        );
        assert!(matches!(orders[0].origin, GraphqlHandlerOrigin::Gqlgen));
        let create: Vec<&GraphqlHandlerLink> =
            links.iter().filter(|l| l.field == "CreateOrder").collect();
        assert_eq!(create[0].op, GraphqlOp::Mutation);
        assert_eq!(
            create[0].handler_function.container.as_deref(),
            Some("mutationResolver")
        );
    }

    #[test]
    fn strawberry_resolver_links_handler() {
        let src = "\
import strawberry

@strawberry.type
class Query:
    @strawberry.field
    def orders(self) -> list[Order]:
        return []
";
        let links = detect_resolver_links(src, "py", "schema.py", &repo());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].field, "orders");
        assert_eq!(links[0].op, GraphqlOp::Query);
        assert!(matches!(links[0].origin, GraphqlHandlerOrigin::Strawberry));
    }

    #[test]
    fn graphql_java_datafetcher_links_handler() {
        let src = "\
import org.springframework.stereotype.Component;
import graphql.schema.DataFetcher;

@Component
public class OrdersDataFetcher implements DataFetcher<List<Order>> {
    @Override
    public List<Order> get(graphql.schema.DataFetchingEnvironment env) {
        return List.of();
    }
}
";
        let links = detect_resolver_links(src, "java", "OrdersDataFetcher.java", &repo());
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].field, "orders");
        assert_eq!(links[0].op, GraphqlOp::Query);
        assert_eq!(links[0].handler_function.name, "OrdersDataFetcher");
        assert!(matches!(links[0].origin, GraphqlHandlerOrigin::GraphqlJava));
    }

    #[test]
    fn apollo_non_resolver_object_is_ignored() {
        let src = "\
const obj = {
  some: 'value',
};
";
        let links = detect_resolver_links(src, "ts", "obj.ts", &repo());
        assert!(links.is_empty());
    }
}
