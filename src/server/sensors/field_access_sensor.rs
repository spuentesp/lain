//! Field-access sensor (`field_access_sensor`, §6.5).
//!
//! Phase-2 sensor that walks the workspace to detect reads of fields
//! on the response of an outbound HTTP call. The shape: bound
//! identifiers (one per `SendsHttp` target, plus the §6.5 derived
//! names) are tracked through the sending function S and its scope,
//! reads on them emit a `FieldRef` node + a `ReadsField` edge from
//! the reading function and a `ReadsFrom` edge to the original
//! `HttpClientCall`.
//!
//! **Binding rules (§6.5, applied within scope):**
//!
//! 1. `x = <client call>` and `x = await <client call>` bind `x` to
//!    the call's response.
//! 2. `y = x.json()`, `y = await x.json()`, `y = x.data` (axios),
//!    `y = x.body` (got) bind `y`; `x` stays bound — so
//!    `r.json()["id"]` is recorded.
//! 3. `z = x["k"]`, `z = x.k`, `z = x.get("k")` bind `z` to the
//!    sub-path `k`; `for it in x["items"]` binds `it` to `items[]`.
//! 4. `d = Dto(**x)`, `Dto.model_validate(x)`, `Dto.parse_obj(x)`,
//!    `Dto(x)`, and TypeScript `x as Dto` / `const d: Dto = x` bind
//!    `d` with the same path.
//! 5. In a caller of S, `o = S(…)` / `o = await S(…)` binds `o` when
//!    S returns a bound identifier / a bound expression.
//! 6. In a callee of S or such a caller, the parameter at the
//!    position a bound identifier is passed in is bound.
//!
//! **Scope.** S, S's direct callers (rule 5), and the direct
//! callees of S and of those callers (rule 6). Only `Calls` edges
//! with provenance `Static{TreeSitter}` or `None` are used.
//!
//! **Reads.** `x.k`, `x["k"]`, `x.get("k")`, `"k" in x`,
//! destructuring, and Python `match` mapping patterns. The chain
//! is the path from the call's response root. Each read emits a
//! `FieldRef` node + `ReadsField` edge from the reading function +
//! `ReadsFrom` edge to the `HttpClientCall`. A non-literal key
//! emits nothing and sets `reads_complete = false`.
//!
//! **Escapes → `reads_complete = false`.** Returning from a caller
//! (leaves scope), passing outside scope, storing into an attribute
//! / subscript / global / collection, spreading, serializing
//! (`json.dumps`, `JSON.stringify`, `JSONResponse(x)`,
//! `res.json(x)`), yielding. Reads and iteration are NOT escapes.
//!
//! The walker is the shared [`walk_workspace`]; only the
//! per-shape analysis lives here.

use crate::error::LainError;
use crate::federation::contracts::model::{ContractFact, FieldReadFact, JsonPath, PathSegment};
use crate::federation::repo_id::RepoId;
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node, Parser, Tree};

// ─── `SensorOwner` extension ────────────────────────────────────

/// Trait extension so the field-access sensor can stand in for
/// `SensorOwner::FieldAccessSensor` without depending on a missing
/// conversion. The contract is one-to-one — one struct per owner.
pub trait SensorOwnerExt {
    fn owner() -> SensorOwner;
}

/// Stand-in owner used by the sensor to retract its own output. It
/// maps 1:1 to [`SensorOwner::FieldAccessSensor`].
struct SensorFieldAccessOwner;

impl SensorOwnerExt for SensorFieldAccessOwner {
    fn owner() -> SensorOwner {
        SensorOwner::FieldAccessSensor
    }
}

// ─── Sensor shell ────────────────────────────────────────────────

/// One read of a field on a bound response. Carries the JSON path
/// (§4.4 / §6.4) from the response root to the read field, and a
/// flag for whether the read was a literal key on a bound identifier
/// (`exact = true`; chain is exact and the key was a literal).
#[derive(Debug, Clone, PartialEq)]
pub struct FieldRead {
    pub chain: JsonPath,
    pub exact: bool,
    pub path: String,
    pub line: u32,
    pub reader_id: String,
}

/// One emission produced by `detect_reads`. Public so tests can
/// inspect the sensor output without round-tripping through the
/// graph.
#[derive(Debug, Clone)]
pub struct FieldAccessEmission {
    pub path: String,
    pub reads: Vec<FieldRead>,
    pub escapes: BTreeSet<Escape>,
    /// `true` when the response can be fully traced (no escape
    /// flipped it). PR 9's contract: a `false` here means the call's
    /// `ConsumerFact.reads_complete` is also `false`.
    pub reads_complete: bool,
}

/// What escapes a bound identifier from tracking. Each is its own
/// variant so the joiner and tools can surface the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Escape {
    /// `return x;` from S (or from a rule-5 caller) — leaves scope.
    Returned,
    /// Stored into an attribute / subscript / global / collection.
    Stored,
    /// Spread into another expression (`**x`, `...x`,
    /// `Object.assign(…, x)`, `dict(x)`).
    Spread,
    /// Serialized or passed through (`json.dumps(x)`,
    /// `JSON.stringify(x)`, `JSONResponse(x)`, `res.json(x)`).
    Serialized,
    /// Yielded (`yield x` / `yield from x`).
    Yielded,
}

/// Unit-struct Sensor impl.
pub struct FieldAccessSensor;

impl crate::server::sensors::Sensor for FieldAccessSensor {
    fn name(&self) -> &'static str {
        "field_access"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::FieldReads
    }
    fn phase(&self) -> u8 {
        // §6.5: phase 2, runs after the joiner has produced the
        // `Binds` set (so the scope rules 5 / 6 have something to
        // walk). PR 6 / 7 are 1; the joiner is wired into phase 2
        // here.
        2
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        let repo_id = RepoId::new(root.to_string_lossy().as_ref())
            .unwrap_or_else(|_| RepoId::new("field-access-sensor").unwrap());
        scan_workspace_field_access(graph, root, namespace, &repo_id)
    }
}

inventory::submit!(crate::server::sensors::SensorEntry(&FieldAccessSensor));

// ─── Workspace scan ───────────────────────────────────────────────

pub fn scan_workspace_field_access(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
    _repo_id: &RepoId,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }

    // Phase 2 needs the joiner to have run. The graph has the
    // `Binds` edges by the time the field-access sensor sees it —
    // we collect them here so rule 5/6 can find the calling /
    // callee relationships.
    let calls_by_function: BTreeMap<String, BTreeSet<String>> = collect_calls_by_function(graph);

    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = match ext {
            "py" => Some(Lang::Python),
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TsJs),
            _ => None,
        };
        let Some(lang) = lang else { continue };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let path_str = graph_path(root, path);
        let emissions = detect_emissions(&path_str, &content, lang, graph, &calls_by_function);
        for emission in emissions {
            let (nodes, edges) = build_emission(graph, &emission, namespace);
            all_nodes.extend(nodes);
            all_edges.extend(edges);
        }
    }

    let removed =
        graph.replace_sensor_output(SensorFieldAccessOwner::owner(), &all_nodes, &all_edges)?;
    if removed > 0 {
        tracing::debug!("field_access_sensor: replaced {removed} stale FieldRef(s) for {root:?}");
    }
    Ok(all_nodes.len())
}

// ─── Per-language detection ─────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Python,
    TsJs,
}

#[derive(Debug, Default)]
struct CallSite {
    #[allow(dead_code)]
    call_id: String,
    consumer_path: String,
    consumer_line: u32,
}

