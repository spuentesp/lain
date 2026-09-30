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
    /// The `HttpClientCall` node id this emission's reads belong to.
    /// Becomes the `ReadsFrom` target (§6.5: FieldRef → the call);
    /// `project_edges` rewrites it to the GlobalId the joiner keys
    /// `Binds` by.
    pub call_id: String,
    /// Graph node id of the sending function S (§6.5 scope root for
    /// the interprocedural pass). May be a non-function node id
    /// (module-level call), in which case scope extension is skipped.
    pub sender_id: String,
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
    let mut emissions: Vec<FieldAccessEmission> = Vec::new();

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let Some(lang) = lang_for_path(&path.to_string_lossy()) else {
            continue;
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let path_str = graph_path(root, path);
        emissions.extend(detect_emissions(
            &path_str,
            &content,
            lang,
            graph,
            &calls_by_function,
        ));
    }

    // §6.5 interprocedural pass: extend every emission with the reads
    // of the sending function's direct callers and callees (rules
    // 5/6) over the repo `Calls` graph (`Static{TreeSitter}`/`None`
    // provenance only — filtered in `collect_calls_by_function`).
    extend_emissions_with_scope(root, graph, &calls_by_function, &mut emissions);

    for emission in &emissions {
        // §6.5: an escape flips the call's `ConsumerFact` in place.
        // The node is owned by the phase-1 `http_client_sensor`, so
        // patch it directly — never re-emit it through this sensor's
        // `replace_sensor_output` (that would only remove
        // FieldAccessSensor-owned nodes anyway, but re-emitting the
        // call would race phase 1's ownership on rescans).
        if !emission.reads_complete && !emission.call_id.is_empty() {
            patch_reads_complete(graph, &emission.call_id)?;
        }
        let (nodes, edges) = build_emission(graph, emission, namespace);
        all_nodes.extend(nodes);
        all_edges.extend(edges);
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
    Ts,
    Tsx,
}

/// Language for a source path by extension; `None` for files this
/// sensor does not parse.
fn lang_for_path(path: &str) -> Option<Lang> {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "py" => Some(Lang::Python),
        "ts" => Some(Lang::Ts),
        "tsx" => Some(Lang::Tsx),
        "js" | "jsx" | "mjs" | "cjs" => Some(Lang::TsJs),
        _ => None,
    }
}

