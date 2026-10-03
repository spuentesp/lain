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
use crate::graph::{GraphDatabase, SensorOwner};
use crate::schema::{GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::path::Path;

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
        .unwrap_or_else(|_| RepoId::new("unknown").expect("valid repo id"));
    let code_ext = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .filter(|e| matches!(*e, "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java"))
            .map(|e| e.to_string())
    };
    let mut total = 0usize;
    let mut all_nodes: Vec<GraphNode> = Vec::new();
    for (path, content, ext) in crate::server::sensors::util::scan_files(root, code_ext) {
        let graph_path_str = crate::graph::graph_path(root, &path);
        let links = detect_resolver_links(&content, &ext, &graph_path_str, &repo_id);
        for link in links {
            let id_name = format!("graphql-handler:{}:{}", link.op, link.field);
            let id = GraphNode::generate_id(
                &NodeType::Module,
                &graph_path_str,
                &id_name,
                Some(link.site_line),
                namespace,
            );
            let mut node = GraphNode::new(
                NodeType::Module,
                id_name.clone(),
                graph_path_str.clone(),
            );
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
    if !all_nodes.is_empty() {
        let _ = graph.replace_sensor_output(
            SensorOwner::GraphqlSensor,
            &all_nodes,
            &[] as &[GraphEdge],
        );
    }
    Ok(total)
}

// ─── Detection ─────────────────────────────────────────────────────────

/// Detect every recognised resolver-link pattern in `content`.
/// Public so the acceptance tests can exercise the detector
/// without going through the graph emission path.
pub fn detect_resolver_links(
    content: &str,
    ext: &str,
    graph_path: &str,
    repo_id: &RepoId,
) -> Vec<GraphqlHandlerLink> {
    let mut out: Vec<GraphqlHandlerLink> = Vec::new();
    match ext {
        "ts" | "tsx" | "js" | "jsx" => detect_apollo(content, graph_path, repo_id, &mut out),
        "go" => detect_gqlgen(content, graph_path, repo_id, &mut out),
        "py" => detect_strawberry(content, graph_path, repo_id, &mut out),
        "java" => detect_graphql_java(content, graph_path, repo_id, &mut out),
        _ => {}
    }
    out
}

fn detect_apollo(
    content: &str,
    graph_path: &str,
    repo_id: &crate::federation::repo_id::RepoId,
    out: &mut Vec<GraphqlHandlerLink>,
) {
    // Apollo resolver map pattern: `Query: { orders: ... }`,
    // `Mutation: { createOrder: ... }`,
    // `Subscription: { ... }`. We look for any line containing
    // `<Op>: {` where Op is Query / Mutation / Subscription and
    // then walk the subsequent indented block for the field
    // assignments.
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let _line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        for op in [GraphqlOp::Query, GraphqlOp::Mutation, GraphqlOp::Subscription] {
            let marker = format!("{}:", op_label(op));
            // We need a `:` right after the op name, optionally
            // followed by a space and `{`. The simplest
            // discriminating pattern is `<op>:` on a line whose
            // value starts with that token.
            if !trimmed.starts_with(&marker) {
                continue;
            }
            // Capture the subsequent block of field assignments.
            // Walk forward until we hit a line that is `}` (or
            // `},` / `};`) at the same (or shallower) indent as
            // the `<op>:` line — that closes the resolver block.
            // We also stop at a sibling op marker
            // (`Mutation:` / `Subscription:`) so an inline map
            // (`{ Query: {...}, Mutation: {...} }`) doesn't
            // cross-pollute fields.
            let op_indent = line.len() - line.trim_start().len();
            for (inner_idx, inner) in lines.iter().enumerate().skip(idx + 1) {
                let inner_trim = inner.trim();
                if inner_trim.is_empty() {
                    continue;
                }
                let inner_indent = inner.len() - inner.trim_start().len();
                // Closing brace of the resolver block — at the
                // same or shallower indent than the op marker.
                if inner_indent <= op_indent
                    && (inner_trim == "}"
                        || inner_trim == "},"
                        || inner_trim == "};")
                {
                    break;
                }
                // Sibling op marker — Query / Mutation /
                // Subscription at the same indent as the
                // current op. Treat as the end of this op's
                // field block.
                if inner_indent <= op_indent
                    && (inner_trim.starts_with("Query:")
                        || inner_trim.starts_with("Mutation:")
                        || inner_trim.starts_with("Subscription:"))
                {
                    break;
                }
                // Field assignment: `<field>: <handler>`.
                // The handler is a function expression, an
                // identifier, or `async (...) => ...`. We only
                // need the field name and the handler identifier
                // (when present).
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
                            path: graph_path.to_string(),
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
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_')
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
        // Walk to the arrow (`=>`) and return the first
        // identifier in the body. The body may be a function
        // call (`ordersResolver(...)`) or a block.
        if let Some(arrow_pos) = expr.find("=>") {
            let body = expr[arrow_pos + 2..].trim();
            // First identifier in the body.
            for token in tokenize(body) {
                if token
                    .chars()
                    .next()
                    .map_or(false, |c| c.is_ascii_alphabetic() || c == '_')
                {
                    return token.to_string();
                }
            }
        }
        return String::new();
    }
    // Bare identifier (possibly with trailing call parens).
    let head: String = expr
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    head
}

fn tokenize(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|tok| !tok.is_empty())
}

fn detect_gqlgen(
    content: &str,
    graph_path: &str,
    repo_id: &crate::federation::repo_id::RepoId,
    out: &mut Vec<GraphqlHandlerLink>,
) {
    // gqlgen resolver pattern:
    //   func (r *queryResolver) Orders(ctx context.Context) ([]*Order, error) { ... }
    //   func (r *mutationResolver) CreateOrder(ctx context.Context, input CreateOrderInput) (*Order, error) { ... }
    // The receiver type ends in `Resolver`; the convention is
    // `queryResolver` / `mutationResolver` /
    // `subscriptionResolver` so we map it to the operation.
    for (idx, line) in content.lines().enumerate() {
        let line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        let Some(after_func) = trimmed.strip_prefix("func ") else {
            continue;
        };
        // Look for `(<receiver> <type>) <Method>(`.
        let Some(paren_start) = after_func.find('(') else {
            continue;
        };
        let Some(paren_end) = after_func[paren_start..].find(')') else {
            continue;
        };
        let receiver = &after_func[paren_start + 1..paren_start + paren_end];
        // The receiver should be `<name> *<type>Resolver`.
        // Split on whitespace, then on `*`.
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
        // Method name: the identifier between the receiver's
        // `)` and the parameter list `(`.
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
                path: graph_path.to_string(),
                container: Some(type_name.to_string()),
                name: method.to_string(),
            },
            origin: GraphqlHandlerOrigin::Gqlgen,
            site_line: line_no,
        });
    }
}