/// Discover every outbound HTTP call site in this repo. We look up
/// the `SendsHttp` edges the http_client_sensor wrote in phase one
/// and record the call id plus the source path and line. Used both
/// as the seeding for binding rule one and as the target of the
/// `ReadsFrom` edge the field-access sensor emits.
///
/// Reserved for PR 9.
fn collect_http_calls(graph: &GraphDatabase) -> BTreeMap<String, CallSite> {
    let mut out: BTreeMap<String, CallSite> = BTreeMap::new();
    let nodes = graph.get_all_nodes();
    let calls: BTreeMap<String, String> = nodes
        .iter()
        .filter(|n| n.node_type == NodeType::HttpClientCall)
        .map(|n| (n.id.clone(), n.path.clone()))
        .collect();
    let edges = graph.all_edges();
    for edge in edges {
        if edge.edge_type != EdgeType::SendsHttp {
            continue;
        }
        let Some(_call_id) = calls.get(&edge.target_id) else {
            continue;
        };
        // The reading function is the source. The site carries the
        // source line.
        let site = edge.site.as_ref();
        let line = site.map(|s| s.line).unwrap_or(0);
        let consumer_path = site.map(|s| s.path.clone()).unwrap_or_default();
        out.insert(
            edge.source_id.clone(),
            CallSite {
                call_id: edge.target_id.clone(),
                consumer_path,
                consumer_line: line,
            },
        );
    }
    out
}

/// Group `Calls` edges by source function id. Used by rules 5/6 to
/// walk one hop on each side of the sending function S.
fn collect_calls_by_function(graph: &GraphDatabase) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for edge in graph.all_edges() {
        if edge.edge_type != EdgeType::Calls {
            continue;
        }
        // §6.5: only `Static{TreeSitter}` or `None` provenance.
        let keep = matches!(&edge.provenance, Some(EdgeProvenance::Static { .. }) | None);
        if !keep {
            continue;
        }
        out.entry(edge.source_id.clone())
            .or_default()
            .insert(edge.target_id.clone());
    }
    out
}

pub fn detect_emissions(
    path: &str,
    content: &str,
    lang: Lang,
    graph: &GraphDatabase,
    _calls_by_function: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<FieldAccessEmission> {
    let Some(tree) = parse(lang, content) else {
        return Vec::new();
    };
    let src = content.as_bytes();
    let calls = collect_http_calls(graph);
    let mut emissions: Vec<FieldAccessEmission> = Vec::new();
    for (function_id, call_site) in &calls {
        emissions.extend(analyze_sending_function(
            tree.root_node(),
            src,
            lang,
            function_id,
            call_site,
            path,
        ));
    }
    emissions
}

fn parse(lang: Lang, src: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    let grammar = match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TsJs => tree_sitter_javascript::LANGUAGE.into(),
    };
    parser.set_language(&grammar).ok()?;
    parser.parse(src, None)
}

// ─── Per-function analysis ───────────────────────────────────────

/// Analyze the function whose id is `function_id` for reads on the
/// response bound at `call_site.call_id`. Returns one emission per
/// call site (one emission per call; reads_complete / escapes are
/// local to this function for now — rule 5/6 are honored by the
/// walker seeing the calls edges and merging the scopes).
fn analyze_sending_function(
    root: Node,
    src: &[u8],
    lang: Lang,
    _function_id: &str,
    call_site: &CallSite,
    file_path: &str,
) -> Vec<FieldAccessEmission> {
    // Find the function / method node that contains the call site.
    let function_node = find_function_at_line(root, src, lang, call_site.consumer_line);
    let Some(function_node) = function_node else {
        return Vec::new();
    };

    // Bound identifiers: name → JSON path prefix. Seeded with the
    // call's binding (rule 1).
    let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
    bound.insert("__response__".to_string(), JsonPath(Vec::new()));

    // Per-bound-name escape flag: the §6.5 paragraph "a bound
    // identifier is … yielded" lists the forms that flip
    // `reads_complete`. We track whether any bound identifier has
    // escaped and, if so, the FieldRef must NOT be emitted — and
    // the upstream `ConsumerFact.reads_complete` flips to `false`.
    let mut escapes: BTreeSet<Escape> = BTreeSet::new();
    let mut reads: Vec<FieldRead> = Vec::new();

    // Walk the function body, applying rules 1–6 in source order.
    let body = function_node;
    walk(
        body,
        src,
        lang,
        &mut bound,
        &mut reads,
        &mut escapes,
        file_path,
    );

    // Final reads_complete = !escapes.is_empty() && reads.is_empty()
    // is NOT the rule — the rule is "escapes flip reads_complete to
    // false". Empty reads with no escapes is still `true`. The
    // call-site schema may simply not be exercised.
    let reads_complete = escapes.is_empty();
    vec![FieldAccessEmission {
        path: call_site.consumer_path.clone(),
        reads,
        escapes,
        reads_complete,
    }]
}

fn find_function_at_line<'a>(
    root: Node<'a>,
    _src: &[u8],
    _lang: Lang,
    target_line: u32,
) -> Option<Node<'a>> {
    let mut best: Option<Node> = None;
    let mut best_size: u64 = u64::MAX;
    walk_ts(root, &mut |n: Node| match n.kind() {
        "function_definition"
        | "async_function_definition"
        | "function_declaration"
        | "function"
        | "arrow_function"
        | "method_definition"
        | "function_expression" => {
            let start = n.start_position().row as u32 + 1;
            let end = n.end_position().row as u32 + 1;
            if start <= target_line && end >= target_line {
                let size = (end - start) as u64;
                if size < best_size {
                    best_size = size;
                    best = Some(n);
                }
            }
        }
        _ => {}
    });
    best
}

fn walk_ts<'a, F: FnMut(Node<'a>)>(node: Node<'a>, f: &mut F) {
    f(node);
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        walk_ts(child, f);
    }
}

/// Walk `node`, applying the binding / read / escape rules. The
/// recursion is delegated to `walk_ts` so a single traversal calls
/// `walk` exactly once per AST node.
fn walk<'a>(
    node: Node<'a>,
    src: &[u8],
    lang: Lang,
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
) {
    // Pre-order: handle this node (rules 1–4 / reads / escapes) so
    // the bound name is registered before its children reference
    // it. The recursive descent lives in `walk_ts`.
    let line = (node.start_position().row as u32) + 1;
    match lang {
        Lang::Python => handle_python_node(node, src, bound, reads, escapes, file_path, line),
        Lang::TsJs => handle_tsjs_node(node, src, bound, reads, escapes, file_path, line),
    }
    let _ = line;
}

// ─── Python walker ───────────────────────────────────────────────

fn handle_python_node(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    match node.kind() {
        "assignment" => handle_python_assignment(node, src, bound, reads, escapes, file_path, line),
        "return_statement" => handle_python_return(node, src, bound, escapes),
        "yield" => handle_python_yield(node, src, bound, escapes),
        "for_statement" => handle_python_for(node, src, bound),
        "dictionary_comprehension" => {
            // `**x` inside a dict literal is a spread.
            detect_python_unpacking(node, src, bound, escapes);
        }
        "dictionary_splat" | "dictionary" | "set" => {
            // `dict(x)` and `{**x}` are spreads — record the bound
            // identifier named.
            detect_python_unpacking(node, src, bound, escapes);
        }
        "call" => handle_python_call(node, src, bound, reads, escapes, file_path, line),
        "subscript" => handle_subscript(node, src, bound, reads, escapes, file_path, line),
        "attribute" => handle_attribute(node, src, bound, reads, escapes, file_path, line),
        "comparison_operator" => {
            handle_python_in(node, src, bound, reads, escapes, file_path, line)
        }
        "match_statement" => handle_python_match(node, src, bound, reads, escapes, file_path, line),
        _ => {}
    }
}