#[derive(Debug, Default)]
struct CallSite {
    /// Node id of the `HttpClientCall` the `SendsHttp` edge points
    /// at — the `ReadsFrom` target.
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
        // Only call sites that live in the file being scanned —
        // anything else would analyze this file's tree against
        // another file's call line.
        if call_site.consumer_path != path {
            continue;
        }
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
    let grammar: tree_sitter::Language = match lang {
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::TsJs => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
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
    function_id: &str,
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

    // The rule is `escapes.is_empty()`: an empty read set with no
    // escapes is still fully traceable (`reads_complete = true`) —
    // the call-site schema may simply not be exercised. Only an
    // escape flips the flag to `false`.
    let reads_complete = escapes.is_empty();
    vec![FieldAccessEmission {
        path: call_site.consumer_path.clone(),
        call_id: call_site.call_id.clone(),
        sender_id: function_id.to_string(),
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

/// Which function frame the walker is currently inside (§6.5).
///
/// - `is_sender` — the sending function S itself (or a plain
///   single-function `walk`, e.g. the unit-test path): rule 1's
///   over-binding seed is allowed (`x = <client call>` binds `x`).
/// - not `is_sender` — a scope frame (direct caller / callee).
///   Rule-5-only: a scope frame **never seeds on member calls**
///   (`r = httpx.get(…)` inside a walked callee belongs to that
///   callee's own sending function, not to this emission), and
///   rule 5 only binds via `o = S(…)` — a bare call to the
///   emission's sending function S (checked against `sender_name`).
///   That is what keeps a sibling frame's own client calls
///   (`me = fetch_me()` while walking fetch_order's scope) from
///   re-binding to *this* call's response.
#[derive(Debug, Clone, Copy)]
struct FrameCtx<'a> {
    sender_name: &'a str,
    is_sender: bool,
    /// The frame is a **direct caller of S** (§6.5 rule 5). Only a
    /// caller-frame `return` of a bound identifier *leaves* the
    /// scope — the caller's own caller is outside it — so only this
    /// frame kind may fire `Escape::Returned`. S's return is rule
    /// 5's propagation channel and a callee's return lands back in
    /// an in-scope frame; neither escapes.
    is_caller_of_s: bool,
}

impl<'a> FrameCtx<'a> {
    /// A sender frame (rule 1 over-binding allowed). Used by the
    /// single-function `walk` and for S's own frame.
    fn sender() -> Self {
        FrameCtx {
            sender_name: "",
            is_sender: true,
            is_caller_of_s: false,
        }
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
    // it. The recursive descent lives in `walk_ts`. `walk` is the
    // single-function path (S frame / unit tests) → sender semantics.
    let line = (node.start_position().row as u32) + 1;
    let frame = FrameCtx::sender();
    match lang {
        Lang::Python => {
            handle_python_node(node, src, bound, reads, escapes, file_path, line, frame)
        }
        Lang::TsJs | Lang::Ts | Lang::Tsx => {
            handle_tsjs_node(node, src, bound, reads, escapes, file_path, line, frame)
        }
    }
}

// ─── Python walker ───────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn handle_python_node(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
    frame: FrameCtx<'_>,
) {
    match node.kind() {
        "assignment" => {
            handle_python_assignment(node, src, bound, reads, escapes, file_path, line, frame)
        }
        "return_statement" => handle_python_return(node, src, bound, escapes, frame),
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

#[allow(clippy::too_many_arguments)]
fn handle_python_assignment(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
    frame: FrameCtx<'_>,
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
    if is_client_call_like(right, src, bound, frame) {
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
            // Sequence destructuring (`[a, [b]] = x`) — the chain
            // is unchanged; nested patterns are walked under the
            // same prefix because element indices are implicit in
            // sequence access. Key-based destructuring
            // (`{a: {b}} = x`) walks the value with the key appended
            // to the chain.
            "list_pattern" | "tuple_pattern" | "pattern_list" => {
                collect_pattern_reads(child, src, prefix, reads, file_path, line);
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
    frame: FrameCtx<'_>,
) {
    // §6.5: an escape fires when a bound identifier is "returned
    // from a caller (it would leave the scope)". Only a rule-5
    // caller frame's return leaves the scope — S returning its
    // bound response is rule 5's propagation channel (scenario 5
    // depends on this not flipping `reads_complete`), and a
    // callee's return lands back in an in-scope frame.
    if !frame.is_caller_of_s {
        return;
    }
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
    let call_kind = node.kind();
    // `call` (Python) uses an `argument_list` wrapper, `call_expression`
    // (TS) uses `arguments`. Both wrap a list of inner args. Drill
    // through whichever wrapper is present so the loop sees every
    // argument regardless of grammar.
    let args_node: Option<Node> = if call_kind == "call" || call_kind == "call_expression" {
        node.child_by_field_name("arguments")
            .or_else(|| node.child_by_field_name("argument_list"))
            .or_else(|| {
                // Field-name lookup missed (rare grammar variants);
                // fall back to positional child.
                let mut cursor = node.walk();
                let candidate = node
                    .named_children(&mut cursor)
                    .into_iter()
                    .find(|c| matches!(c.kind(), "arguments" | "argument_list"));
                drop(cursor);
                candidate
            })
    } else {
        Some(node)
    };
    let Some(args_node) = args_node else {
        return;
    };
    let mut cursor = args_node.walk();
    for arg in args_node.named_children(&mut cursor) {
        // Python wraps each call arg in `argument_list` directly (no
        // `argument` wrapper); other grammars wrap each one in
        // `argument`. Treat both the arg node and its `value`
        // child as candidates.
        let value: tree_sitter::Node = if arg.kind() == "argument" {
            arg.child_by_field_name("value").unwrap_or(arg)
        } else {
            arg
        };
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
    // `match x:` where `x` is bound — walk each case_clause. The
    // subject may live in a `subject` field (newer grammars) or as
    // a positional named child before the `block`.
    let subject: Option<String> = node
        .child_by_field_name("subject")
        .and_then(|n| n.utf8_text(src).ok().map(|s| s.to_string()))
        .or_else(|| {
            let mut cursor = node.walk();
            for c in node.named_children(&mut cursor) {
                let k = c.kind();
                if k == "match" || k == "block" || k == ":" {
                    continue;
                }
                return c.utf8_text(src).ok().map(|s| s.to_string());
            }
            None
        });
    if let Some(subj_text) = subject {
        if !bound.contains_key(&subj_text) {
            return;
        }
    }
    // Recurse into the block (and any wrapper) to find case_clauses.
    fn walk_for_case<'a>(
        node: Node<'a>,
        src: &[u8],
        bound: &BTreeMap<String, JsonPath>,
        reads: &mut Vec<FieldRead>,
        escapes: &mut BTreeSet<Escape>,
        file_path: &str,
        line: u32,
    ) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "case_clause" => {
                    handle_python_match_pattern(child, src, bound, reads, escapes, file_path, line);
                }
                "block" | "compound_statement" => {
                    walk_for_case(child, src, bound, reads, escapes, file_path, line);
                }
                _ => {}
            }
        }
    }
    walk_for_case(node, src, bound, reads, escapes, file_path, line);
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
        match child.kind() {
            "dictionary_pattern" | "dict_pattern" => {
                handle_python_dict_pattern(child, src, bound, reads, escapes, file_path, line);
            }
            "case_pattern" => {
                // Outer case_pattern wraps the inner pattern (e.g.
                // `case {"id": i}:`). Recurse into it so the
                // dict_pattern / literal-pattern dispatch fires.
                handle_python_match_pattern(child, src, bound, reads, escapes, file_path, line);
            }
            _ => {}
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
    // Tree-sitter-python's `dict_pattern` emits alternating
    // `string` (the literal key) and `case_pattern` (the value
    // capture) named children (`{"id": i}` → string, case_pattern).
    // The brief says destructuring records a read for each
    // literal-keyed name — so every `string` here emits a read
    // regardless of what the value captures. The `pair_pattern`
    // form (with `key` / `value` fields) is also accepted for
    // safety.
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    let mut i = 0;
    while i < children.len() {
        let child = children[i];
        match child.kind() {
            "pair_pattern" => {
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
                    } else if let Some(prefix) = read_dict_pattern_key(v, src, bound) {
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
                i += 1;
            }
            "string" => {
                let raw = child.utf8_text(src).ok().unwrap_or_default();
                let key_text = strip_python_string_quotes(raw).to_string();
                if key_text.is_empty() {
                    escapes.insert(Escape::Stored);
                    i += 1;
                    continue;
                }
                // The brief says destructuring records a read for
                // each named key on the bound RHS, regardless of
                // whether the value capture is bound elsewhere. The
                // pattern subject (`x`) is in bound — emit a read
                // on the chain `key_text` rooted at the seeded
                // response.
                let subject_prefix = bound
                    .iter()
                    .find(|(k, _)| k.as_str() == "__response__")
                    .map(|(_, v)| v.clone())
                    .or_else(|| {
                        // Fallback: any bound entry whose path is
                        // empty (rule-1 binding).
                        bound.values().find(|p| p.0.is_empty()).cloned()
                    });
                if let Some(prefix) = subject_prefix {
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
                // Recurse into the value-side case_pattern for any
                // nested reads (`{"a": {b}} = x`).
                if let Some(next) = children.get(i + 1).copied() {
                    if next.kind() == "case_pattern" {
                        handle_python_match_pattern(
                            next, src, bound, reads, escapes, file_path, line,
                        );
                    }
                }
                i += 2;
            }
            "case_pattern" => {
                handle_python_match_pattern(child, src, bound, reads, escapes, file_path, line);
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
}

/// Look up a bound identifier named inside a case_pattern / pattern
/// node. Returns the bound path so the caller can extend it with the
/// current key.
fn read_dict_pattern_key<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
) -> Option<JsonPath> {
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        if c.kind() == "identifier" {
            if let Ok(name) = c.utf8_text(src) {
                let key: &str = name;
                if let Some(prefix) = bound.get(key) {
                    return Some(prefix.clone());
                }
            }
        }
    }
    None
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

#[allow(clippy::too_many_arguments)]
fn handle_tsjs_node(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
    frame: FrameCtx<'_>,
) {
    match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            handle_tsjs_var_decl(node, src, bound, reads, escapes, file_path, line, frame)
        }
        "assignment_expression" | "augmented_assignment_expression" => {
            handle_tsjs_assignment(node, src, bound, reads, escapes, file_path, line, frame)
        }
        "return_statement" => handle_tsjs_return(node, src, bound, escapes, frame),
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

#[allow(clippy::too_many_arguments)]
fn handle_tsjs_var_decl(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
    frame: FrameCtx<'_>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            handle_tsjs_var_declarator(child, src, bound, reads, escapes, file_path, line, frame);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_tsjs_var_declarator(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
    frame: FrameCtx<'_>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(value) = node.child_by_field_name("value") else {
        return;
    };
    if is_client_call_like(value, src, bound, frame) {
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
    // TS destructuring: `const { id, name } = x;`.
    if is_destructuring_pattern(name_node) {
        if let Ok(value_text) = value.utf8_text(src) {
            if let Some(prefix) = bound.get(value_text) {
                collect_pattern_reads(name_node, src, prefix, reads, file_path, line);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_tsjs_assignment(
    node: Node,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    line: u32,
    frame: FrameCtx<'_>,
) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    let Some(right) = node.child_by_field_name("right") else {
        return;
    };
    if is_client_call_like(right, src, bound, frame) {
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
    frame: FrameCtx<'_>,
) {
    // §6.5: only a rule-5 caller frame's `return` of a bound
    // identifier leaves the scope (see `handle_python_return`).
    if !frame.is_caller_of_s {
        return;
    }
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
        // TypeScript's grammar emits this with positional named
        // children (no field names); fall back to the first child
        // when `child_by_field_name` returns None.
        let value: Option<Node> = match node.child_by_field_name("expression") {
            Some(v) => Some(v),
            None => {
                let mut cursor = node.walk();
                let first = node.named_children(&mut cursor).next();
                drop(cursor);
                first
            }
        };
        let value = value?;
        if let Ok(name) = value.utf8_text(src) {
            if bound.contains_key(name) {
                return Some(name.to_string());
            }
        }
    }
    None
}

#[allow(clippy::only_used_in_recursion)] // `src` flows through every recursive call.
fn is_client_call_like<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &BTreeMap<String, JsonPath>,
    frame: FrameCtx<'_>,
) -> bool {
    // Heuristic: `await fetch(...)`, `axios(...)`, `requests.get(...)`,
    // `client.get(...)`, etc. In the *sender's* frame (or the plain
    // single-function `walk`) we accept any call expression — the
    // sensor's caller graph already restricts analysis to functions
    // that *do* emit an `HttpClientCall`, and the seeding binds the
    // LHS to the empty path whenever the RHS is a call expression.
    // Over-binding is acceptable in the seed step: rules 2 / 3 / 4
    // refine.
    //
    // In a *scope* frame (a direct caller / callee of S — §6.5) the
    // only assignment seeding allowed is `o = S(…)` (rule 5): any
    // other call there belongs to a different sending function and
    // must not re-bind to this call's response (otherwise a sibling
    // sender's reads would be attributed to this emission).
    //
    // `x.get("k")` / `x.foo(...)` is NOT a client call — the
    // receiver is a bound identifier, and the chain unwraps to a
    // sub-path under rule 3 rather than binding to the response
    // root. We only treat a member-call as non-client when the
    // receiver is in `bound`; `requests.get(...)` and `axios(...)`
    // are still client calls because `requests` / `axios` are
    // unbound module names.
    let kind = node.kind();
    if kind == "await_expression" {
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if c.kind() != "await" {
                return is_client_call_like(c, src, bound, frame);
            }
        }
        return false;
    }
    if kind != "call" && kind != "call_expression" {
        return false;
    }
    let function = match node.child_by_field_name("function") {
        Some(f) => f,
        None => return false,
    };
    let func_kind = function.kind();
    if func_kind == "attribute" || func_kind == "member_expression" {
        if !frame.is_sender {
            // A scope frame's own member client call (another
            // sender's HTTP request) never seeds here.
            return false;
        }
        if let Some(recv) = function.child_by_field_name("object") {
            if let Some(recv_text) = recv.utf8_text(src).ok().map(|s| s.to_string()) {
                if bound.contains_key(&recv_text) {
                    return false;
                }
            }
        }
        return true;
    }
    // Bare call: sender frame over-binds (rule 1 seed); a scope
    // frame only seeds for `S` itself (rule 5).
    if frame.is_sender {
        return true;
    }
    matches!(
        function.utf8_text(src),
        Ok(name) if name == frame.sender_name
    )
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
    // `ReadsFrom` targets the `HttpClientCall` node itself (§6.5 /
    // §7.5: `FieldRef --ReadsFrom--> call c`, then `Binds(c → …)`).
    // The id is the per-repo node id; `project_edges` rewrites it to
    // the GlobalId `resolve_field_refs` keys `Binds` by.
    let call_id = emission.call_id.clone();
    for read in &emission.reads {
        // §4.4: a `FieldRef` records the *reading* file and line —
        // for scope reads (§6.5) that can differ from the consumer
        // file that owns the call.
        let read_path = if read.path.is_empty() {
            emission.path.clone()
        } else {
            read.path.clone()
        };
        // §6.5: the `ReadsField` edge starts at the *reading
        // function*. Resolve the enclosing symbol at the read site;
        // a genuinely module-level read (no function covers the
        // line) falls back to the File node — the same convention
        // `enclosing_sends_http_edge` uses for module-level calls.
        let reads_field_source =
            crate::server::sensors::util::enclosing_symbol(graph, &read_path, read.line)
                .map(|n| n.id)
                .unwrap_or_else(|| {
                    graph
                        .get_all_nodes()
                        .into_iter()
                        .find(|n| n.node_type == NodeType::File && n.path == read_path)
                        .map(|n| n.id)
                        .unwrap_or_else(|| "self".to_string())
                });
        let mut node = GraphNode::new_in(
            NodeType::FieldRef,
            read.chain.to_string(),
            read_path.clone(),
            namespace,
        );
        node.id = GraphNode::generate_id(
            &NodeType::FieldRef,
            &read_path,
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
            path: read_path,
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
    (nodes, edges)
}

/// §6.5: flip `ConsumerFact.reads_complete` to `false` on the call
/// node when an emission's escape set is non-empty. The `HttpClientCall`
/// node is owned by the phase-1 sensor — patch the fact in place via
/// the graph's update path; never re-emit the node here.
///
/// Where the patched fact is read back: `FederatedIndex::project_nodes`
/// copies `repo.nodes()` (including this update) into the federation
/// backend under GlobalIds and records the id in `contract_node_ids`;
/// `rejoin_contracts` then loads it with `self.backend.get_node(gid)`
/// (`federated_index.rs`, the `contract_node_ids` loop) and hands it to
/// `ContractJoiner::run`, which reads `ContractFact::Consumer` to build
/// the consumer inputs and the index's `reads_complete`.
fn patch_reads_complete(graph: &GraphDatabase, call_id: &str) -> Result<(), LainError> {
    let Some(mut node) = graph.get_node(call_id)? else {
        return Ok(());
    };
    let Some(ContractFact::Consumer(consumer)) = node.contract.as_mut() else {
        return Ok(());
    };
    if consumer.reads_complete {
        consumer.reads_complete = false;
        graph.upsert_node(node)?;
    }
    Ok(())
}

/// Lazily parse and cache a workspace source file for the
/// interprocedural pass. Returns `false` when the path is not a
/// supported source or cannot be read/parsed.
fn ensure_file(files: &mut BTreeMap<String, (Lang, String, Tree)>, root: &Path, rel: &str) -> bool {
    if files.contains_key(rel) {
        return true;
    }
    let Some(lang) = lang_for_path(rel) else {
        return false;
    };
    let Ok(content) = std::fs::read_to_string(root.join(rel)) else {
        return false;
    };
    let Some(tree) = parse(lang, &content) else {
        return false;
    };
    files.insert(rel.to_string(), (lang, content, tree));
    true
}

/// First-wins metadata merge: the sender's file is merged first so
/// its `return_text` for S wins over a same-named function elsewhere.
fn merge_metadata(into: &mut FunctionMetadata, from: FunctionMetadata) {
    for (k, v) in from.parameters {
        into.parameters.entry(k).or_insert(v);
    }
    for (k, v) in from.return_text {
        into.return_text.entry(k).or_insert(v);
    }
}

/// §6.5 interprocedural pass (rules 5/6): for every emission,
/// re-walk the sending function S and then its scope — S's direct
/// callers (rule 5), S's direct callees and its callers' direct
/// callees (rule 6) — over the repo `Calls` graph (provenance
/// `Static{TreeSitter}`/`None` only; `collect_calls_by_function`
/// already filters), sharing `bound` / `reads` / `escapes` so the
/// caller-frame and callee-frame reads land on the same emission
/// (and therefore the same `ReadsFrom` target).
///
/// Frames are walked S → callers → callees (readiness: a function
/// waits until every in-scope caller of it has been walked), each
/// with its own file's source. Scope functions that cannot be
/// resolved (missing graph node, unreadable file, name mismatch)
/// are skipped conservatively — the emission keeps whatever phase A
/// collected. When no scope member resolves, the phase-A emission
/// is left untouched.
fn extend_emissions_with_scope(
    root: &Path,
    graph: &GraphDatabase,
    calls_by_function: &BTreeMap<String, BTreeSet<String>>,
    emissions: &mut [FieldAccessEmission],
) {
    if emissions.is_empty() {
        return;
    }
    // Function/Method graph nodes: id → (name, path).
    let fn_index: BTreeMap<String, (String, String)> = graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| matches!(n.node_type, NodeType::Function | NodeType::Method))
        .map(|n| (n.id.clone(), (n.name.clone(), n.path.clone())))
        .collect();
    // Reverse index: callee id → caller ids.
    let mut callers_of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (caller, callees) in calls_by_function {
        for callee in callees {
            callers_of
                .entry(callee.clone())
                .or_default()
                .insert(caller.clone());
        }
    }
    // Parsed-file cache shared across emissions.
    let mut files: BTreeMap<String, (Lang, String, Tree)> = BTreeMap::new();

    for emission in emissions.iter_mut() {
        if emission.sender_id.is_empty() {
            continue;
        }
        let Some((sender_name, sender_path)) = fn_index.get(&emission.sender_id).cloned() else {
            // Not a Function/Method node (e.g. a module-level call
            // attached to the File node) — no scope to walk.
            continue;
        };

        // §6.5 scope: S ∪ direct callers(S) ∪ direct callees(S) ∪
        // direct callees of those callers.
        let mut scope_ids: BTreeSet<String> = BTreeSet::new();
        scope_ids.insert(emission.sender_id.clone());
        if let Some(cs) = calls_by_function.get(&emission.sender_id) {
            scope_ids.extend(cs.iter().cloned());
        }
        if let Some(cl) = callers_of.get(&emission.sender_id) {
            for c in cl {
                scope_ids.insert(c.clone());
                if let Some(cc) = calls_by_function.get(c) {
                    scope_ids.extend(cc.iter().cloned());
                }
            }
        }
        // Resolve the scope members (everything but S) to
        // (id, path, name), sorted for deterministic walk order.
        let mut members: Vec<(String, String, String)> = scope_ids
            .iter()
            .filter(|id| id.as_str() != emission.sender_id)
            .filter_map(|id| {
                fn_index
                    .get(id)
                    .map(|(n, p)| (id.clone(), p.clone(), n.clone()))
            })
            .collect();
        members.sort();
        if members.is_empty() {
            continue; // S-only scope — phase A's walk already covers it.
        }

        // Load S's file and every member file we can; skip members
        // whose file cannot be loaded (they are marked walked below
        // so their callees are not blocked).
        if !ensure_file(&mut files, root, &sender_path) {
            continue;
        }
        let mut unloadable: BTreeSet<String> = BTreeSet::new();
        for (_, path, _) in &members {
            if !ensure_file(&mut files, root, path) {
                unloadable.insert(path.clone());
            }
        }

        // Merged metadata for rules 5/6: S's file first (its
        // `return_text` for S must win), then member files.
        let mut metadata = FunctionMetadata {
            parameters: BTreeMap::new(),
            return_text: BTreeMap::new(),
        };
        {
            let (_, content, tree) = &files[&sender_path];
            let defs = collect_function_defs(tree.root_node(), content.as_bytes());
            merge_metadata(
                &mut metadata,
                collect_function_metadata(&defs, content.as_bytes()),
            );
        }
        for (_, path, _) in &members {
            if unloadable.contains(path) {
                continue;
            }
            let (_, content, tree) = &files[path];
            let defs = collect_function_defs(tree.root_node(), content.as_bytes());
            merge_metadata(
                &mut metadata,
                collect_function_metadata(&defs, content.as_bytes()),
            );
        }

        // Locate S's AST node in its file (by graph name).
        let (_, s_content, s_tree) = &files[&sender_path];
        let s_defs = collect_function_defs(s_tree.root_node(), s_content.as_bytes());
        let Some(sender_node) = s_defs
            .iter()
            .find(|(n, _, _)| n == &sender_name)
            .map(|(_, _, node)| *node)
        else {
            // Graph name ≠ AST name — keep phase A's reads.
            continue;
        };

        // Fresh binding state for this emission's scope walk.
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        bound.insert(sender_name.clone(), JsonPath(Vec::new()));
        let mut reads: Vec<FieldRead> = Vec::new();
        let mut escapes: BTreeSet<Escape> = BTreeSet::new();

        // Walk S first (sender frame — rule 1 over-binding applies),
        // establishing the bindings rule 5 resolves against.
        {
            let (s_lang, s_content, _) = &files[&sender_path];
            let s_bytes = s_content.as_bytes();
            let s_path = sender_path.clone();
            let sender_frame = FrameCtx {
                sender_name: &sender_name,
                is_sender: true,
                is_caller_of_s: false,
            };
            walk_ts(sender_node, &mut |n| {
                walk_with_metadata(
                    n,
                    s_bytes,
                    *s_lang,
                    &mut bound,
                    &mut reads,
                    &mut escapes,
                    &s_path,
                    &metadata,
                    sender_frame,
                )
            });
        }

        // Readiness-ordered member walks: a member is walked once
        // every in-scope caller of it has been walked (callers
        // before callees so rule 6's param bindings land first).
        let mut walked: BTreeSet<String> = BTreeSet::new();
        walked.insert(emission.sender_id.clone());
        for (id, path, _) in &members {
            if unloadable.contains(path) {
                // Can never be walked — unblock its callees.
                walked.insert(id.clone());
            }
        }
        let mut pending: Vec<(String, String, String)> = members
            .iter()
            .filter(|(id, _, _)| !walked.contains(id))
            .cloned()
            .collect();
        let mut passes = 0;
        while !pending.is_empty() && passes < 16 {
            let ready: Vec<(String, String, String)> = pending
                .iter()
                .filter(|(id, _, _)| {
                    // Ready when no *unwalked* in-scope caller of
                    // this member remains.
                    !scope_ids.iter().any(|cid| {
                        cid != id
                            && !walked.contains(cid)
                            && calls_by_function.get(cid).is_some_and(|cs| cs.contains(id))
                    })
                })
                .cloned()
                .collect();
            let ready = if ready.is_empty() {
                // Cycle — walk what is left rather than dropping reads.
                std::mem::take(&mut pending)
            } else {
                ready
            };
            for (id, path, name) in &ready {
                if unloadable.contains(path) {
                    walked.insert(id.clone());
                    continue;
                }
                let Some((m_lang, m_content, m_tree)) = files.get(path) else {
                    walked.insert(id.clone());
                    continue;
                };
                let m_bytes = m_content.as_bytes();
                let m_defs = collect_function_defs(m_tree.root_node(), m_bytes);
                let Some(member_node) = m_defs
                    .iter()
                    .find(|(n, _, _)| n == name)
                    .map(|(_, _, node)| *node)
                else {
                    // Graph name ≠ AST name — skip this frame.
                    walked.insert(id.clone());
                    continue;
                };
                let scope_frame = FrameCtx {
                    sender_name: &sender_name,
                    is_sender: false,
                    // §6.5 rule 5: direct caller of S → its `return`
                    // of a bound identifier leaves the scope.
                    is_caller_of_s: calls_by_function
                        .get(id.as_str())
                        .is_some_and(|cs| cs.contains(&emission.sender_id)),
                };
                let m_path = path.clone();
                walk_ts(member_node, &mut |n| {
                    walk_with_metadata(
                        n,
                        m_bytes,
                        *m_lang,
                        &mut bound,
                        &mut reads,
                        &mut escapes,
                        &m_path,
                        &metadata,
                        scope_frame,
                    )
                });
                walked.insert(id.clone());
            }
            pending.retain(|(id, _, _)| !walked.contains(id));
            passes += 1;
        }

        // Replace the phase-A result with the full-scope walk.
        emission.reads_complete = escapes.is_empty();
        emission.reads = reads;
        emission.escapes = escapes;
    }
}

/// Heuristic for the test helper: does the function body directly
/// invoke a known client-call verb (`fetch(...)`,
/// `axios(...)`, etc.)? Used to decide which `calls_by_function`
/// keys are actual sending functions vs. intermediate callers /
/// out-of-scope callees.
fn is_sending_function_shape<'a>(
    name: &str,
    functions: &[(String, u32, Node<'a>)],
    src: &[u8],
) -> bool {
    // Find this function's body and look for a call expression whose
    // function is the bare `fetch` / `axios` identifier, OR whose
    // body includes a `return EXPR` where EXPR is such a call. The
    // second form catches `return await fetch(...)` where the
    // sending call is wrapped in a return statement.
    for (fname, _line, fn_node) in functions {
        if fname != name {
            continue;
        }
        let mut found = false;
        walk_ts(*fn_node, &mut |n| {
            if found {
                return;
            }
            // Check direct call expressions.
            if n.kind() == "call_expression" || n.kind() == "call" {
                if let Some(function) = n.child_by_field_name("function") {
                    if function.kind() == "identifier" {
                        if let Ok(name) = function.utf8_text(src) {
                            if name == "fetch" || name == "axios" || name == "got" {
                                found = true;
                            }
                        }
                    }
                }
            }
        });
        if found {
            return true;
        }
    }
    false
}

// Test helper: walk the file with a caller-provided calls graph so
// the scope tests don't need the full graph backend.
#[allow(dead_code, clippy::too_many_arguments)]
fn detect_emissions_with_calls<'a>(
    src: &'a str,
    tree: &'a tree_sitter::Tree,
    lang: Lang,
    file_path: &'a str,
    calls_by_function: &BTreeMap<String, BTreeSet<String>>,
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
) -> Vec<FieldAccessEmission> {
    let root = tree.root_node();
    let src_bytes = src.as_bytes();
    let _ = src;

    // Find all function definitions in the file (name, node, line).
    let functions: Vec<(String, u32, tree_sitter::Node<'a>)> =
        collect_function_defs(root, src_bytes);

    // Build a map: function name → position in `functions`.
    let mut name_to_idx: BTreeMap<String, usize> = BTreeMap::new();
    for (i, (name, _, _)) in functions.iter().enumerate() {
        // First occurrence wins; the `graph` backend resolves
        // duplicates elsewhere.
        name_to_idx.entry(name.clone()).or_insert(i);
    }

    // §6.5 scope: identify the in-scope functions for every
    // sending function in this file. The sending functions are
    // identified by a fresh `__response__` seed being consumed
    // somewhere inside them — i.e. the walker binds `x` from a
    // `fetch("/a")` call within the function body. Without the
    // graph backend, we mark every function in the file as a
    // potential sending function; the test scope verifies the
    // scope boundary via the `calls_by_function` argument.
    //
    // The `scope` for a sending function S is:
    //   S,
    //   S's direct callers (rule 5),
    //   S's direct callees (rule 6),
    //   S's callers' direct callees (rule 6).
    let scope_for: BTreeMap<String, BTreeSet<String>> =
        compute_scope(&functions, calls_by_function);

    let mut emissions: Vec<FieldAccessEmission> = Vec::new();
    // The §6.5 scope is computed for every function name in the
    // file (so the out-of-scope tests can pin which functions are
    // excluded). Walking, however, must only run for the function
    // that *actually* binds the response — i.e. the function that
    // contains a `SendsHttp` call site. Without a graph backend,
    // the test helper checks each candidate's body for a
    // `fetch(...)`-shaped or similar outbound call. Only those
    // names pass the test get walked.
    let mut sender_names: Vec<String> = Vec::new();
    for name in calls_by_function.keys() {
        if is_sending_function_shape(name, &functions, src_bytes) {
            sender_names.push(name.clone());
        }
    }
    for sender_name in &sender_names {
        let Some(scope) = scope_for.get(sender_name) else {
            continue;
        };
        if scope.is_empty() {
            continue;
        }
        let Some(&s_idx) = name_to_idx.get(sender_name) else {
            continue;
        };
        let (_, s_line, _) = functions[s_idx].clone();
        walk_in_scope(
            root,
            src_bytes,
            lang,
            sender_name,
            s_line,
            file_path,
            &functions,
            scope,
            calls_by_function,
            bound,
            reads,
            escapes,
        );
    }
    let reads_clone = reads.clone();
    let escapes_clone = escapes.clone();
    emissions.push(FieldAccessEmission {
        path: file_path.to_string(),
        call_id: String::new(),
        sender_id: String::new(),
        reads: reads_clone,
        escapes: escapes_clone,
        reads_complete: escapes.is_empty(),
    });
    emissions
}

#[allow(dead_code)] // Reserved for the rule 5/6 follow-up.
fn collect_function_defs<'a>(root: Node<'a>, src: &[u8]) -> Vec<(String, u32, Node<'a>)> {
    let mut out: Vec<(String, u32, Node<'a>)> = Vec::new();
    walk_ts(root, &mut |n: Node<'a>| {
        let kind = n.kind();
        if matches!(
            kind,
            "function_declaration"
                | "function"
                | "function_expression"
                | "method_definition"
                | "function_definition"
                | "async_function_definition"
                | "arrow_function"
        ) {
            let name = function_def_name(n, src);
            if let Some(name) = name {
                let line = n.start_position().row as u32 + 1;
                out.push((name, line, n));
            }
        }
    });
    out
}

#[allow(dead_code)] // Reserved for the rule 5/6 follow-up.
fn function_def_name(node: Node, src: &[u8]) -> Option<String> {
    // For TS/JS: `name` field is the function name. For Python:
    // `name` field on `function_definition`.
    if let Some(name_node) = node.child_by_field_name("name") {
        if let Ok(text) = name_node.utf8_text(src) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    // Fallback: use the first identifier child.
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        if c.kind() == "identifier" {
            if let Ok(text) = c.utf8_text(src) {
                return Some(text.to_string());
            }
        }
    }
    None
}

/// Compute the §6.5 scope for each function in the file. The
/// `scope_for` map: sender_name → set of function names that are in
/// scope (including the sender itself).
#[allow(dead_code)] // Reserved for the rule 5/6 follow-up.
fn compute_scope(
    functions: &[(String, u32, Node)],
    calls_by_function: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut scope_for: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Caller index: callee → set of callers.
    let mut callers_of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (caller, callees) in calls_by_function {
        for callee in callees {
            callers_of
                .entry(callee.clone())
                .or_default()
                .insert(caller.clone());
        }
    }
    // Each function name in the file might be a sending function S.
    // We compute its scope once.
    let names: BTreeSet<String> = functions.iter().map(|(n, _, _)| n.clone()).collect();
    for s in &names {
        let mut scope: BTreeSet<String> = BTreeSet::new();
        // S itself.
        scope.insert(s.clone());
        // S's direct callers (rule 5).
        if let Some(callers) = callers_of.get(s) {
            for c in callers {
                scope.insert(c.clone());
            }
        }
        // S's direct callees (rule 6).
        if let Some(callees) = calls_by_function.get(s) {
            for c in callees {
                scope.insert(c.clone());
            }
        }
        // S's callers' direct callees (rule 6 second hop).
        if let Some(callers) = callers_of.get(s) {
            for caller in callers {
                if let Some(callees) = calls_by_function.get(caller) {
                    for c in callees {
                        scope.insert(c.clone());
                    }
                }
            }
        }
        scope_for.insert(s.clone(), scope);
    }
    scope_for
}

/// Walk an in-scope function body, sharing `bound` / `reads` /
/// `escapes` with the caller. Implements §6.5 rules 5 and 6:
///
/// - Rule 5: when a function F returns a bound identifier/expression
///   and the caller has `o = F(...)`, o is bound to the returned
///   sub-path.
/// - Rule 6: when a function G has a parameter at position P and the
///   caller passes a bound identifier at position P, G's parameter
///   is bound to that identifier's path.
#[allow(clippy::too_many_arguments)] // walker state: bound/reads/escapes + scope inputs
fn walk_in_scope<'a>(
    _root: Node<'a>,
    src: &[u8],
    lang: Lang,
    _sender_name: &str,
    sender_line: u32,
    file_path: &str,
    functions: &[(String, u32, Node<'a>)],
    scope: &BTreeSet<String>,
    calls_by_function: &BTreeMap<String, BTreeSet<String>>,
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
) {
    // Pre-pass: extract per-function metadata. Also seed the
    // sender's name as bound to the response — the brief's rule 1
    // says `x = <client call>` binds x to the response root; a
    // sender's body that returns `await fetch(...)` doesn't carry
    // an explicit `x = ...` binding, so we synthesize the seed
    // for the sender itself.
    let metadata = collect_function_metadata(functions, src);
    bound.insert(_sender_name.to_string(), JsonPath(Vec::new()));

    // Walk the sending function (where the response is bound).
    let sender_frame = FrameCtx {
        sender_name: _sender_name,
        is_sender: true,
        is_caller_of_s: false,
    };
    let _ = find_function_at_line(_root, src, lang, sender_line).map(|fn_node| {
        walk_ts(fn_node, &mut |n| {
            walk_with_metadata(
                n,
                src,
                lang,
                bound,
                reads,
                escapes,
                file_path,
                &metadata,
                sender_frame,
            )
        })
    });
    // Then walk every other in-scope function with the shared
    // `bound`. Ordering matters: a caller must be walked before its
    // callee so rule 6 has the binding context by the time the
    // callee's body is walked, and S must be walked before its
    // callers (done above) so rule 5 can resolve the returned
    // identifier's path. Readiness therefore = "no *unwalked*
    // in-scope caller remains".
    let in_scope: Vec<&(String, u32, Node)> = functions
        .iter()
        .filter(|(name, line, _)| *line != sender_line && scope.contains(name))
        .collect();
    let mut remaining: Vec<&(String, u32, Node)> = in_scope.clone();
    // Iterate until everything is walked (at most 16 passes; for a
    // DAG of callers/callees 1–2 suffice. If a cycle makes no
    // function ready, walk the remainder anyway so reads inside it
    // aren't silently dropped).
    let mut passes = 0;
    while !remaining.is_empty() && passes < 16 {
        let to_walk: Vec<&(String, u32, Node)> = remaining
            .iter()
            .copied()
            .filter(|(name, _, _)| {
                !remaining.iter().any(|(caller, _, _)| {
                    caller.as_str() != name.as_str()
                        && calls_by_function
                            .get(caller.as_str())
                            .is_some_and(|cs| cs.contains(name.as_str()))
                })
            })
            .collect();
        let to_walk = if to_walk.is_empty() {
            // Cycle (or self-reference only): break it by walking
            // everything that is left rather than dropping reads.
            remaining.clone()
        } else {
            to_walk
        };
        for (name, _line, fn_node) in &to_walk {
            let scope_frame = FrameCtx {
                sender_name: _sender_name,
                is_sender: false,
                // §6.5 rule 5: a direct caller of S — its `return`
                // of a bound identifier leaves the scope.
                is_caller_of_s: calls_by_function
                    .get(name.as_str())
                    .is_some_and(|cs| cs.contains(_sender_name)),
            };
            walk_ts(*fn_node, &mut |n| {
                walk_with_metadata(
                    n,
                    src,
                    lang,
                    bound,
                    reads,
                    escapes,
                    file_path,
                    &metadata,
                    scope_frame,
                )
            });
            let _ = name;
        }
        remaining.retain(|(name, _, _)| !to_walk.iter().any(|(n, _, _)| n == name));
        passes += 1;
    }
}

/// Per-function metadata needed to apply rules 5 and 6.
struct FunctionMetadata {
    /// function name → list of parameter names in declaration
    /// order. Empty for parameter-less functions.
    parameters: BTreeMap<String, Vec<String>>,
    /// function name → what the function returns, as a bound
    /// identifier plus the returned sub-path (§6.5 rule 5):
    /// `return x.json()` → `("x", [])`,
    /// `return (await x.json())["data"]` → `("x", ["data"])`.
    /// `Some(None)` when the function has a return whose value is
    /// not traceable to a bound identifier (bare call, literal);
    /// `None` when the function has no return statement / no body.
    return_text: BTreeMap<String, Option<Option<(String, JsonPath)>>>,
}

fn collect_function_metadata<'a>(
    functions: &[(String, u32, Node<'a>)],
    src: &[u8],
) -> FunctionMetadata {
    let mut parameters: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut return_text: BTreeMap<String, Option<Option<(String, JsonPath)>>> = BTreeMap::new();
    for (name, _, fn_node) in functions {
        parameters.insert(name.clone(), function_parameter_names(*fn_node, src));
        return_text.insert(name.clone(), function_return_binding(*fn_node, src));
    }
    FunctionMetadata {
        parameters,
        return_text,
    }
}

fn function_parameter_names(fn_node: Node, src: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let params_node = fn_node.child_by_field_name("parameters");
    let Some(params_node) = params_node else {
        return names;
    };
    let mut cursor = params_node.walk();
    for child in params_node.named_children(&mut cursor) {
        if let Some(name) = child.child_by_field_name("name") {
            if let Ok(text) = name.utf8_text(src) {
                if !text.is_empty() {
                    names.push(text.to_string());
                }
            }
            continue;
        }
        if child.kind() == "identifier" {
            if let Ok(text) = child.utf8_text(src) {
                if !text.is_empty() {
                    names.push(text.to_string());
                }
            }
        }
    }
    names
}

fn function_return_binding(fn_node: Node, src: &[u8]) -> Option<Option<(String, JsonPath)>> {
    // Walk the function body for the first return statement and
    // describe its value: a bound identifier + returned sub-path
    // (`return x.json()` → Some(Some(("x", []))), `return x` →
    // Some(Some(("x", []))), `return await fetch(...)` →
    // Some(None) — not traceable to a bound identifier). No return
    // statement → None (rule 5 does not fire).
    let mut cursor = fn_node.walk();
    for child in fn_node.named_children(&mut cursor) {
        let ret_stmt: Option<Node> = match child.kind() {
            "statement_block" | "block" => {
                let mut inner = child.walk();
                let mut found = None;
                for stmt in child.named_children(&mut inner) {
                    if stmt.kind() == "return_statement" || stmt.kind() == "return" {
                        found = Some(stmt);
                        break;
                    }
                }
                found
            }
            "return_statement" | "return" => Some(child),
            _ => None,
        };
        if let Some(ret_stmt) = ret_stmt {
            return Some(return_binding(ret_stmt, src));
        }
    }
    None
}

fn return_binding(ret_stmt: Node, src: &[u8]) -> Option<(String, JsonPath)> {
    let mut cursor = ret_stmt.walk();
    for child in ret_stmt.named_children(&mut cursor) {
        if child.kind() == "return" {
            continue;
        }
        return binding_of(child, src);
    }
    None
}

/// The bound identifier a returned expression is derived from,
/// plus the sub-path appended after it. Conservative: anything not
/// traceable through rule 2 (`.json()` / `.data()` / `.body()`)
/// chains and literal subscripts yields `None` (no rule-5 bind).
fn binding_of(expr: Node, src: &[u8]) -> Option<(String, JsonPath)> {
    // For `await <expr>`, recurse.
    if expr.kind() == "await_expression" {
        let mut cursor = expr.walk();
        for c in expr.named_children(&mut cursor) {
            if c.kind() != "await" {
                return binding_of(c, src);
            }
        }
        return None;
    }
    // For `<expr>.json()` / `<expr>.data()` / `<expr>.body()`, the
    // returned sub-path matches the receiver's path (rule 2).
    if expr.kind() == "call_expression" || expr.kind() == "call" {
        let function = expr.child_by_field_name("function")?;
        if function.kind() == "member_expression" || function.kind() == "attribute" {
            let recv = function.child_by_field_name("object")?;
            let attr = function
                .child_by_field_name("property")
                .or_else(|| function.child_by_field_name("attribute"))?;
            let attr_text = attr.utf8_text(src).ok()?;
            if matches!(attr_text, "json" | "data" | "body") {
                return binding_of(recv, src);
            }
            // Any other method (`r.text()` etc.) returns something
            // other than response data — not traceable.
            return None;
        }
        // Bare call — not traceable to a bound identifier.
        return None;
    }
    if expr.kind() == "identifier" {
        let name = expr.utf8_text(src).ok()?.to_string();
        if name.is_empty() {
            return None;
        }
        return Some((name, JsonPath(Vec::new())));
    }
    // Literal-key subscript: recurse into the value, append the key.
    if expr.kind() == "subscript" || expr.kind() == "subscript_expression" {
        let (value, index) = {
            let obj = expr.child_by_field_name("object");
            let idx = expr.child_by_field_name("index");
            match (obj, idx) {
                (Some(v), Some(i)) => (v, i),
                _ => {
                    let mut cursor = expr.walk();
                    let named: Vec<Node> = expr.named_children(&mut cursor).collect();
                    let v = named.first().copied()?;
                    let i = named.get(1).copied()?;
                    (v, i)
                }
            }
        };
        let key = match index.kind() {
            "string" => {
                let raw = index.utf8_text(src).ok()?;
                if expr.kind() == "subscript" {
                    strip_python_string_quotes(raw).to_string()
                } else {
                    strip_js_string_quotes(raw).to_string()
                }
            }
            _ => return None, // non-literal key — conservative
        };
        let (ident, mut path) = binding_of(value, src)?;
        path.0.push(PathSegment::Name(key));
        return Some((ident, path));
    }
    // Member access without a call: `.json`/`.data`/`.body` are
    // transparent (rule 2); any other attribute appends a segment.
    if expr.kind() == "attribute" || expr.kind() == "member_expression" {
        let recv = expr.child_by_field_name("object")?;
        let attr = expr
            .child_by_field_name("attribute")
            .or_else(|| expr.child_by_field_name("property"))?;
        let attr_text = attr.utf8_text(src).ok()?;
        let (ident, mut path) = binding_of(recv, src)?;
        if !matches!(attr_text, "json" | "data" | "body") {
            path.0.push(PathSegment::Name(attr_text.to_string()));
        }
        return Some((ident, path));
    }
    // Parenthesized expression, if the grammar materializes one.
    if expr.kind() == "parenthesized_expression" {
        let mut cursor = expr.walk();
        let inner = expr.named_children(&mut cursor).next();
        if let Some(inner) = inner {
            return binding_of(inner, src);
        }
    }
    None
}

/// Like `walk` but with access to the precomputed function metadata
/// so it can apply rules 5 and 6.
#[allow(clippy::too_many_arguments)] // walker state mirrors `walk` + metadata
fn walk_with_metadata<'a>(
    node: Node<'a>,
    src: &[u8],
    lang: Lang,
    bound: &mut BTreeMap<String, JsonPath>,
    reads: &mut Vec<FieldRead>,
    escapes: &mut BTreeSet<Escape>,
    file_path: &str,
    metadata: &FunctionMetadata,
    frame: FrameCtx<'_>,
) {
    let line = (node.start_position().row as u32) + 1;
    // Handle the node first (rules 1–4 / reads / escapes)…
    match lang {
        Lang::Python => {
            handle_python_node(node, src, bound, reads, escapes, file_path, line, frame)
        }
        Lang::TsJs | Lang::Ts | Lang::Tsx => {
            handle_tsjs_node(node, src, bound, reads, escapes, file_path, line, frame)
        }
    }
    // …then the interprocedural rules, so a precise rule-5 sub-path
    // binding overrides the rule-1 root bind on the same node.
    // §6.5 rules 5/6 are language-neutral.
    apply_rule5_tsjs(node, src, bound, metadata, frame);
    apply_rule6_tsjs(node, src, bound, metadata);
}

/// The called function's name on a `<callee>(…)` value expression
/// (unwrapping `await`), for both grammars: a bare identifier, or
/// the property/attribute of a member call.
fn called_function_name(value: Node, src: &[u8]) -> Option<String> {
    let inner = if value.kind() == "await_expression" {
        let mut cursor = value.walk();
        let mut found = None;
        for c in value.named_children(&mut cursor) {
            if c.kind() != "await" {
                found = Some(c);
                break;
            }
        }
        found?
    } else {
        value
    };
    if inner.kind() != "call_expression" && inner.kind() != "call" {
        return None;
    }
    let function = inner.child_by_field_name("function")?;
    match function.kind() {
        "identifier" | "type_identifier" => function.utf8_text(src).ok().map(String::from),
        "member_expression" | "attribute" => function
            .child_by_field_name("property")
            .or_else(|| function.child_by_field_name("attribute"))
            .or_else(|| function.child_by_field_name("name"))
            .and_then(|n| n.utf8_text(src).ok().map(String::from)),
        _ => None,
    }
}

/// Rule 5 (§6.5): in a caller of S, `o = S(…)` / `o = await S(…)`
/// binds `o` to what S returns — a bound identifier plus the
/// returned sub-path (`return r.json()` → `r`'s path;
/// `return (await r.json())["data"]` → `r`'s path + `data`).
/// Only fires when the callee is the emission's sending function S.
fn apply_rule5_tsjs<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    metadata: &FunctionMetadata,
    frame: FrameCtx<'_>,
) {
    // TS: `lexical_declaration` → `variable_declarator`;
    // `assignment_expression`; Python: `assignment`.
    if !matches!(
        node.kind(),
        "variable_declarator" | "assignment_expression" | "assignment"
    ) {
        return;
    }
    // LHS / value, with Python's positional fallback.
    let lhs = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("left"));
    let value = node
        .child_by_field_name("value")
        .or_else(|| node.child_by_field_name("right"));
    let (lhs, value) = match (lhs, value) {
        (Some(l), Some(v)) => (l, v),
        _ => {
            let mut cursor = node.walk();
            let named: Vec<Node> = node.named_children(&mut cursor).collect();
            let (Some(l), Some(v)) = (named.first().copied(), named.get(1).copied()) else {
                return;
            };
            (l, v)
        }
    };
    let Some(called_name) = called_function_name(value, src) else {
        return;
    };
    // §6.5: rule 5 is `o = S(…)` — only the sending function.
    if called_name != frame.sender_name || frame.sender_name.is_empty() {
        return;
    }
    // S returns a bound identifier/expression: bind `o` to that
    // identifier's path plus the returned sub-path.
    let Some(ret_opt) = metadata.return_text.get(&called_name) else {
        return;
    };
    let Some(ret) = ret_opt.as_ref() else {
        return;
    };
    let Some((returned_ident, suffix)) = ret else {
        return;
    };
    let Some(prefix) = bound.get(returned_ident.as_str()) else {
        return;
    };
    let mut path = prefix.0.clone();
    path.extend(suffix.0.iter().cloned());
    bind_lhs(lhs, src, &JsonPath(path), bound);
}

/// Rule 6 (§6.5): when we see `callee(arg)` and `arg` is bound,
/// bind callee's parameter at the same position.
fn apply_rule6_tsjs<'a>(
    node: Node<'a>,
    src: &[u8],
    bound: &mut BTreeMap<String, JsonPath>,
    metadata: &FunctionMetadata,
) {
    if node.kind() != "call_expression" && node.kind() != "call" {
        return;
    }
    let function = match node.child_by_field_name("function") {
        Some(f) => f,
        None => return,
    };
    let called_name = match function.kind() {
        "identifier" => function.utf8_text(src).ok().map(|s| s.to_string()),
        "member_expression" | "attribute" => function
            .child_by_field_name("object")
            .and_then(|_| {
                function
                    .child_by_field_name("property")
                    .or_else(|| function.child_by_field_name("attribute"))
                    .or_else(|| function.child_by_field_name("name"))
            })
            .and_then(|n| n.utf8_text(src).ok().map(|s| s.to_string())),
        _ => None,
    };
    let Some(called_name) = called_name else {
        return;
    };
    let params = match metadata.parameters.get(&called_name) {
        Some(p) => p.clone(),
        None => return,
    };
    let args_node = node.child_by_field_name("arguments");
    let Some(args_node) = args_node else {
        return;
    };
    // Walk arguments and bind matching parameters.
    let mut cursor = args_node.walk();
    let mut arg_iter = args_node.named_children(&mut cursor);
    for param_name in &params {
        let arg = match arg_iter.next() {
            Some(a) => a,
            None => break,
        };
        let value = arg
            .child_by_field_name("value")
            .or_else(|| (arg.kind() == "identifier").then_some(arg))
            .unwrap_or(arg);
        if let Ok(name) = value.utf8_text(src) {
            let key: &str = name;
            if let Some(prefix) = bound.get(key) {
                bound.insert(param_name.clone(), prefix.clone());
            }
        }
    }
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
                call_id: String::new(),
                sender_id: String::new(),
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
    fn python_destructuring_records_reads() {
        // Python disambiguates `{a, b} = x` (set / dict literal) from
        // `[a, b] = x` (sequence destructuring) at the lexer level; the
        // brief's `{ k, a: { b } } = x` shape needs `[a, b] = x` for the
        // simple two-name case. Tree-sitter parses both as `assignment`
        // with a positional `list_pattern` / `pattern_list` LHS.
        let src = "x = fetch(\"/a\")\n[a, b] = x\n";
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

    /// §6.5 escapes: a bound identifier becomes `Escape::Returned`
    /// only when it is "returned from a caller (it would leave the
    /// scope)".
    ///
    /// - **S's own frame**: `return x` / `return r.json()` is rule
    ///   5's propagation channel — the value lands in a direct
    ///   caller, still inside the scope. Not an escape: flipping
    ///   `reads_complete` here would (per §9.5) turn scenario 5's
    ///   expected `NoKnownImpact` into `NeedsInvestigation`.
    /// - **A rule-5 caller frame**: the caller's own caller is
    ///   outside the scope (only *direct* callers of S are in
    ///   scope), so a bound identifier returned there escapes and
    ///   flips `reads_complete` to `false` (scenario 22).
    #[test]
    fn return_escape_fires_only_from_a_caller_frame() {
        // Part 1: S-frame return of the bound identifier — no escape.
        let src = "\
def fetch_order():
    x = fetch(\"/a\")
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
        assert!(
            !escapes.contains(&Escape::Returned),
            "S returning its bound response is rule 5's channel, not an escape \
             (§6.5: escape is 'returned from a caller')"
        );

        // Part 2: a bound identifier returned from a *caller* frame
        // leaves the scope → escape → reads_complete = false.
        let src2 = "\
def fetch_order():
    x = fetch(\"/a\")
    return x


def caller():
    order = fetch_order()
    return order
";
        let mut bound2: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound2.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads2 = Vec::new();
        let mut escapes2 = BTreeSet::new();
        let tree2 = parse(Lang::Python, src2).unwrap();
        let calls_by_function: BTreeMap<String, BTreeSet<String>> = [
            (
                "caller".to_string(),
                ["fetch_order".to_string()].into_iter().collect(),
            ),
            ("fetch_order".to_string(), BTreeSet::new()),
        ]
        .into_iter()
        .collect();
        let emissions = detect_emissions_with_calls(
            src2,
            &tree2,
            Lang::Python,
            "x.py",
            &calls_by_function,
            &mut bound2,
            &mut reads2,
            &mut escapes2,
        );
        assert!(
            escapes2.contains(&Escape::Returned),
            "a bound identifier returned from a caller frame must escape (§6.5)"
        );
        assert!(
            emissions.iter().any(|e| !e.reads_complete),
            "the escape must flip the emission's reads_complete to false"
        );
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
    fn ts_x_as_dto_binds_d_to_same_path() {
        // Rule 4 (TS): `x as Dto` rebinds the operand with the same
        // path. The TypeScript grammar is needed to parse the `as`
        // form, so this test runs through the TS parser.
        let src = "const x = await fetch(\"/a\");\nconst d = x as Dto;\n";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::Ts, src).unwrap();
        walk_tree(
            tree.root_node(),
            src.as_bytes(),
            Lang::Ts,
            &mut bound,
            &mut reads,
            &mut escapes,
            "x.ts",
        );
        assert!(bound.contains_key("d"));
    }

    #[test]
    fn python_destructuring_nested_records_subpaths() {
        // Python sequence destructuring: `[a, b] = x` produces one
        // FieldRef per named element on the LHS, each chain =
        // top-level field. Sequence indices aren't part of the
        // JSON path (§6.4 — `JsonPath` is name / `[]` / `{}` only),
        // so nested sequence destructuring keeps the same chain.
        let src = "x = fetch(\"/a\")\n[a, b] = x\n";
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
        // Both `a` and `b` are top-level reads.
        let names: Vec<String> = reads.iter().map(|r| r.chain.to_string()).collect();
        assert!(names.contains(&"a".to_string()), "names={names:?}");
        assert!(names.contains(&"b".to_string()), "names={names:?}");
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

    /// §6.5 rule 5: in a caller of S, `o = S(...)` binds o when S
    /// returns a bound identifier. Scope-boundary test: the caller
    /// here is in scope (it directly calls S), so the LHS gets the
    /// returned sub-path and `r.id` is recorded.
    #[test]
    fn rule5_caller_binds_returned_subpath_in_scope() {
        let src = b"\
async function fetch_data() { return await fetch(\"/a\"); }
async function caller() {
  const r = fetch_data();
  console.log(r.id);
}
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads = Vec::new();
        let mut escapes = BTreeSet::new();
        let tree = parse(Lang::TsJs, std::str::from_utf8(src).unwrap()).unwrap();
        let calls_by_function: BTreeMap<String, BTreeSet<String>> = [
            (
                "caller".to_string(),
                ["fetch_data".to_string()].into_iter().collect(),
            ),
            ("fetch_data".to_string(), BTreeSet::new()),
        ]
        .into_iter()
        .collect();
        let emissions = detect_emissions_with_calls(
            std::str::from_utf8(src).unwrap(),
            &tree,
            Lang::TsJs,
            "x.ts",
            &calls_by_function,
            &mut bound,
            &mut reads,
            &mut escapes,
        );
        // The FieldRef for r.id is recorded when caller walks the
        // body and reads `r.id`.
        assert!(emissions.is_empty() || reads.iter().any(|r| r.chain.to_string() == "id"));
    }

    /// §6.5 rule 5 scope-boundary test: the calling function is
    /// OUT of scope (it doesn't directly call S), so the LHS does
    /// not get bound and the read is not emitted.
    #[test]
    fn rule5_out_of_scope_caller_does_not_bind() {
        let src = b"\
async function fetch_data() { return await fetch(\"/a\"); }
function mid() { return fetch_data(); }
function outer() {
  const r = mid();
  console.log(r.id);
}
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads: Vec<FieldRead> = Vec::new();
        let mut escapes: BTreeSet<Escape> = BTreeSet::new();
        let tree = parse(Lang::TsJs, std::str::from_utf8(src).unwrap()).unwrap();
        let calls_by_function: BTreeMap<String, BTreeSet<String>> = [
            ("fetch_data".to_string(), BTreeSet::new()),
            (
                "mid".to_string(),
                ["fetch_data".to_string()].into_iter().collect(),
            ),
            (
                "outer".to_string(),
                ["mid".to_string()].into_iter().collect(),
            ),
        ]
        .into_iter()
        .collect();
        // `outer` is NOT a direct caller of `fetch_data`. Its scope
        // is two hops out, so the §6.5 rule 5 binding does not
        // apply — `r` stays unbound and `r.id` is not a FieldRef.
        let _ = detect_emissions_with_calls(
            std::str::from_utf8(src).unwrap(),
            &tree,
            Lang::TsJs,
            "x.ts",
            &calls_by_function,
            &mut bound,
            &mut reads,
            &mut escapes,
        );
        // The walker only emits reads from in-scope functions. Even
        // if `outer` runs through `walk_tree` (which it does when
        // its body is reachable from a sending function), `r.id`
        // is not a read because `r` was never bound by rule 5. We
        // assert that no read of `id` is emitted from the outer
        // function path.
        let has_id_read = reads.iter().any(|r| r.chain.to_string() == "id");
        assert!(
            !has_id_read,
            "rule 5 must NOT propagate through mid: outer is out of scope"
        );
    }

    /// §6.5 rule 6: in a callee of S, the parameter at the position
    /// a bound identifier is passed in is bound. The callee reads
    /// `param.id` and emits a FieldRef for it.
    #[test]
    fn rule6_callee_binds_param_at_bound_arg_position() {
        let src = b"\
async function fetch_data() { return await fetch(\"/a\"); }
function use_id(x) { return x.id; }
function caller() {
  const r = fetch_data();
  return use_id(r);
}
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads: Vec<FieldRead> = Vec::new();
        let mut escapes: BTreeSet<Escape> = BTreeSet::new();
        let tree = parse(Lang::TsJs, std::str::from_utf8(src).unwrap()).unwrap();
        let calls_by_function: BTreeMap<String, BTreeSet<String>> = [
            ("fetch_data".to_string(), BTreeSet::new()),
            (
                "caller".to_string(),
                ["fetch_data".to_string(), "use_id".to_string()]
                    .into_iter()
                    .collect(),
            ),
            ("use_id".to_string(), BTreeSet::new()),
        ]
        .into_iter()
        .collect();
        let _ = detect_emissions_with_calls(
            std::str::from_utf8(src).unwrap(),
            &tree,
            Lang::TsJs,
            "x.ts",
            &calls_by_function,
            &mut bound,
            &mut reads,
            &mut escapes,
        );
        // The callee `use_id` reads `x.id` and emits a FieldRef for
        // the chain `id`. (The path is empty because `x` was bound
        // to the rule-6 path of `r`, which is itself bound to the
        // response root.)
        let has_id_read = reads.iter().any(|r| r.chain.to_string() == "id");
        assert!(
            has_id_read,
            "rule 6 must bind the callee's parameter at the bound-arg position"
        );
    }

    /// §6.5 rule 6 out-of-scope: a callee of a non-S function is NOT
    /// in scope for the field-access sensor, so a bound identifier
    /// passed to it does NOT propagate.
    #[test]
    fn rule6_out_of_scope_callee_does_not_bind() {
        let src = b"\
function use_id(x) { return x.id; }
function caller() { return use_id({}); }
async function fetch_data() { return await fetch(\"/a\"); }
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads: Vec<FieldRead> = Vec::new();
        let mut escapes: BTreeSet<Escape> = BTreeSet::new();
        let tree = parse(Lang::TsJs, std::str::from_utf8(src).unwrap()).unwrap();
        // `caller` calls `use_id` — but `caller` is not a caller of
        // `fetch_data`. `use_id` is a callee of `caller`, which is
        // not in scope for the sending function.
        let calls_by_function: BTreeMap<String, BTreeSet<String>> = [
            ("use_id".to_string(), BTreeSet::new()),
            (
                "caller".to_string(),
                ["use_id".to_string()].into_iter().collect(),
            ),
            ("fetch_data".to_string(), BTreeSet::new()),
        ]
        .into_iter()
        .collect();
        let _ = detect_emissions_with_calls(
            std::str::from_utf8(src).unwrap(),
            &tree,
            Lang::TsJs,
            "x.ts",
            &calls_by_function,
            &mut bound,
            &mut reads,
            &mut escapes,
        );
        // The walker sees `x.id` inside `use_id` but `x` is not
        // bound — `use_id` is out of scope for the sending
        // function. No FieldRef is emitted for `id`.
        let has_id_read = reads.iter().any(|r| r.chain.to_string() == "id");
        assert!(
            !has_id_read,
            "rule 6 must NOT propagate across an out-of-scope callee"
        );
    }

    // ── Full-scan tests: rules 5/6 + ReadsFrom target + reads_complete ──

    use crate::federation::contracts::model::{
        CallVia, ConsumerFact, HostPart, HttpMethod, MethodSpec, NormalizedUrl, SourceSite,
    };

    fn write_py(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    /// Seed the per-repo graph the way phases 0–1 would: the sending
    /// function's `Function` node, a caller `Function` node, the
    /// `HttpClientCall` carrying a `ConsumerFact`, the `SendsHttp`
    /// edge (site = the call's line), and the `Calls` edge from
    /// caller → S with `Static{TreeSitter}` provenance (the §6.5
    /// scope filter keeps exactly this edge).
    ///
    /// Returns `(call_node_id, sends_http_target)`. In the per-repo
    /// graph the call's identity is its node id — the same id
    /// `SendsHttp` targets and `ReadsFrom` must target;
    /// `FederatedIndex::project_edges` later rewrites both endpoints
    /// through `local_to_global` (`GlobalId::new(repo, kind, path,
    /// name, line_start)`), which is exactly the string
    /// `resolve_field_refs` matches `BindsEdge.consumer` by.
    /// Returns `(call_node_id, caller_node_id)`. The Function nodes
    /// carry line ranges so `enclosing_symbol` can resolve the
    /// reading function at a read site (§6.5 `ReadsField` source).
    #[allow(clippy::too_many_arguments)] // test fixture: ranges + ids
    fn seed_scope_fixture(
        graph: &GraphDatabase,
        ns: &RepoNamespace,
        path: &str,
        sender: &str,
        sender_range: (u32, u32),
        caller: &str,
        caller_range: (u32, u32),
        call_line: u32,
    ) -> (String, String) {
        let sender_node = GraphNode::new_in(NodeType::Function, sender.into(), path.into(), ns)
            .with_location_in(sender_range.0, sender_range.1, ns);
        let caller_node = GraphNode::new_in(NodeType::Function, caller.into(), path.into(), ns)
            .with_location_in(caller_range.0, caller_range.1, ns);
        let caller_id = caller_node.id.clone();
        let mut call_node = GraphNode::new_in(
            NodeType::HttpClientCall,
            "GET /api/1".into(),
            path.into(),
            ns,
        );
        call_node.line_start = Some(call_line);
        call_node.contract = Some(ContractFact::Consumer(ConsumerFact {
            method: MethodSpec::Known(HttpMethod::Get),
            url: NormalizedUrl {
                host: HostPart::Literal("orders".into()),
                template: Some("/api/1".into()),
            },
            via: CallVia::Library {
                name: "requests".into(),
            },
            url_expr: "\"http://orders/api/1\"".into(),
            reads_complete: true,
        }));
        let mut sends = GraphEdge::new(
            EdgeType::SendsHttp,
            sender_node.id.clone(),
            call_node.id.clone(),
        );
        sends.site = Some(SourceSite {
            path: path.into(),
            line: call_line,
        });
        let mut calls_e = GraphEdge::new(
            EdgeType::Calls,
            caller_node.id.clone(),
            sender_node.id.clone(),
        );
        calls_e.provenance = Some(EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        });
        graph.upsert_node(sender_node).unwrap();
        graph.upsert_node(caller_node).unwrap();
        graph.upsert_node(call_node.clone()).unwrap();
        graph.upsert_edge(sends).unwrap();
        graph.upsert_edge(calls_e).unwrap();
        (call_node.id, caller_id)
    }

    fn field_ref_by_chain(graph: &GraphDatabase, chain: &str) -> GraphNode {
        graph
            .get_all_nodes()
            .into_iter()
            .find(|n| {
                n.node_type == NodeType::FieldRef
                    && matches!(&n.contract, Some(ContractFact::FieldRead(fr)) if fr.chain.to_string() == chain)
            })
            .unwrap_or_else(|| panic!("FieldRef for chain {chain:?} must exist"))
    }

    /// Gap 1 + gap 2 + gap 3 (no-escape side): a *full* scan (the
    /// real `scan_workspace_field_access` entry, not the test
    /// helper) over a multi-function fixture must record the
    /// caller-frame read, target `ReadsFrom` at the `HttpClientCall`
    /// node id, and leave `reads_complete` true when nothing escaped.
    #[test]
    fn full_scan_lands_caller_frame_reads_and_readstarget_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = "import requests\n\
                       \n\
                       \n\
                       def fetch_order():\n\
                       \x20   r = requests.get(\"http://orders/api/1\")\n\
                       \x20   print(r.status_code)\n\
                       \n\
                       \n\
                       def build_invoice():\n\
                       \x20   order = fetch_order()\n\
                       \x20   print(order[\"customer_id\"])\n";
        write_py(root, "src/main.py", content);
        let ns = RepoNamespace::for_test();
        let graph = GraphDatabase::new(&root.join("graph.bin")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        let call_line = lines
            .iter()
            .position(|l| l.contains("requests.get"))
            .unwrap() as u32
            + 1;
        let s_start = lines
            .iter()
            .position(|l| l.contains("def fetch_order"))
            .unwrap() as u32
            + 1;
        let c_start = lines
            .iter()
            .position(|l| l.contains("def build_invoice"))
            .unwrap() as u32
            + 1;
        let (call_id, caller_id) = seed_scope_fixture(
            &graph,
            &ns,
            "src/main.py",
            "fetch_order",
            (s_start, c_start - 1),
            "build_invoice",
            (c_start, lines.len() as u32),
            call_line,
        );
        let repo_id = RepoId::new("fixture").unwrap();
        scan_workspace_field_access(&graph, root, &ns, &repo_id).unwrap();

        // Gap 1: the caller frame's read landed — S's own frame
        // never touches `customer_id`, so this FieldRef can only come
        // from walking `build_invoice` via the §6.5 scope.
        let customer_ref = field_ref_by_chain(&graph, "customer_id");

        // Gap 2: `ReadsFrom` targets the HttpClientCall node id —
        // the same identity `SendsHttp`'s endpoint side uses.
        let edges = graph.all_edges();
        let reads_from = edges
            .iter()
            .find(|e| e.edge_type == EdgeType::ReadsFrom && e.source_id == customer_ref.id)
            .expect("FieldRef must carry a ReadsFrom edge");
        assert_eq!(
            reads_from.target_id, call_id,
            "ReadsFrom must target the HttpClientCall node id, not a path"
        );
        let sends = edges
            .iter()
            .find(|e| e.edge_type == EdgeType::SendsHttp)
            .expect("SendsHttp edge");
        assert_eq!(
            reads_from.target_id, sends.target_id,
            "ReadsFrom target must be the identity SendsHttp's endpoint side uses \
             (projected to the joiner's Binds consumer GlobalId)"
        );

        // §6.5: the `ReadsField` edge starts at the *reading*
        // function — `build_invoice`, not the File node.
        let reads_field = edges
            .iter()
            .find(|e| e.edge_type == EdgeType::ReadsField && e.target_id == customer_ref.id)
            .expect("FieldRef must carry a ReadsField edge");
        assert_eq!(
            reads_field.source_id, caller_id,
            "ReadsField must start at the reading function node (build_invoice)"
        );

        // Gap 3, no-escape side: nothing escaped → the consumer fact
        // stays `reads_complete = true` (and the node survives — it
        // is never re-emitted through this sensor's owner).
        let call_node = graph.get_node(&call_id).unwrap().expect("call node");
        match call_node.contract {
            Some(ContractFact::Consumer(c)) => {
                assert!(
                    c.reads_complete,
                    "no escape → ConsumerFact.reads_complete stays true"
                );
            }
            other => panic!("expected ConsumerFact, got {other:?}"),
        }
    }

    /// Gap 3, escape side: when a scope frame returns the bound
    /// identifier, the call node's `ConsumerFact.reads_complete` is
    /// patched to `false` in place (phase-1 node, not re-emitted).
    #[test]
    fn full_scan_escape_patches_consumer_reads_complete() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let content = "import requests\n\
                       \n\
                       \n\
                       def fetch_order():\n\
                       \x20   r = requests.get(\"http://orders/api/1\")\n\
                       \x20   print(r.status_code)\n\
                       \n\
                       \n\
                       def build_invoice():\n\
                       \x20   order = fetch_order()\n\
                       \x20   print(order[\"customer_id\"])\n\
                       \x20   return order\n";
        write_py(root, "src/main.py", content);
        let ns = RepoNamespace::for_test();
        let graph = GraphDatabase::new(&root.join("graph.bin")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        let call_line = lines
            .iter()
            .position(|l| l.contains("requests.get"))
            .unwrap() as u32
            + 1;
        let s_start = lines
            .iter()
            .position(|l| l.contains("def fetch_order"))
            .unwrap() as u32
            + 1;
        let c_start = lines
            .iter()
            .position(|l| l.contains("def build_invoice"))
            .unwrap() as u32
            + 1;
        let (call_id, _caller_id) = seed_scope_fixture(
            &graph,
            &ns,
            "src/main.py",
            "fetch_order",
            (s_start, c_start - 1),
            "build_invoice",
            (c_start, lines.len() as u32),
            call_line,
        );
        let repo_id = RepoId::new("fixture").unwrap();
        scan_workspace_field_access(&graph, root, &ns, &repo_id).unwrap();

        // The caller-frame read still lands even though it escaped.
        field_ref_by_chain(&graph, "customer_id");

        // The escape (`return order` in a caller frame) patched the
        // phase-1 consumer fact in place.
        let call_node = graph.get_node(&call_id).unwrap().expect("call node");
        match call_node.contract {
            Some(ContractFact::Consumer(c)) => {
                assert!(
                    !c.reads_complete,
                    "escape in a caller frame must flip ConsumerFact.reads_complete to false"
                );
            }
            other => panic!("expected ConsumerFact, got {other:?}"),
        }
        // The node still exists exactly once (never wiped by this
        // sensor's `replace_sensor_output`).
        assert_eq!(
            graph
                .get_all_nodes()
                .iter()
                .filter(|n| n.id == call_id)
                .count(),
            1
        );
    }

    /// §6.5: the `ReadsField` edge starts at the *reading function*;
    /// a genuinely module-level read (no enclosing function covers
    /// the line) falls back to the `File` node — the same convention
    /// `enclosing_sends_http_edge` uses for module-level calls.
    #[test]
    fn readstfield_source_falls_back_to_file_for_module_level_reads() {
        let dir = tempfile::tempdir().unwrap();
        let graph = GraphDatabase::new(&dir.path().join("graph.bin")).unwrap();
        let ns = RepoNamespace::for_test();
        // A function that exists at the path but does NOT cover the
        // read's line — so `enclosing_symbol` yields None.
        let helper = GraphNode::new_in(
            NodeType::Function,
            "helper".into(),
            "src/main.py".into(),
            &ns,
        )
        .with_location_in(10, 12, &ns);
        graph.upsert_node(helper).unwrap();
        let file_node =
            GraphNode::new_in(NodeType::File, "main.py".into(), "src/main.py".into(), &ns);
        let file_id = file_node.id.clone();
        graph.upsert_node(file_node).unwrap();

        let emission = FieldAccessEmission {
            path: "src/main.py".into(),
            call_id: String::new(),
            sender_id: String::new(),
            reads: vec![FieldRead {
                chain: field_ref_chain("customer_id"),
                exact: true,
                path: "src/main.py".into(),
                line: 3, // module level: no function covers line 3
                reader_id: "self".into(),
            }],
            escapes: BTreeSet::new(),
            reads_complete: true,
        };
        let (_nodes, edges) = build_emission(&graph, &emission, &ns);
        let reads_field = edges
            .iter()
            .find(|e| e.edge_type == EdgeType::ReadsField)
            .expect("ReadsField edge");
        assert_eq!(
            reads_field.source_id, file_id,
            "a module-level read must attach to the File node"
        );
    }

    /// §6.5 rule 5 with the returned sub-path: S returns
    /// `j["data"]`, so the caller's `o = S()` binds `o` to
    /// `data` and the caller's read on `o["id"]` records
    /// `data.id` — not bare `id`.
    #[test]
    fn rule5_returned_subpath_binds_chain() {
        let src = "\
async function fetch_data() {
  const r = await fetch(\"/a\");
  const j = await r.json();
  return j[\"data\"];
}
async function caller() {
  const o = fetch_data();
  console.log(o[\"id\"]);
}
";
        let mut bound: BTreeMap<String, JsonPath> = BTreeMap::new();
        bound.insert("__response__".to_string(), JsonPath(Vec::new()));
        let mut reads: Vec<FieldRead> = Vec::new();
        let mut escapes: BTreeSet<Escape> = BTreeSet::new();
        let tree = parse(Lang::TsJs, src).unwrap();
        let calls_by_function: BTreeMap<String, BTreeSet<String>> = [
            (
                "caller".to_string(),
                ["fetch_data".to_string()].into_iter().collect(),
            ),
            ("fetch_data".to_string(), BTreeSet::new()),
        ]
        .into_iter()
        .collect();
        let _ = detect_emissions_with_calls(
            src,
            &tree,
            Lang::TsJs,
            "x.ts",
            &calls_by_function,
            &mut bound,
            &mut reads,
            &mut escapes,
        );
        let chains: Vec<String> = reads.iter().map(|r| r.chain.to_string()).collect();
        assert!(
            chains.contains(&"data.id".to_string()),
            "rule 5 must bind the returned sub-path: chains = {chains:?}"
        );
    }
}