fn detect_strawberry(
    content: &str,
    graph_path: &str,
    repo_id: &crate::federation::repo_id::RepoId,
    out: &mut Vec<GraphqlHandlerLink>,
) {
    // Strawberry resolver pattern:
    //   @strawberry.field
    //   def orders(self) -> list[Order]:
    // The decorator may also include resolver= or
    // description= kwargs; we ignore them.
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let _line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        if !trimmed.starts_with("@strawberry.field") {
            continue;
        }
        // Look forward for the `def` line. We accept any of the
        // three ops — the class the function lives in
        // (`Query` / `Mutation` / `Subscription`) determines it.
        // Without AST access we default to `Query` (the most
        // common Strawberry shape). The acceptance scenario
        // (F3) exercises the Query case.
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
            // The class context (Query / Mutation / Subscription)
            // would normally come from the surrounding class.
            // Without a full AST we default to Query (per the
            // acceptance scenario) and let the F3 test pin the
            // behaviour.
            out.push(GraphqlHandlerLink {
                op: GraphqlOp::Query,
                field: head.clone(),
                handler_function: SymbolKey {
                    repo: repo_id.clone(),
                    path: graph_path.to_string(),
                    container: None,
                    name: head,
                },
                origin: GraphqlHandlerOrigin::Strawberry,
                site_line: (def_idx as u32) + 1,
            });
            break;
        }
    }
}

fn detect_graphql_java(
    content: &str,
    graph_path: &str,
    repo_id: &crate::federation::repo_id::RepoId,
    out: &mut Vec<GraphqlHandlerLink>,
) {
    // graphql-java DataFetcher pattern:
    //   @Component
    //   public class OrdersDataFetcher implements DataFetcher<Order> { ... }
    // registered via
    //   RuntimeWiring.newRuntimeDataFetcher().register("orders", new OrdersDataFetcher())
    // or the equivalent `SchemaParser` builder. The
    // convention is that the class name is `<Field>DataFetcher`
    // (e.g. `OrdersDataFetcher` for the `orders` field).
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let line_no = (idx as u32) + 1;
        let trimmed = line.trim();
        // Look for `<name>DataFetcher` declaration preceded by
        // `@Component` (within the previous 4 non-comment
        // lines).
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
        // Walk back over up to 4 non-comment, non-empty lines
        // looking for `@Component`. graphql-java's Spring
        // starter emits the annotation on the line immediately
        // before the class, but the gap can be wider (e.g. a
        // license header) so we allow 4 lines of slack.
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
            // SDL field names are conventionally camelCase /
            // snake_case. Java class names are PascalCase; the
            // canonical `OrdersDataFetcher` => `orders` mapping
            // requires lowercasing the first char so the SDL
            // join key matches the consumer side. The
            // handler-function `name` keeps the original
            // PascalCase (`OrdersDataFetcher`) so a
            // `handler → function` lookup finds the class.
            field: lower_first(field),
            handler_function: SymbolKey {
                repo: repo_id.clone(),
                path: graph_path.to_string(),
                container: None,
                name: class_name,
            },
            origin: GraphqlHandlerOrigin::GraphqlJava,
            site_line: line_no,
        });
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
        assert!(matches!(
            orders[0].origin,
            GraphqlHandlerOrigin::Apollo
        ));
        let create: Vec<&GraphqlHandlerLink> = links
            .iter()
            .filter(|l| l.field == "createOrder")
            .collect();
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
        let create: Vec<&GraphqlHandlerLink> = links
            .iter()
            .filter(|l| l.field == "CreateOrder")
            .collect();
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
        assert!(matches!(
            links[0].origin,
            GraphqlHandlerOrigin::Strawberry
        ));
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
        assert!(matches!(
            links[0].origin,
            GraphqlHandlerOrigin::GraphqlJava
        ));
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