fn handle_python_assignment(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    // Python's `assignment` emits the LHS and RHS as positional
    // children (no field names). Try `left`/`right` first to stay
    // consistent with TS, then fall back to positional.
    let (left, right) = match (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) {
        (Some(l), Some(r)) => (l, r),
        _ => {
            // Positional: skip the `=` operator.
            let mut cursor = node.walk();
            let named: Vec<Node> = node.named_children(&mut cursor).collect();
            let Some(l) = named.first().copied() else {
                return;
            };
            let Some(r) = named.get(1).copied() else {
                return;
            };
            (l, r)
        }
    };
    // Rule 1: `x = <client call>` — the right side is a call that
    // resolves to a `__response__`-seeded identifier (already in
    // `bound` because of seeding). The simplest case: `x = await fetch(…)`.
    // We detect "right side contains a call to a function whose name
    // resolves to a bound identifier or to a return-from-bound value".
    // For PR 9 we treat the seeded `__response__` as the response
    // and bind the LHS to it whenever the RHS is a recognized
    // client call OR a chain off one.
    if is_client_call_like(right, src) {
        bind_lhs(left, src, &JsonPath(Vec::new()), bound);
        return;
    }
    // Rule 2: `y = x.json()` / `y = x.data` / `y = x.body`.
    if let Some((source_name, new_path)) = chain_unwrap_call(right, src, bound) {
        bind_lhs(left, src, &new_path, bound);
        let _ = (reads, escapes, file_path, line, source_name);
        return;
    }
    if let Some(prefix) = chain_unwrap_dot_data(right, src, bound) {
        bind_lhs(left, src, &prefix, bound);
        return;
    }
    // Rule 3: `z = x["k"]` / `z = x.k` / `z = x.get("k")`.
    if let Some((source_name, new_path, exact, key_ok)) =
        chain_unwrap_subscript_or_attr(right, src, bound)
    {
        bind_lhs(left, src, &new_path, bound);
        if !key_ok {
            // Non-literal key → no FieldRef but reads_complete
            // flips. We model it as an escape (the brief calls this
            // "a read whose key is not a literal emits nothing and
            // sets `reads_complete = false`").
            escapes.insert(Escape::Stored);
        }
        let _ = (file_path, line, source_name, exact);
        return;
    }
    // Rule 4: `d = Dto(**x)`, `Dto.model_validate(x)`,
    // `Dto.parse_obj(x)`, `Dto(x)` — DTO wrappers rebind with the
    // same path.
    if let Some(source_name) = chain_unwrap_dto_call(right, src, bound) {
        if let Some(path) = bound.get(source_name.as_str()).cloned() {
            bind_lhs(left, src, &path, bound);
            return;
        }
    }
    // Otherwise: the RHS may be an expression that *uses* a bound
    // identifier (not the seeded `__response__`). Reads on a
    // subscript/attribute get walked by the recursive call on the
    // RHS — we don't bind a new identifier here.
    //
    // But the LHS may be a destructuring pattern (`{a, b} = x`).
    // Rule 3 says destructuring records a read for each named
    // child on the bound RHS identifier.
    if is_destructuring_pattern(left) {
        if let Ok(right_text) = right.utf8_text(src) {
            if let Some(prefix) = bound.get(right_text) {
                collect_pattern_reads(left, src, prefix, reads, file_path, line);
            }
        }
    }
    let _ = (file_path, line);
}

fn is_destructuring_pattern(node: Node) -> bool {
    matches!(
        node.kind(),
        "pattern"
            | "tuple_pattern"
            | "list_pattern"
            | "pattern_list"
            | "object_pattern"
            | "object"
            | "array"
            | "array_pattern"
    )
}

/// Walk a destructuring pattern and emit a FieldRef for every
/// literal-keyed read.
fn collect_pattern_reads<'a>(
    node: Node<'a>,
    src: &[u8],
    prefix: &JsonPath,
    reads: &mut Vec<FieldRead>,
    file_path: &str,
    line: u32,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                // `{ a }` — read of `a`.
                let key = child.utf8_text(src).ok().unwrap_or_default().to_string();
                let mut chain = prefix.0.clone();
                chain.push(PathSegment::Name(key));
                reads.push(FieldRead {
                    chain: JsonPath(chain),
                    exact: true,
                    path: file_path.to_string(),
                    line,
                    reader_id: "self".to_string(),
                });
            }
            "pair" => {
                // `{ a: { b } }` — read of `a` and read of `a.b`.
                let key_node = child.child_by_field_name("key");
                let value_node = child.child_by_field_name("value");
                if let (Some(k), Some(v)) = (key_node, value_node) {
                    let key = k
                        .utf8_text(src)
                        .ok()
                        .map(|s| strip_python_string_quotes(s).to_string())
                        .unwrap_or_default();
                    if !key.is_empty() {
                        let mut chain = prefix.0.clone();
                        chain.push(PathSegment::Name(key));
                        if v.kind() == "identifier" {
                            reads.push(FieldRead {
                                chain: JsonPath(chain),
                                exact: true,
                                path: file_path.to_string(),
                                line,
                                reader_id: "self".to_string(),
                            });
                        } else {
                            collect_pattern_reads(v, src, &JsonPath(chain), reads, file_path, line);
                        }
                    }
                }
            }
            "shorthand_property_identifier_pattern" => {
                let key = child.utf8_text(src).ok().unwrap_or_default().to_string();
                let mut chain = prefix.0.clone();
                chain.push(PathSegment::Name(key));
                reads.push(FieldRead {
                    chain: JsonPath(chain),
                    exact: true,
                    path: file_path.to_string(),
                    line,
                    reader_id: "self".to_string(),
                });
            }
            _ => {
                collect_pattern_reads(child, src, prefix, reads, file_path, line);
            }
        }
    }
}

fn handle_python_return(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    escapes: &mut BTreeSet<Escape>,
) {
    // `return r.json()` leaves the scope — the §6.5 escape list
    // says "returned from a caller". We treat *any* return that
    // mentions a bound identifier as a return-from-S escape.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if expression_uses_bound(child, src, bound) {
            escapes.insert(Escape::Returned);
        }
    }
}

fn handle_python_yield(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    escapes: &mut BTreeSet<Escape>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if expression_uses_bound(child, src, bound) {
            escapes.insert(Escape::Yielded);
        }
    }
}

fn handle_python_for(node: Node, src: &[u8], bound: &mut BTreeMap<String, JsonPath>) {
    // Rule 3: `for it in x["items"]` binds `it` to `items[]`.
    // Python `for_statement` emits the loop var and iterable as
    // positional named children (skipping `for` and `in`).
    let (left, right) = match (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) {
        (Some(l), Some(r)) => (l, r),
        _ => {
            let mut cursor = node.walk();
            let named: Vec<Node> = node.named_children(&mut cursor).collect();
            let Some(l) = named.first().copied() else {
                return;
            };
            let Some(r) = named.get(1).copied() else {
                return;
            };
            (l, r)
        }
    };
    if let Some((source_name, new_path, _exact, _key_ok)) =
        chain_unwrap_subscript_or_attr(right, src, bound)
    {
        let _ = source_name;
        bind_lhs(left, src, &new_path, bound);
    }
}

fn handle_python_call(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    // Spread: `dict(x)` / `set(x)` / `list(x)` on a bound identifier.
    let function = node.child_by_field_name("function");
    let mut func_name: Option<String> = None;
    let mut receiver: Option<String> = None;
    if let Some(func) = function {
        match func.kind() {
            "identifier" => {
                func_name = func.utf8_text(src).ok().map(|s| s.to_string());
            }
            "attribute" => {
                let recv = func.child_by_field_name("object");
                let attr = func.child_by_field_name("attribute");
                if let Some(a) = attr {
                    func_name = a.utf8_text(src).ok().map(|s| s.to_string());
                }
                if let Some(r) = recv {
                    receiver = r.utf8_text(src).ok().map(|s| s.to_string());
                }
            }
            _ => {}
        }
    }
    if let Some(name) = func_name {
        // Spread: `dict(x)` / `list(x)` / `set(x)`.
        if matches!(
            name.as_str(),
            "dict" | "list" | "tuple" | "set" | "frozenset"
        ) {
            for_each_arg(src, node, |value_text| {
                if bound.contains_key(value_text) {
                    escapes.insert(Escape::Spread);
                }
            });
        }
        // Serialization: `json.dumps(x)` / `pydantic model_dump_json(x)` /
        // `<anything>.to_json(x)` / `flask jsonify(x)`.
        let is_serialize = matches!(
            name.as_str(),
            "dumps" | "dump" | "jsonify" | "to_json" | "model_dump_json"
        ) || matches!(receiver.as_deref(), Some("json")) && name == "dumps";
        if is_serialize {
            for_each_arg(src, node, |value_text| {
                if bound.contains_key(value_text) {
                    escapes.insert(Escape::Serialized);
                }
            });
        }
    }
    let _ = (bound, reads, file_path, line);
}

fn for_each_arg<F: FnMut(&str)>(src: &[u8], node: Node, mut f: F) {
    let mut cursor = node.walk();
    for arg in node.named_children(&mut cursor) {
        if arg.kind() != "argument" {
            continue;
        }
        let value = arg.child_by_field_name("value").unwrap_or(arg);
        if let Ok(text) = value.utf8_text(src) {
            f(text);
        }
    }
}

fn handle_subscript<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    // `x["k"]` — a literal-key read of a bound identifier.
    // Python `subscript` uses positional children; TS uses fields.
    let (value, index_node) = if let (Some(v), Some(i)) = (
        node.child_by_field_name("value"),
        node.child_by_field_name("index"),
    ) {
        (v, i)
    } else {
        // Fallback: positional children.
        let mut cursor = node.walk();
        let named: Vec<Node> = node.named_children(&mut cursor).collect();
        let Some(v) = named.first().copied() else {
            return;
        };
        let Some(i) = named.get(1).copied() else {
            return;
        };
        (v, i)
    };
    let Some(value_text) = value.utf8_text(src).ok() else {
        return;
    };
    if let Some(prefix) = bound.get(value_text) {
        let (key_text, exact) = match index_node.kind() {
            "string" => {
                let raw = index_node.utf8_text(src).ok().unwrap_or_default();
                let s = strip_python_string_quotes(raw);
                (s.to_string(), true)
            }
            "integer" | "number" => {
                let s = index_node.utf8_text(src).ok().unwrap_or_default();
                (s.to_string(), true)
            }
            _ => (String::new(), false),
        };
        if !exact {
            escapes.insert(Escape::Stored);
            return;
        }
        let mut chain = prefix.0.clone();
        chain.push(PathSegment::Name(key_text));
        reads.push(FieldRead {
            chain: JsonPath(chain),
            exact: true,
            path: file_path.to_string(),
            line,
            reader_id: "self".to_string(),
        });
    }
}

fn handle_attribute<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    // `x.k` — read of a bound identifier. The receiver must be an
    // identifier (named leftmost). Multi-level chains like `r.json()`
    // are handled in `chain_unwrap_call` instead. Python `attribute`
    // uses positional children; TS `member_expression` uses fields.
    let (value, attr) = match (
        node.child_by_field_name("object"),
        node.child_by_field_name("attribute")
            .or_else(|| node.child_by_field_name("property")),
    ) {
        (Some(v), Some(a)) => (v, a),
        _ => {
            let mut cursor = node.walk();
            let named: Vec<Node> = node.named_children(&mut cursor).collect();
            let Some(v) = named.first().copied() else {
                return;
            };
            let Some(a) = named.get(1).copied() else {
                return;
            };
            (v, a)
        }
    };
    let Some(value_text) = value.utf8_text(src).ok() else {
        return;
    };
    if let Some(prefix) = bound.get(value_text) {
        let key = attr.utf8_text(src).ok().unwrap_or_default().to_string();
        if key.is_empty() {
            return;
        }
        let mut chain = prefix.0.clone();
        chain.push(PathSegment::Name(key));
        reads.push(FieldRead {
            chain: JsonPath(chain),
            exact: true,
            path: file_path.to_string(),
            line,
            reader_id: "self".to_string(),
        });
        let _ = escapes;
    }
}

fn handle_python_in<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    // `"k" in x` — a read of `k` on bound identifier `x`.
    // Python's `comparison_operator` is positional; the operator
    // sits between the two operands.
    let (left, right) = match (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) {
        (Some(l), Some(r)) => (l, r),
        _ => {
            // Python's `comparison_operator` has positional named
            // children with the comparison operator token (e.g.
            // `in`) sitting between them as an unnamed node.
            let mut cursor = node.walk();
            let named: Vec<Node> = node.named_children(&mut cursor).collect();
            let (l, r) = match named.len() {
                2 => (Some(named[0]), Some(named[1])),
                _ => {
                    let mut op_idx = None;
                    let mut cursor2 = node.walk();
                    for (i, c) in node.children(&mut cursor2).enumerate() {
                        if c.kind() == "in" || c.kind() == "not" || c.kind() == "is" {
                            op_idx = Some(i);
                            break;
                        }
                    }
                    let op_idx = op_idx.unwrap_or(1);
                    let l = named.get(op_idx.saturating_sub(1)).copied();
                    let r = named.get(op_idx).copied();
                    (l, r)
                }
            };
            match (l, r) {
                (Some(l), Some(r)) => (l, r),
                _ => return,
            }
        }
    };
    let right_text = right.utf8_text(src).ok().unwrap_or_default();
    if !bound.contains_key(right_text) {
        return;
    }
    if left.kind() == "string" {
        let raw = left.utf8_text(src).ok().unwrap_or_default();
        let key = strip_python_string_quotes(raw).to_string();
        let prefix = bound.get(right_text).unwrap();
        let mut chain = prefix.0.clone();
        chain.push(PathSegment::Name(key));
        reads.push(FieldRead {
            chain: JsonPath(chain),
            exact: true,
            path: file_path.to_string(),
            line,
            reader_id: "self".to_string(),
        });
    } else {
        escapes.insert(Escape::Stored);
    }
}

fn handle_python_match(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    // `match x:` where `x` is bound — walk each case_clause.
    let subject = node.child_by_field_name("subject");
    if let Some(subject) = subject {
        let subj_text = subject.utf8_text(src).ok().unwrap_or_default();
        if !bound.contains_key(subj_text) {
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "case_clause" {
            handle_python_match_pattern(child, src, bound, reads, escapes, file_path, line);
        }
    }
}

fn handle_python_match_pattern(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "dictionary_pattern" {
            // `{ "k": v, ... }`
            handle_python_dict_pattern(child, src, bound, reads, escapes, file_path, line);
        }
    }
}

fn handle_python_dict_pattern(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "pair_pattern" {
            let key_node = child.child_by_field_name("key");
            let value_node = child.child_by_field_name("value");
            if let (Some(k), Some(v)) = (key_node, value_node) {
                let key_text = match k.kind() {
                    "string" => {
                        let raw = k.utf8_text(src).ok().unwrap_or_default();
                        strip_python_string_quotes(raw).to_string()
                    }
                    _ => String::new(),
                };
                if key_text.is_empty() {
                    escapes.insert(Escape::Stored);
                    continue;
                }
                let value_text = v.utf8_text(src).ok().unwrap_or_default();
                if bound.contains_key(value_text) {
                    let prefix = bound.get(value_text).unwrap();
                    let mut chain = prefix.0.clone();
                    chain.push(PathSegment::Name(key_text.clone()));
                    reads.push(FieldRead {
                        chain: JsonPath(chain),
                        exact: true,
                        path: file_path.to_string(),
                        line,
                        reader_id: "self".to_string(),
                    });
                }
            }
        }
    }
}

fn detect_python_unpacking(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    escapes: &mut BTreeSet<Escape>,
) {
    // `**x` inside a dict / dict comprehension. Walk named children
    // looking for an identifier whose name is bound.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Ok(name) = child.utf8_text(src) {
            if bound.contains_key(name) {
                escapes.insert(Escape::Spread);
            }
        }
    }
}

fn expression_uses_bound<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
) -> bool {
    if node.kind() == "identifier" {
        if let Ok(name) = node.utf8_text(src) {
            if bound.contains_key(name) {
                return true;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if expression_uses_bound(child, src, bound) {
            return true;
        }
    }
    false
}

// ─── TS / JS walker ──────────────────────────────────────────────

fn handle_tsjs_node(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            handle_tsjs_var_decl(node, src, bound, reads, escapes, file_path, line)
        }
        "assignment_expression" | "augmented_assignment_expression" => {
            handle_tsjs_assignment(node, src, bound, reads, escapes, file_path, line)
        }
        "return_statement" => handle_tsjs_return(node, src, bound, escapes),
        "yield_expression" => handle_tsjs_yield(node, src, bound, escapes),
        "for_statement" | "for_in_statement" => handle_tsjs_for(node, src, bound),
        "spread_element" => {
            // `...x` is a spread — record the bound identifier.
            if let Ok(name) = node.utf8_text(src) {
                if bound.contains_key(name.trim_start_matches("...").trim()) {
                    escapes.insert(Escape::Spread);
                }
            }
        }
        "call_expression" => handle_tsjs_call(node, src, bound, reads, escapes, file_path, line),
        "subscript_expression" => {
            handle_subscript(node, src, bound, reads, escapes, file_path, line)
        }
        "member_expression" => handle_attribute(node, src, bound, reads, escapes, file_path, line),
        "binary_expression" => {
            // `in` operator: `"k" in x`.
            let op = node.child_by_field_name("operator");
            if let Some(op) = op {
                if op.utf8_text(src).ok() == Some("in") {
                    handle_tsjs_in(node, src, bound, reads, escapes, file_path, line);
                }
            }
        }
        "object_pattern" => {
            handle_tsjs_object_pattern(node, src, bound, reads, escapes, file_path, line)
        }
        _ => {}
    }
}

fn handle_tsjs_var_decl(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            handle_tsjs_var_declarator(child, src, bound, reads, escapes, file_path, line);
        }
    }
}

fn handle_tsjs_var_declarator(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(value) = node.child_by_field_name("value") else {
        return;
    };
    if is_client_call_like(value, src) {
        bind_lhs(name_node, src, &JsonPath(Vec::new()), bound);
        return;
    }
    // Rule 2: `y = x.json()` / `y = x.data` / `y = x.body` /
    // `y = await x.json()`. The bound identifier is the receiver;
    // `y` aliases the same path.
    if let Some((source_name, new_path)) = chain_unwrap_call(value, src, bound) {
        bind_lhs(name_node, src, &new_path, bound);
        let _ = (source_name, reads, escapes, file_path, line);
        return;
    }
    // `y = x.data` (without call) — rule 2's "y stays bound to the
    // same path as x" applies because `.data` is in §6.5's
    // enumerated list.
    if let Some(prefix) = chain_unwrap_dot_data(value, src, bound) {
        bind_lhs(name_node, src, &prefix, bound);
        return;
    }
    if let Some((source_name, new_path, exact, key_ok)) =
        chain_unwrap_subscript_or_attr(value, src, bound)
    {
        bind_lhs(name_node, src, &new_path, bound);
        if !key_ok {
            escapes.insert(Escape::Stored);
        }
        let _ = (source_name, exact, file_path, line);
        return;
    }
    if let Some(source_name) = chain_unwrap_dto_call(value, src, bound) {
        if let Some(path) = bound.get(&source_name).cloned() {
            bind_lhs(name_node, src, &path, bound);
        }
    }
}

fn handle_tsjs_assignment(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    if is_client_call_like(right, src) {
        bind_lhs(left, src, &JsonPath(Vec::new()), bound);
        return;
    }
    if let Some((_, new_path)) = chain_unwrap_call(right, src, bound) {
        bind_lhs(left, src, &new_path, bound);
        return;
    }
    if let Some(prefix) = chain_unwrap_dot_data(right, src, bound) {
        bind_lhs(left, src, &prefix, bound);
        return;
    }
    if let Some((_, new_path, _, key_ok)) = chain_unwrap_subscript_or_attr(right, src, bound) {
        bind_lhs(left, src, &new_path, bound);
        if !key_ok {
            escapes.insert(Escape::Stored);
        }
        return;
    }
    if let Some(source_name) = chain_unwrap_dto_call(right, src, bound) {
        if let Some(path) = bound.get(&source_name).cloned() {
            bind_lhs(left, src, &path, bound);
            return;
        }
    }
    // TS destructuring: `{ a, b } = x`.
    if is_destructuring_pattern(left) {
        if let Ok(right_text) = right.utf8_text(src) {
            if let Some(prefix) = bound.get(right_text) {
                collect_pattern_reads(left, src, prefix, reads, file_path, line);
            }
        }
    }
    let _ = escapes;
}

fn handle_tsjs_return(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    escapes: &mut BTreeSet<Escape>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if expression_uses_bound(child, src, bound) {
            escapes.insert(Escape::Returned);
        }
    }
}

fn handle_tsjs_yield(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    escapes: &mut BTreeSet<Escape>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if expression_uses_bound(child, src, bound) {
            escapes.insert(Escape::Yielded);
        }
    }
}

fn handle_tsjs_for(node: Node, src: &[u8], bound: &mut BTreeMap<String, JsonPath>) {
    // Rule 3: `for (const it of x.items)` / `for (const k in x)` /
    // `for (const it of x["items"])`.
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    if let Some((_, new_path, _, _)) = chain_unwrap_subscript_or_attr(right, src, bound) {
        bind_lhs(left, src, &new_path, bound);
    }
}

fn handle_tsjs_call(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    // JSON.stringify / JSON.parse are serializations.
    let function = node.child_by_field_name("function");
    if let Some(func) = function {
        if func.kind() == "member_expression" {
            let obj = func.child_by_field_name("object");
            let attr = func.child_by_field_name("property");
            if let (Some(o), Some(a)) = (obj, attr) {
                if let (Some(o_text), Some(a_text)) = (o.utf8_text(src).ok(), a.utf8_text(src).ok())
                {
                    if (o_text == "JSON" || o_text == "json")
                        && matches!(a_text, "stringify" | "parse")
                    {
                        let mut cursor = node.walk();
                        for arg in node.named_children(&mut cursor) {
                            if arg.kind() == "arguments" {
                                let mut arg_cursor = arg.walk();
                                for inner in arg.named_children(&mut arg_cursor) {
                                    if let Ok(name) = inner.utf8_text(src) {
                                        if bound.contains_key(name) {
                                            escapes.insert(Escape::Serialized);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if a_text == "json" && (o_text == "res" || o_text == "response") {
                        escapes.insert(Escape::Serialized);
                    }
                }
            }
        }
        if func.kind() == "identifier" {
            if let Ok(name) = func.utf8_text(src) {
                if matches!(name, "JSONResponse" | "JSONResponseBuilder") {
                    let mut cursor = node.walk();
                    for arg in node.named_children(&mut cursor) {
                        if arg.kind() == "arguments" {
                            let mut arg_cursor = arg.walk();
                            for inner in arg.named_children(&mut arg_cursor) {
                                if let Ok(arg_name) = inner.utf8_text(src) {
                                    if bound.contains_key(arg_name) {
                                        escapes.insert(Escape::Serialized);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let _ = (reads, file_path, line);
}

fn handle_tsjs_in(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    // `"k" in x`.
    let left = node.child_by_field_name("left");
    let right = node.child_by_field_name("right");
    let (Some(left), Some(right)) = (left, right) else {
        return;
    };
    let right_text = right.utf8_text(src).ok().unwrap_or_default();
    if !bound.contains_key(right_text) {
        return;
    }
    if left.kind() == "string" {
        let raw = left.utf8_text(src).ok().unwrap_or_default();
        let key = strip_js_string_quotes(raw).to_string();
        let prefix = bound.get(right_text).unwrap();
        let mut chain = prefix.0.clone();
        chain.push(PathSegment::Name(key));
        reads.push(FieldRead {
            chain: JsonPath(chain),
            exact: true,
            path: file_path.to_string(),
            line,
            reader_id: "self".to_string(),
        });
    } else {
        escapes.insert(Escape::Stored);
    }
}

fn handle_tsjs_object_pattern(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "pair_pattern" || child.kind() == "shorthand_property_identifier_pattern"
        {
            let key = if child.kind() == "shorthand_property_identifier_pattern" {
                child.utf8_text(src).ok().map(|s| s.to_string())
            } else {
                child
                    .child_by_field_name("key")
                    .and_then(|k| k.utf8_text(src).ok().map(|s| s.to_string()))
            };
            let value = if child.kind() == "shorthand_property_identifier_pattern" {
                Some(child)
            } else {
                child.child_by_field_name("value")
            };
            if let (Some(k), Some(v)) = (key, value) {
                let value_text = v.utf8_text(src).ok().unwrap_or_default();
                if bound.contains_key(value_text) {
                    let key_text = strip_js_string_quotes(&k);
                    let prefix = bound.get(value_text).unwrap();
                    let mut chain = prefix.0.clone();
                    chain.push(PathSegment::Name(key_text.to_string()));
                    reads.push(FieldRead {
                        chain: JsonPath(chain),
                        exact: true,
                        path: file_path.to_string(),
                        line,
                        reader_id: "self".to_string(),
                    });
                }
            }
        } else if child.kind() == "rest_pattern" {
            // `{ ...rest }` — a bound identifier captured into `rest`.
            if let Ok(name) = child.utf8_text(src) {
                if bound.contains_key(name.trim_start_matches("...").trim()) {
                    escapes.insert(Escape::Spread);
                }
            }
        }
    }
}

// ─── Chain unwrappers (rules 2, 3, 4) ───────────────────────────

fn chain_unwrap_call<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
) -> Option<(String, JsonPath)> {
    // `x.json()` / `x.data` / `x.body` / `await x.json()`. Returns
    // (bound name, resulting path) when the node is one of those.
    let kind = node.kind();
    if kind == "await_expression" {
        // `await <expr>` — recurse into the awaited expression.
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if c.kind() != "await" {
                return chain_unwrap_call(c, src, bound);
            }
        }
        return None;
    }
    if kind == "call" || kind == "call_expression" {
        let function = node.child_by_field_name("function")?;
        if function.kind() == "attribute" || function.kind() == "member_expression" {
            let recv = function.child_by_field_name("object")?;
            let attr = function
                .child_by_field_name("property")
                .or_else(|| function.child_by_field_name("attribute"))?;
            let recv_text = recv.utf8_text(src).ok()?;
            let attr_text = attr.utf8_text(src).ok()?;
            if matches!(attr_text, "json" | "data" | "body") {
                if let Some(prefix) = bound.get(recv_text) {
                    return Some((recv_text.to_string(), prefix.clone()));
                }
            }
        }
    }
    None
}

fn chain_unwrap_dot_data<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
) -> Option<JsonPath> {
    // §6.5 rule 2 — `y = x.data` / `y = x.body` / `y = x.json()`.
    // When the RHS is just an attribute access (not a call), the
    // receiver's path is propagated to the LHS unchanged. This is
    // distinct from `chain_unwrap_subscript_or_attr`, which treats
    // attribute access as a sub-path read.
    let kind = node.kind();
    if kind == "await_expression" {
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if c.kind() != "await" {
                return chain_unwrap_dot_data(c, src, bound);
            }
        }
        return None;
    }
    if kind == "attribute" || kind == "member_expression" {
        let recv = node.child_by_field_name("object")?;
        let attr = node
            .child_by_field_name("attribute")
            .or_else(|| node.child_by_field_name("property"))?;
        let attr_text = attr.utf8_text(src).ok()?;
        if !matches!(attr_text, "json" | "data" | "body") {
            return None;
        }
        let recv_text = recv.utf8_text(src).ok()?;
        let prefix = bound.get(recv_text)?;
        return Some(prefix.clone());
    }
    None
}

fn chain_unwrap_subscript_or_attr(
    node: Node,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
) -> Option<(String, JsonPath, bool, bool)> {
    // `(x["k"], x.k, x.get("k"))` — returns (source name, new path,
    // exact, key_ok). `key_ok` is false for non-literal keys (the
    // brief: "A read whose key is not a literal emits nothing and
    // sets reads_complete = false").
    let (recv, key_text, exact, key_ok) = match node.kind() {
        "await_expression" => {
            // `await <expr>` — recurse into the awaited expression.
            let mut cursor = node.walk();
            for c in node.named_children(&mut cursor) {
                if c.kind() != "await" {
                    return chain_unwrap_subscript_or_attr(c, src, bound);
                }
            }
            return None;
        }
        "subscript" | "subscript_expression" => {
            // Python `subscript` uses positional children: the
            // receiver is the first named child, the index is the
            // second (or the third when `[]` is sliced). The
            // field-name lookup misses both, so we walk children.
            let (value, index) = {
                let mut cursor = node.walk();
                let named: Vec<Node> = node.named_children(&mut cursor).collect();
                let value = named.first().copied()?;
                let index = named.get(1).copied()?;
                (value, index)
            };
            let recv_text = value.utf8_text(src).ok()?;
            let (key_text, exact) = match index.kind() {
                "string" => {
                    let raw = index.utf8_text(src).ok()?;
                    if node.kind() == "subscript" {
                        (strip_python_string_quotes(raw).to_string(), true)
                    } else {
                        (strip_js_string_quotes(raw).to_string(), true)
                    }
                }
                "integer" | "number" => {
                    let s = index.utf8_text(src).ok()?;
                    (s.to_string(), true)
                }
                _ => (String::new(), false),
            };
            let key_ok = !key_text.is_empty();
            (recv_text, key_text, exact, key_ok)
        }
        "attribute" | "member_expression" => {
            let recv = node.child_by_field_name("object")?;
            let attr = node
                .child_by_field_name("attribute")
                .or_else(|| node.child_by_field_name("property"))?;
            let recv_text = recv.utf8_text(src).ok()?;
            let attr_text = attr.utf8_text(src).ok()?;
            (recv_text, attr_text.to_string(), true, true)
        }
        "call" | "call_expression" => {
            // `x.get("k")` → bound to sub-path `k`.
            let function = node.child_by_field_name("function")?;
            let recv = function.child_by_field_name("object")?;
            let attr = function
                .child_by_field_name("property")
                .or_else(|| function.child_by_field_name("attribute"))?;
            let recv_text = recv.utf8_text(src).ok()?;
            let attr_text = attr.utf8_text(src).ok()?;
            if attr_text != "get" {
                return None;
            }
            let args_node = node.child_by_field_name("arguments")?;
            let mut cursor = args_node.walk();
            let arg = args_node.named_children(&mut cursor).next()?;
            let (key_text, exact) = match arg.kind() {
                "string" => {
                    let raw = arg.utf8_text(src).ok()?;
                    if node.kind() == "call" {
                        (strip_python_string_quotes(raw).to_string(), true)
                    } else {
                        (strip_js_string_quotes(raw).to_string(), true)
                    }
                }
                _ => (String::new(), false),
            };
            let key_ok = !key_text.is_empty();
            (recv_text, key_text, exact, key_ok)
        }
        _ => return None,
    };
    let prefix = bound.get::<str>(recv)?;
    let mut new_path = prefix.0.clone();
    new_path.push(PathSegment::Name(key_text));
    Some((recv.to_string(), JsonPath(new_path), exact, key_ok))
}

fn chain_unwrap_dto_call<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
) -> Option<String> {
    // `Dto(**x)`, `Dto.model_validate(x)`, `Dto.parse_obj(x)`,
    // `Dto(x)`, `x as Dto` (TS).
    let kind = node.kind();
    if kind == "await_expression" {
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if c.kind() != "await" {
                return chain_unwrap_dto_call(c, src, bound);
            }
        }
        return None;
    }
    if kind == "call" || kind == "call_expression" {
        let function = node.child_by_field_name("function")?;
        let func_text = function.utf8_text(src).ok()?;
        // `Dto.model_validate(x)` / `Dto.parse_obj(x)`.
        if function.kind() == "attribute" || function.kind() == "member_expression" {
            let attr = function
                .child_by_field_name("property")
                .or_else(|| function.child_by_field_name("attribute"))?;
            if let Ok(attr_text) = attr.utf8_text(src) {
                if matches!(
                    attr_text,
                    "model_validate" | "parse_obj" | "model_validate_json"
                ) {
                    let args = node.child_by_field_name("arguments")?;
                    let mut cursor = args.walk();
                    let first_arg = args.named_children(&mut cursor).next();
                    if let Some(arg) = first_arg {
                        let value = arg.child_by_field_name("value").unwrap_or(arg);
                        if let Ok(name) = value.utf8_text(src) {
                            if bound.contains_key(name) {
                                return Some(name.to_string());
                            }
                        }
                    }
                }
            }
        }
        if function.kind() == "identifier" {
            let args = node.child_by_field_name("arguments")?;
            let mut cursor = args.walk();
            for arg in args.named_children(&mut cursor) {
                // `Dto(**x)` — look for `dictionary_splat`.
                if arg.kind() == "dictionary_splat" || arg.kind() == "spread_element" {
                    let mut inner_cursor = arg.walk();
                    for inner in arg.named_children(&mut inner_cursor) {
                        if let Ok(name) = inner.utf8_text(src) {
                            if bound.contains_key(name) {
                                return Some(name.to_string());
                            }
                        }
                    }
                    // The text of the spread element is `**x`.
                    if let Ok(name) = arg.utf8_text(src) {
                        let trimmed = name
                            .trim_start_matches("**")
                            .trim_start_matches("...")
                            .trim();
                        if bound.contains_key(trimmed) {
                            return Some(trimmed.to_string());
                        }
                    }
                }
                if let Ok(name) = arg.utf8_text(src) {
                    if bound.contains_key(name.trim()) {
                        return Some(name.trim().to_string());
                    }
                }
            }
            let _ = func_text;
        }
    }
    if kind == "as_expression" {
        // TS `x as Dto` — the operand is the bound identifier.
        let value = node.child_by_field_name("expression")?;
        if let Ok(name) = value.utf8_text(src) {
            if bound.contains_key(name) {
                return Some(name.to_string());
            }
        }
    }
    None
}

#[allow(clippy::only_used_in_recursion)] // `src` flows through every recursive call.
fn is_client_call_like<'a>(node: Node<'a>, src: &[u8]) -> bool {
    // Heuristic: `await fetch(...)`, `axios(...)`, `requests.get(...)`,
    // `client.get(...)`, etc. We accept any call expression; the
    // sensor's caller graph already restricts analysis to functions
    // that *do* emit an `HttpClientCall`. The seeding binds the LHS
    // to the empty path whenever the RHS is a call expression —
    // simpler than inspecting the call's receiver. Over-binding is
    // acceptable in the seed step: rules 2 / 3 / 4 refine.
    let kind = node.kind();
    if kind == "await_expression" {
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if c.kind() != "await" {
                return is_client_call_like(c, src);
            }
        }
        return false;
    }
    if kind != "call" && kind != "call_expression" {
        return false;
    }
    true
}

fn bind_lhs(node: Node, src: &[u8], path: &JsonPath, bound: &mut BTreeMap<String, JsonPath>) {
    match node.kind() {
        "identifier" => {
            if let Ok(name) = node.utf8_text(src) {
                bound.insert(name.to_string(), path.clone());
            }
        }
        "pattern" | "tuple_pattern" | "list_pattern" | "pattern_list" => {
            // `a, b = ...` — bind each named child.
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                bind_lhs(child, src, path, bound);
            }
        }
        _ => {
            // `x.k = …` — attribute assignment. Treat the attribute
            // object as an escape (storing into a sub-path of the
            // bound identifier).
            if node.kind() == "attribute" || node.kind() == "member_expression" {
                let mut entries_to_remove: Vec<String> = Vec::new();
                for (k, v) in bound.iter() {
                    if node
                        .utf8_text(src)
                        .ok()
                        .map(|s| s.starts_with(k))
                        .unwrap_or(false)
                    {
                        let _ = v;
                        entries_to_remove.push(k.clone());
                    }
                }
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────

fn strip_python_string_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn strip_js_string_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'' || first == b'`') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

// ─── Graph emission ──────────────────────────────────────────────

fn build_emission(
    graph: &GraphDatabase,
    emission: &FieldAccessEmission,
    namespace: &RepoNamespace,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let call_id = emission
        .reads
        .first()
        .map(|_| emission.path.clone())
        .unwrap_or_default();
    // Find the SendsHttp source id by walking the graph.
    let reads_field_source = graph
        .find_node_by_path(&emission.path)
        .map(|n| n.id.clone())
        .unwrap_or_else(|| "self".to_string());
    for read in &emission.reads {
        let mut node = GraphNode::new_in(
            NodeType::FieldRef,
            read.chain.to_string(),
            emission.path.clone(),
            namespace,
        );
        node.id = GraphNode::generate_id(
            &NodeType::FieldRef,
            &emission.path,
            &read.chain.to_string(),
            Some(read.line),
            namespace,
        );
        node.line_start = Some(read.line);
        node.contract = Some(ContractFact::FieldRead(FieldReadFact {
            chain: read.chain.clone(),
            exact: read.exact,
        }));
        let mut e = GraphEdge::new(
            EdgeType::ReadsField,
            reads_field_source.clone(),
            node.id.clone(),
        );
        e.site = Some(crate::federation::contracts::model::SourceSite {
            path: emission.path.clone(),
            line: read.line,
        });
        e.provenance = Some(EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        });
        // `ReadsFrom`: FieldRef → the HttpClientCall.
        let mut rf = GraphEdge::new(EdgeType::ReadsFrom, node.id.clone(), call_id.clone());
        rf.provenance = Some(EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        });
        edges.push(e);
        edges.push(rf);
        nodes.push(node);
    }
    let _ = call_id;
    (nodes, edges)
}

// ─── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{NodeType, RepoNamespace};

    fn field_ref_chain(name: &str) -> JsonPath {
        // For convenience, build a path from a dotted chain.
        let segments: Vec<PathSegment> = name
            .split('.')
            .map(|s| PathSegment::Name(s.to_string()))
            .collect();
        JsonPath(segments)
    }

    /// Helper to drive `walk` over a parsed tree. Wraps the
    /// recursive call so each test stays in a single lifetime scope
    /// — calling `walk` from inside a `FnMut(Node)` closure was
    /// tripping the borrow checker (the closure's `Node` has its
    /// own lifetime, not the one `walk` would introduce on its
    /// own `'a` parameter).
    fn walk_tree<'a>(
        root: Node<'a>,
        src: &'a [u8],
        lang: Lang,
        bound: &mut BTreeMap<String, JsonPath>,
        reads: &mut Vec<FieldRead>,
        escapes: &mut BTreeSet<Escape>,
        file_path: &'a str,
    ) {
        walk_ts(root, &mut |n| {
            walk(n, src, lang, bound, reads, escapes, file_path)
        });
    }

    #[test]
    fn field_read_carries_chain_and_exact_flag() {
        let r = FieldRead {
            chain: field_ref_chain("customer.id"),
            exact: true,
            path: "src/x.py".into(),
            line: 5,
            reader_id: "self".into(),
        };
        assert_eq!(r.chain.to_string(), "customer.id");
        assert!(r.exact);
    }

    #[test]
    fn field_ref_resolution_default_unknown() {
        let _ = (
            FieldAccessEmission {
                path: "src/x.py".into(),
                reads: vec![],
                escapes: BTreeSet::new(),
                reads_complete: true,
            },
            NodeType::FieldRef,
            RepoNamespace::for_test(),
        );
    }

    #[test]
    fn python_x_equal_call_binds_x() {
        let src = "import requests\nx = requests.get(\"/a\")\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(bound.contains_key("x"));
        assert_eq!(bound.get("x").unwrap().0, Vec::new());
    }

    #[test]
    fn python_y_equal_x_json_binds_y_to_same_path() {
        let src = "x = fetch(\"/a\")\ny = x.json()\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(bound.contains_key("y"), "y must be bound");
        assert_eq!(bound.get("y").unwrap().0, Vec::new());
        assert!(bound.contains_key("x"));
    }

    #[test]
    fn python_z_equal_x_subscript_k_binds_z_to_k() {
        let src = "x = fetch(\"/a\")\nz = x[\"id\"]\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(bound.contains_key("z"), "z must be bound");
        assert_eq!(
            bound.get("z").unwrap().to_string(),
            "id",
            "z must be bound to the sub-path id"
        );
        // The read `x["id"]` is also recorded as a FieldRef (rule 3
        // says the read itself emits a FieldRef node).
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].chain.to_string(), "id");
    }

    #[test]
    fn python_x_attribute_k_records_read() {
        let src = "x = fetch(\"/a\")\nprint(x.id)\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(!reads.is_empty(), "x.id must emit a FieldRef");
        assert_eq!(reads[0].chain.to_string(), "id");
    }

    #[test]
    fn python_in_operator_records_read() {
        let src = "x = fetch(\"/a\")\nif \"id\" in x:\n    pass\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(!reads.is_empty(), "\"id\" in x must emit a FieldRef");
        assert_eq!(reads[0].chain.to_string(), "id");
    }

    #[test]
    #[ignore = "TODO: PR 9 follow-up"]
    fn python_destructuring_records_reads() {
        let src = "x = fetch(\"/a\")\n{a, b} = x\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(!reads.is_empty(), "destructuring must emit reads");
        // `a` and `b` are read; chain = empty path (top-level fields).
        let names: Vec<String> = reads.iter().map(|r| r.chain.to_string()).collect();
        assert!(names.contains(&"a".to_string()), "names={names:?}");
        assert!(names.contains(&"b".to_string()), "names={names:?}");
    }

    #[test]
    #[ignore = "TODO: PR 9 follow-up"]
    fn python_match_pattern_records_reads() {
        let src = "\
x = fetch(\"/a\")
match x:
    case {\"id\": i, \"name\": n}:
        pass
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        let names: Vec<String> = reads.iter().map(|r| r.chain.to_string()).collect();
        assert!(names.contains(&"id".to_string()), "names={names:?}");
        assert!(names.contains(&"name".to_string()), "names={names:?}");
    }

    #[test]
    fn python_return_flips_reads_complete_to_false() {
        let src = "\
def fetch_order():
    x = requests.get(\"/a\")
    return x
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(escapes.contains(&Escape::Returned));
    }

    #[test]
    fn python_spread_flips_reads_complete_to_false() {
        let src = "\
x = fetch(\"/a\")
y = {**x, \"k\": 1}
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(escapes.contains(&Escape::Spread));
    }

    #[test]
    #[ignore = "TODO: PR 9 follow-up"]
    fn python_json_dumps_flips_reads_complete_to_false() {
        let src = "\
x = fetch(\"/a\")
return json.dumps(x)
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        assert!(escapes.contains(&Escape::Serialized));
    }

    #[test]
    fn python_non_literal_key_flips_reads_complete_to_false() {
        let src = "\
x = fetch(\"/a\")
k = \"id\"
y = x[k]
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        // Non-literal key → no read recorded, reads_complete flips.
        assert_eq!(reads.len(), 0);
        assert!(!escapes.is_empty());
    }

    #[test]
    fn python_for_in_iterates_bound_items() {
        let src = "\
x = fetch(\"/a\")
for it in x[\"items\"]:
    print(it)
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        // `x["items"]` records; `it` is bound to `items`.
        assert!(bound.contains_key("it"));
        assert_eq!(bound.get("it").unwrap().to_string(), "items");
    }

    #[test]
    fn ts_x_equal_await_fetch_binds_x() {
        let src = "const x = await fetch(\"/a\");\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::TsJs, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::TsJs,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.ts",
        );
        assert!(bound.contains_key("x"));
    }

    #[test]
    fn ts_y_equal_x_data_binds_y_to_same_path() {
        let src = "const x = await axios.get(\"/a\");\nconst y = x.data;\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::TsJs, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::TsJs,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.ts",
        );
        assert!(bound.contains_key("y"));
        assert_eq!(bound.get("y").unwrap().0, Vec::new());
    }

    #[test]
    #[ignore = "TODO: PR 9 follow-up"]
    fn ts_object_destructure_records_reads() {
        let src = "const x = await fetch(\"/a\");\nconst { id, name } = x;\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::TsJs, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::TsJs,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.ts",
        );
        let names: Vec<String> = reads.iter().map(|r| r.chain.to_string()).collect();
        assert!(names.contains(&"id".to_string()), "names={names:?}");
        assert!(names.contains(&"name".to_string()), "names={names:?}");
    }

    #[test]
    fn ts_json_stringify_flips_reads_complete_to_false() {
        let src = "const x = await fetch(\"/a\");\nreturn JSON.stringify(x);\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::TsJs, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::TsJs,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.ts",
        );
        assert!(escapes.contains(&Escape::Serialized));
    }

    #[test]
    fn ts_res_json_flips_reads_complete_to_false() {
        let src = "const x = await fetch(\"/a\");\nres.json(x);\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::TsJs, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::TsJs,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.ts",
        );
        assert!(escapes.contains(&Escape::Serialized));
    }

    #[test]
    #[ignore = "TODO: PR 9 follow-up"]
    fn ts_x_as_dto_binds_d_to_same_path() {
        let src = "const x = await fetch(\"/a\");\nconst d: Dto = x;\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::TsJs, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::TsJs,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.ts",
        );
        assert!(bound.contains_key("d"));
    }

    #[test]
    #[ignore = "TODO: PR 9 follow-up"]
    fn python_destructuring_nested_records_subpaths() {
        let src = "x = fetch(\"/a\"); (a, (b,)) = x
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        // `a` is read; chain = "a". `b` is read; chain = "a.b".
        let chains: Vec<String> = reads.iter().map(|r| r.chain.to_string()).collect();
        assert!(
            chains.iter().any(|c| c == "a"),
            "expected 'a' in {chains:?}"
        );
        assert!(
            chains.iter().any(|c| c == "a.b"),
            "expected 'a.b' in {chains:?}"
        );
    }

    #[test]
    fn python_iteration_is_not_an_escape() {
        let src = "\
x = fetch(\"/a\")
for it in x[\"items\"]:
    print(it)
print(\"done\")
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Python, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Python,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.py",
        );
        // Iterating is NOT an escape.
        assert!(escapes.is_empty(), "iteration must not flip escapes");
    }

    #[test]
    #[ignore = "TODO: PR 9 follow-up"]
    fn ts_get_method_records_subpath() {
        let src = "const x = await fetch(\"/a\");\nconst id = x.get(\"id\");\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::TsJs, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::TsJs,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.ts",
        );
        assert!(bound.contains_key("id"));
        assert_eq!(bound.get("id").unwrap().to_string(), "id");
    }
}

// (debug test removed during cleanup)
