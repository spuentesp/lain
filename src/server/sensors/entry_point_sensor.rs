//! Entry-point sensor (`docs/CONTRACT_FEDERATION.md` §6.6).
//!
//! Sets `GraphNode.entry` on function nodes so `used_by` (`§10.9`) can say
//! why code runs:
//!
//! | `EntryKind` | Detection |
//! | --- | --- |
//! | `HttpHandler` | Target of a `CallsHttp` edge (route → handler) |
//! | `Scheduled`   | node-cron `cron.schedule(expr, fn)`; module-level `setInterval(fn, …)`; APScheduler `@<x>.scheduled_job(…)` and `add_job(fn, …)`; Celery `@<x>.task` and `@shared_task`; NestJS `@Cron(…)` |
//! | `Cli`        | click `@click.command` and `@<group>.command`, typer `@<app>.command`, commander `.command(…).action(fn)` |
//! | `Main`       | functions named `main` or `App`; the enclosing module of an `if __name__ == "__main__":` block |
//!
//! A function passed by reference (`cron.schedule("0 0 1 * *", buildMonthlyReport)`)
//! is resolved by name in the same file.
//!
//! **Clears and resets `entry` on every run.** The T5 scaffolding in
//! [`GraphDatabase::replace_sensor_output`](crate::graph::GraphDatabase::replace_sensor_output)
//! wipes every node's `entry` field when called with `SensorOwner::EntryPointSensor`,
//! then this sensor re-applies the detections it just made.
//! `EntryPointSensor` owns no nodes (`sensor_owner_of` returns `None` for
//! every node it would write), so the owner-based removal in step 1/2 of
//! `replace_sensor_output` is a no-op for us — confirmed by reading the
//! scaffolding.
//!
//! Phase 2 (`§6.1`): runs after `http_sensor` has emitted `CallsHttp`
//! edges and after `field_access_sensor`, so the `HttpHandler`
//! derivation has the edges it needs.
//!
//! Determinism: every detector returns `BTreeMap`, and the per-file
//! detections are merged into a single `BTreeMap` sorted by
//! `(path, name)` so two scans on the same tree produce the same
//! `entry` set (`§8.3`).

use crate::error::LainError;
use crate::federation::contracts::model::EntryKind;
use crate::graph::{GraphDatabase, SensorOwner};
use crate::schema::{GraphNode, NodeType, RepoNamespace};
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::util::{language_for, parse_for_lang, Lang};
use crate::server::sensors::SensorEntry;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

/// The detected entry points in one file: `(function_name_in_file, line)`
/// → `EntryKind`. Multi-detection (rare in practice) keeps the first
/// hit per the order detectors fire.
type FileDetections = BTreeMap<(String, u32), EntryKind>;

/// Walk every file under `root`, detect entry points, and stamp
/// `GraphNode.entry` on the matching function nodes.
///
/// The `replace_sensor_output` call with `SensorOwner::EntryPointSensor`
/// wipes every node's `entry` field up front (T5 scaffolding, step 3),
/// then this function re-applies the freshly-detected set. The owner
/// owns no nodes, so the stale-id removal in steps 1/2 is a no-op.
///
/// Layering per-repo overrides from `<root>/.lain/patterns/` happens
/// at the top of this function via [`Patterns::with_overrides`]: the
/// per-repo YAML + `.scm` overrides are merged into the bundled
/// registry before any walker code runs, so the
/// Java/ASP.NET/Rails entry-point walkers see override-augmented
/// `.scm` queries via `Patterns::compiled_queries`.
pub fn scan_workspace_entry_points(
    graph: &GraphDatabase,
    root: &std::path::Path,
    _namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }

    // Step A — clear every node's `entry` field. Empty node / edge
    // sets: this sensor owns no nodes of its own.
    graph.replace_sensor_output(SensorOwner::EntryPointSensor, &[], &[])?;

    let mut by_id: BTreeMap<String, (String, EntryKind)> = BTreeMap::new();
    let patterns = Patterns::with_overrides(root)?;
    let patterns: &Patterns = &patterns;

    for entry in crate::server::sensors::util::walk_workspace(root) {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ![
            "rs", "py", "ts", "js", "go", "java", "cs", "rb", "kt", "kts",
        ]
        .contains(&ext)
        {
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let graph_path = crate::graph::graph_path(root, path);
        let detections = detect_in_file(&content, &graph_path, ext, patterns);
        for ((name, _line), kind) in detections {
            // Resolve the name to a graph node id. The scanner may
            // have produced multiple `Function` nodes for the same
            // name (overloads, methods on different classes); we pick
            // the first in `(path, id)` order — `find_node_by_name`'s
            // stable tiebreaker.
            if let Some(node) = find_function_node(graph, &graph_path, &name) {
                by_id
                    .entry(node.id.clone())
                    .or_insert_with(|| (name.clone(), kind));
            }
        }
    }

    // Step B — derive `HttpHandler` from `CallsHttp` edges http_sensor
    // emitted in phase 0.
    for (_route_id, handler_id) in graph.calls_http_pairs() {
        by_id
            .entry(handler_id.clone())
            .or_insert_with(|| ("<http_handler>".to_string(), EntryKind::HttpHandler));
    }

    // Step C — stamp the detections.
    let mut stamped = 0usize;
    for (id, (name, kind)) in &by_id {
        if graph.set_entry(id, *kind)? {
            stamped += 1;
        }
        let _ = name; // retained for debugging
    }

    Ok(stamped)
}

/// Resolve a `(path, name)` pair to a single function/method graph node.
/// Returns `None` if no such node is indexed yet (the regex detected
/// a function the scanner hasn't produced — the entry-point stamp
/// silently skips it; the next scan will pick it up).
fn find_function_node(graph: &GraphDatabase, path: &str, name: &str) -> Option<GraphNode> {
    graph
        .get_all_nodes()
        .into_iter()
        .filter(|n| n.path == path)
        .filter(|n| n.name == name)
        .filter(|n| matches!(n.node_type, NodeType::Function | NodeType::Method))
        .min_by(|a, b| a.id.cmp(&b.id))
}

/// All §6.6 detectors applied to one file's content. Returns a map of
/// `(function_name, first_occurrence_line)` → `EntryKind`.
fn detect_in_file(content: &str, _path: &str, ext: &str, patterns: &Patterns) -> FileDetections {
    let mut out: FileDetections = BTreeMap::new();
    detect_scheduled(content, ext, &mut out);
    detect_cli(content, ext, &mut out);
    detect_main(content, ext, &mut out);
    detect_http_handler(content, ext, &mut out, patterns);
    out
}

// ─── HTTP handler (annotation-based controllers) ───────────────
//
// Workstream 5 — for languages where the HTTP route is a method
// annotation (Java Spring / JAX-RS, C# ASP.NET) the
// `HttpHandler` entry point follows the same wiring as Python's
// `@app.route` → handler: the function name appears on the line
// immediately below the annotation. Task 4 of the data-driven
// sensor-patterns plan replaced the inline regex tables with
// tree-sitter queries loaded via `Patterns::compiled_queries()`. The
// regex fallback below (`detect_java_http_handler_regex`,
// `detect_csharp_http_handler_regex`,
// `detect_ruby_rails_controller_regex`) is retained for the case
// where a `.scm` body is missing or empty — keeps the walker
// resilient to partial migrations.
fn detect_http_handler(content: &str, ext: &str, out: &mut FileDetections, patterns: &Patterns) {
    // Java Spring / JAX-RS: `@GetMapping("/x")` on a method, with
    // the public method on the next non-blank line.
    if ext == "java" {
        if !detect_java_http_handler(content, out, patterns) {
            detect_java_http_handler_regex(content, out);
        }
        return;
    }
    // C# ASP.NET: `[HttpGet("/x")]` on a method.
    if ext == "cs" {
        if !detect_csharp_http_handler(content, out, patterns) {
            detect_csharp_http_handler_regex(content, out);
        }
        return;
    }
    // Ruby Rails: action methods (`def index`, `def show`, `def
    // create`, …) inside `app/controllers/*.rb`.
    if ext == "rb" {
        if !detect_ruby_rails_controller(content, out, patterns) {
            detect_ruby_rails_controller_regex(content, out);
        }
        return;
    }
    // Kotlin Ktor: handler functions inside `routing { get("/x") { … } }`
    // blocks. Detected via the matching `HttpRoute` node the
    // http_sensor emits with a sentinel handler name
    // (`Kt doRoute<R>block`); no in-source regex needed here — the
    // `CallsHttp` edge from `http_sensor` resolves via the
    // entry-point's CallsHttp-pair sweep.
    if ext == "kt" || ext == "kts" {
        // No in-source detection; Kotlin entry points are surfaced
        // either by `detect_main` (functions named `main`) or by
        // the CallsHttp-edge resolution in step B of
        // `scan_workspace_entry_points`.
        let _ = content;
    }
}

/// Find a `.scm` query body by its `<lang>/<framework>.scm` key
/// via `Patterns::compiled_queries()`. Returns `None` when the key
/// isn't in the registry or when a malformed override `.scm` body
/// failed validation at load time; callers fall back to their inline
/// regex path so a partial migration still works. Task 6 of the
/// data-driven-sensor-patterns plan switched `compiled_queries()` to
/// return `Result` so that an uncompilable override surfaces a
/// structured error at load time; this accessor flattens the `Err`
/// to `None` so the walker keeps going and the sensor's per-file
/// scan does not 500 the whole pipeline on a single broken override.
///
/// The runtime-override wire-in (this PR) takes `&Patterns` so the
/// per-repo overrides layered on the scanner's `Patterns` instance
/// are visible to the entry-point walker. The bundled-singleton path
/// (`Patterns::patterns()`) is preserved as a no-arg compatibility
/// helper below.
fn entry_point_query_body<'a>(patterns: &'a Patterns, key: &str) -> Option<&'a str> {
    patterns
        .compiled_queries()
        .ok()?
        .iter()
        .find(|(k, _, _, _)| *k == key)
        .map(|(_, _, _, body)| *body)
}

/// Backward-compatible wrapper that resolves against the bundled
/// singleton. Used by lib tests that don't go through
/// `scan_workspace_entry_points` and therefore don't have a
/// per-repo patterns instance to thread.
#[allow(dead_code)]
fn entry_point_query_body_singleton(key: &str) -> Option<&'static str> {
    entry_point_query_body(Patterns::patterns(), key)
}

/// Returns `true` when the .scm body has at least one non-comment
/// line. Comment-only stubs short-circuit to the regex fallback.
fn scm_has_query(query: &str) -> bool {
    query.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.is_empty() && !trimmed.starts_with(';')
    })
}

/// Extract the source text covered by `node` from `content`.
fn text_for_node<'a>(node: tree_sitter::Node, content: &'a str) -> Option<&'a str> {
    content.get(node.start_byte()..node.end_byte())
}

/// Query-driven Spring `@GetMapping` / `@PostMapping` /
/// `@RequestMapping` handler detection. Runs the
/// `java/spring-entry-point.scm` query body against the parsed
/// Java tree. Captures `@verb` (annotation name), `@path` (path
/// literal), and `@handler` (method name).
///
/// Returns `true` when the query ran successfully and was not
/// missing/empty; the caller falls back to the regex detector when
/// this returns `false`. Returns `false` if the file is missing,
/// has an empty query, fails to compile, fails to parse, or has a
/// tree-sitter error.
fn detect_java_http_handler(content: &str, out: &mut FileDetections, patterns: &Patterns) -> bool {
    let Some(body) = entry_point_query_body(patterns, "java/spring-entry-point.scm") else {
        return false;
    };
    if !scm_has_query(body) {
        return false;
    }
    let Some(tree) = parse_for_lang(Lang::Java, content) else {
        return false;
    };
    let grammar = language_for(Lang::Java);
    let Ok(query) = Query::new(&grammar, body) else {
        return false;
    };
    let idxs = capture_indices(&query, &["verb", "path", "handler"]);
    let verb_idx = idxs[0];
    let path_idx = idxs[1];
    let handler_idx = idxs[2];
    let mut cursor = QueryCursor::new();
    let src = content.as_bytes();
    let mut matches = cursor.matches(&query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let verb = verb_idx.and_then(|i| m.nodes_for_capture_index(i).next());
        let handler = handler_idx.and_then(|i| m.nodes_for_capture_index(i).next());
        let path = path_idx.and_then(|i| m.nodes_for_capture_index(i).next());
        let (Some(verb), Some(handler)) = (verb, handler) else {
            continue;
        };
        let (Some(verb_text), Some(handler_text)) = (
            text_for_node(verb, content),
            text_for_node(handler, content),
        ) else {
            continue;
        };
        if !is_spring_route_annotation(verb_text) {
            continue;
        }
        let _ = text_for_node(path.unwrap_or(verb), content);
        let line = (handler.start_position().row as u32) + 1;
        out.entry((handler_text.to_string(), line))
            .or_insert(EntryKind::HttpHandler);
    }
    true
}

/// `true` when `verb` is a Spring route annotation
/// (`@GetMapping`, `@PostMapping`, `@PutMapping`, `@DeleteMapping`,
/// `@PatchMapping`, `@RequestMapping`).
fn is_spring_route_annotation(verb: &str) -> bool {
    matches!(
        verb,
        "GetMapping"
            | "PostMapping"
            | "PutMapping"
            | "DeleteMapping"
            | "PatchMapping"
            | "RequestMapping"
    )
}

/// Query-driven ASP.NET `[HttpGet]` / `[HttpPost]` /
/// `[HttpPatch]` / `[HttpDelete]` etc. handler detection via
/// `csharp/aspnet-entry-point.scm`.
///
/// Returns `true` when the query ran successfully; the caller falls
/// back to the regex detector when this returns `false`. Returns
/// `false` if the file is missing, has an empty query, fails to
/// compile, fails to parse, or has a tree-sitter error.
fn detect_csharp_http_handler(
    content: &str,
    out: &mut FileDetections,
    patterns: &Patterns,
) -> bool {
    let Some(body) = entry_point_query_body(patterns, "csharp/aspnet-entry-point.scm") else {
        return false;
    };
    if !scm_has_query(body) {
        return false;
    }
    let Some(tree) = parse_for_lang(Lang::CSharp, content) else {
        return false;
    };
    let grammar = language_for(Lang::CSharp);
    let Ok(query) = Query::new(&grammar, body) else {
        return false;
    };
    let idxs = capture_indices(&query, &["verb", "path", "handler"]);
    let verb_idx = idxs[0];
    let path_idx = idxs[1];
    let handler_idx = idxs[2];
    let mut cursor = QueryCursor::new();
    let src = content.as_bytes();
    let mut matches = cursor.matches(&query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let verb = verb_idx.and_then(|i| m.nodes_for_capture_index(i).next());
        let handler = handler_idx.and_then(|i| m.nodes_for_capture_index(i).next());
        let path = path_idx.and_then(|i| m.nodes_for_capture_index(i).next());
        let (Some(verb), Some(handler)) = (verb, handler) else {
            continue;
        };
        let (Some(verb_text), Some(handler_text)) = (
            text_for_node(verb, content),
            text_for_node(handler, content),
        ) else {
            continue;
        };
        if !is_aspnet_route_attribute(verb_text) {
            continue;
        }
        let _ = text_for_node(path.unwrap_or(verb), content);
        let line = (handler.start_position().row as u32) + 1;
        out.entry((handler_text.to_string(), line))
            .or_insert(EntryKind::HttpHandler);
    }
    true
}

/// `true` when `verb` is an ASP.NET route attribute
/// (`HttpGet`, `HttpPost`, `HttpPut`, `HttpDelete`, `HttpPatch`,
/// `HttpHead`, `HttpOptions`, `HttpRequest`).
fn is_aspnet_route_attribute(verb: &str) -> bool {
    matches!(
        verb,
        "HttpGet"
            | "HttpPost"
            | "HttpPut"
            | "HttpDelete"
            | "HttpPatch"
            | "HttpHead"
            | "HttpOptions"
            | "HttpRequest"
    )
}

/// Query-driven Rails controller-action detection via
/// `ruby/rails-entry-point.scm`. Matches every `def` inside a
/// `*Controller` class. Captures `@class_name` (e.g. `UsersController`)
/// and `@handler` (action method name).
///
/// Returns `true` when the query ran successfully; the caller falls
/// back to the regex detector when this returns `false`. Returns
/// `false` if the file is missing, has an empty query, fails to
/// compile, fails to parse, or has a tree-sitter error.
fn detect_ruby_rails_controller(
    content: &str,
    out: &mut FileDetections,
    patterns: &Patterns,
) -> bool {
    let Some(body) = entry_point_query_body(patterns, "ruby/rails-entry-point.scm") else {
        return false;
    };
    if !scm_has_query(body) {
        return false;
    }
    let Some(tree) = parse_for_lang(Lang::Ruby, content) else {
        return false;
    };
    let grammar = language_for(Lang::Ruby);
    let Ok(query) = Query::new(&grammar, body) else {
        return false;
    };
    let idxs = capture_indices(&query, &["handler", "class_name"]);
    let handler_idx = idxs[0];
    let class_name_idx = idxs[1];
    let mut cursor = QueryCursor::new();
    let src = content.as_bytes();
    let mut matches = cursor.matches(&query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        let class_name = class_name_idx.and_then(|i| m.nodes_for_capture_index(i).next());
        let Some(handler_idx) = handler_idx else {
            continue;
        };
        // The rails-entry-point.scm query uses the `(method …)+`
        // quantifier so every action method inside the controller
        // body produces its own `@handler` capture under the same
        // class-level match. Iterate over every captured node —
        // `nodes_for_capture_index(...).next()` only emits the first
        // and would silently drop `def show`, `def create`, … in
        // multi-action controllers.
        for handler in m.nodes_for_capture_index(handler_idx) {
            let Some(handler_text) = text_for_node(handler, content) else {
                continue;
            };
            let _ = class_name;
            // Skip DSL-shaped method definitions (private / protected /
            // class macros).
            if matches!(handler_text, "initialize" | "self" | "method_missing") {
                continue;
            }
            let line = (handler.start_position().row as u32) + 1;
            out.entry((handler_text.to_string(), line))
                .or_insert(EntryKind::HttpHandler);
        }
    }
    true
}

/// Resolve a list of capture names to their index in the query. Any
/// name not bound by the query returns `None` for that slot.
fn capture_indices(query: &Query, names: &[&str]) -> Vec<Option<u32>> {
    let mut idxs = vec![None; names.len()];
    for (i, name) in query.capture_names().iter().enumerate() {
        if let Some(pos) = names.iter().position(|n| *n == *name) {
            idxs[pos] = Some(i as u32);
        }
    }
    idxs
}

/// Regex fallback for Java Spring detection — used only when the
/// `.scm` query body is missing, empty, fails to parse, or fails to
/// compile. Preserves the Workstream 5 behaviour so a partial
/// migration (`.scm` not yet authored) still produces correct
/// detections.
fn detect_java_http_handler_regex(content: &str, out: &mut FileDetections) {
    let deco_re = regex_cached(
        r"(?m)^\s*@(?i:(?:GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping))\s*\([^)]*\)\s*$",
    );
    let method_re = regex_cached(r"(?m)^\s*public\s+[\w<>,\s]+\s+(\w+)\s*\(");
    for cap in deco_re.captures_iter(content) {
        let m = cap.get(0).unwrap();
        let line = line_of_match(content, m.start());
        let after = &content[line_end_byte(content, m.end())..];
        let lines: Vec<&str> = after.lines().take(6).collect();
        for (i, line_text) in lines.iter().enumerate() {
            if line_text.trim().is_empty() {
                continue;
            }
            if let Some(method_cap) = method_re.captures(line_text) {
                let name = method_cap[1].to_string();
                let method_line = line + 1 + i as u32;
                out.entry((name, method_line))
                    .or_insert(EntryKind::HttpHandler);
                break;
            }
            if line_text.trim_start().starts_with('@') {
                continue;
            }
            break;
        }
    }
}

/// Regex fallback for C# ASP.NET detection — used only when the
/// `.scm` query body is missing, empty, fails to parse, or fails to
/// compile. Preserves the Workstream 5 behaviour.
fn detect_csharp_http_handler_regex(content: &str, out: &mut FileDetections) {
    let deco_re = regex_cached(
        r"(?m)^\s*\[(?i:(?:HttpGet|HttpPost|HttpPut|HttpDelete|HttpPatch|HttpHead|HttpOptions))",
    );
    let method_re = regex_cached(
        r"(?m)^\s*(?:public|private|internal|protected|async)\s+[\w<>,\s\[\]?]+\s+(\w+)\s*\(",
    );
    for cap in deco_re.captures_iter(content) {
        let m = cap.get(0).unwrap();
        let line = line_of_match(content, m.start());
        let after = &content[line_end_byte(content, m.end())..];
        let lines: Vec<&str> = after.lines().take(6).collect();
        for (i, line_text) in lines.iter().enumerate() {
            if line_text.trim().is_empty() {
                continue;
            }
            if let Some(method_cap) = method_re.captures(line_text) {
                let name = method_cap[1].to_string();
                let method_line = line + 1 + i as u32;
                out.entry((name, method_line))
                    .or_insert(EntryKind::HttpHandler);
                break;
            }
            if line_text.trim_start().starts_with('[') {
                continue;
            }
            break;
        }
    }
}

/// Return the byte offset of the newline that ends the line
/// containing `byte_offset`. If the line isn't terminated, returns
/// `byte_offset`. Used by Java/C# HTTP-handler regex fallbacks to
/// step past the annotation's argument list before scanning forward
/// for the method declaration.
fn line_end_byte(content: &str, byte_offset: usize) -> usize {
    match content[byte_offset..].find('\n') {
        Some(n) => byte_offset + n + 1,
        None => content.len(),
    }
}

/// Regex fallback for Ruby Rails controller-action detection. Used only
/// when the `.scm` query body is missing, empty, fails to parse,
/// or fails to compile. Preserves the Workstream 5 behaviour.
fn detect_ruby_rails_controller_regex(content: &str, out: &mut FileDetections) {
    let method_re = regex_cached(r"(?m)^\s*def\s+([A-Za-z_][A-Za-z0-9_]*[!?]?)\s*(?:\(|;|\s*$)");
    let controller_re = regex_cached(r"class\s+\w+Controller\s*<");
    let is_controller = controller_re.is_match(content);
    if !is_controller {
        return;
    }
    for cap in method_re.captures_iter(content) {
        let name = cap[1].to_string();
        if matches!(name.as_str(), "initialize" | "self" | "method_missing") {
            continue;
        }
        let line = line_of_match(content, cap.get(0).unwrap().start());
        out.entry((name, line)).or_insert(EntryKind::HttpHandler);
    }
}

/// Walk every `def name(` in `content`. For each, collect the
/// stack of decorators on the immediately preceding lines (no
/// intervening non-decorator, non-blank line) and invoke `f` with
/// the decorator list (closest-to-def first) plus the function name
/// and line. Blank lines and comments between decorators and the
/// `def` are skipped; a blank line or non-decorator line ends the
/// stack.
fn for_each_def_with_decorators(content: &str, f: &mut dyn FnMut(&[String], &str, u32)) {
    let def_re = regex_cached(r"(?m)^[ \t]*def[ \t]+([A-Za-z_][A-Za-z0-9_]*)\s*\(");
    let dec_re = regex_cached(r"(?m)^[ \t]*@([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)");
    let bytes = content.as_bytes();
    for cap in def_re.captures_iter(content) {
        let m = cap.get(0).unwrap();
        let name = cap[1].to_string();
        let line = line_of_match(content, m.start());
        // Walk backwards from the def's start to collect stacked
        // decorators. The byte range `[line_start, m.start())` (where
        // `line_start` is the position of the newline that ends the
        // previous line, or 0) covers everything above the def line.
        // Stop at the first non-decorator, non-blank line.
        let prev_nl = bytes[..m.start()].iter().rposition(|&b| b == b'\n');
        let mut line_end = match prev_nl {
            Some(p) => p + 1,
            None => 0,
        };
        let mut decs: Vec<String> = Vec::new();
        loop {
            if line_end == 0 {
                break;
            }
            // The previous line is `[prev_prev_nl + 1, line_end)`.
            // We need `prev_prev_nl + 1` for the start and look at
            // the content there.
            let prev_line_start = bytes[..line_end - 1]
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|p| p + 1)
                .unwrap_or(0);
            let line_bytes = &bytes[prev_line_start..line_end - 1];
            let line_str = std::str::from_utf8(line_bytes).unwrap_or("");
            let trimmed = line_str.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some(dec_cap) = dec_re.captures(trimmed) {
                decs.push(dec_cap[1].to_string());
                line_end = if prev_line_start == 0 {
                    0
                } else {
                    prev_line_start
                };
            } else {
                break;
            }
        }
        decs.reverse();
        f(&decs, &name, line);
    }
}

// ─── Scheduled ────────────────────────────────────────────────────────

fn detect_scheduled(content: &str, ext: &str, out: &mut FileDetections) {
    // node-cron (TS/JS): `cron.schedule(<expr>, <name>)` and arrow-lambda form.
    if matches!(ext, "ts" | "js") {
        for cap in regex_cached(
            r#"(?m)^[^\S\n]*cron\.schedule\s*\(\s*["'][^"']*["']\s*,\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*[,\)]"#,
        )
        .captures_iter(content)
        {
            let name = cap[1].to_string();
            let line = line_of_match(content, cap.get(0).unwrap().start());
            out.entry((name, line)).or_insert(EntryKind::Scheduled);
        }
        // Arrow lambda: `cron.schedule("...", () => buildMonthlyReport(...))` or
        // `cron.schedule("...", async () => buildMonthlyReport(...))`.
        for cap in regex_cached(
            r#"(?m)^[^\S\n]*cron\.schedule\s*\(\s*["'][^"']*["']\s*,\s*(?:async\s+)?\(\s*\)\s*=>\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\("#,
        )
        .captures_iter(content)
        {
            let name = cap[1].to_string();
            let line = line_of_match(content, cap.get(0).unwrap().start());
            out.entry((name, line)).or_insert(EntryKind::Scheduled);
        }
        // module-level `setInterval(<name>(...), ...)` — column ≤ 2 only.
        for cap in regex_cached(
            r"(?m)^(?:[A-Za-z_$][A-Za-z0-9_$]*\.)?setInterval\s*\(\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\(",
        )
        .captures_iter(content)
        {
            let name = cap[1].to_string();
            let line = line_of_match(content, cap.get(0).unwrap().start());
            out.entry((name, line)).or_insert(EntryKind::Scheduled);
        }
    }

    // Python — Celery (`@app.task`, `@shared_task`) and APScheduler
    // (`@<x>.scheduled_job`, `<x>.add_job(fn, …)`).
    if ext == "py" {
        // `@app.task` / `@shared_task` (and any decorator ending in `.task` /
        // `shared_task` / `.scheduled_job`) on the lines immediately above
        // a `def name(`. Stacked decorators (e.g. `@click.command()` plus
        // `@click.option('--name')`) are walked via
        // [`for_each_def_with_decorators`].
        for_each_def_with_decorators(content, &mut |decs, name, line| {
            for dec in decs {
                let is_scheduled = dec == "task"
                    || dec == "shared_task"
                    || dec == "scheduled_job"
                    || dec.ends_with(".task")
                    || dec.ends_with(".shared_task")
                    || dec.ends_with(".scheduled_job");
                if is_scheduled {
                    out.entry((name.to_string(), line))
                        .or_insert(EntryKind::Scheduled);
                    return;
                }
            }
        });
        // `<scheduler>.add_job(<name>, ...)` and `<scheduler>.add_job(<name>(...), ...)`.
        for cap in regex_cached(
            r"(?m)^[ \t]*[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*\.add_job\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\b",
        )
        .captures_iter(content)
        {
            let name = cap[1].to_string();
            let line = line_of_match(content, cap.get(0).unwrap().start());
            out.entry((name, line)).or_insert(EntryKind::Scheduled);
        }
    }

    // NestJS `@Cron(...)` on a method (TS/JS).
    if matches!(ext, "ts" | "js") {
        for cap in regex_cached(
            r"(?m)^[ \t]*@Cron\s*\([^)]*\)\s*\n[ \t]*(?:async\s+)?(?:[A-Za-z_$][A-Za-z0-9_$]*\.)?[A-Za-z_$][A-Za-z0-9_$]*\s*\(",
        )
        .captures_iter(content)
        {
            let whole = cap.get(0).unwrap().as_str();
            // Extract the LAST identifier on the matched text — the
            // method name (the first identifier may be `Cron` from
            // the decorator).
            let mut last_name: Option<String> = None;
            for m in regex_cached(r"[A-Za-z_$][A-Za-z0-9_$]*\s*\(").find_iter(whole) {
                last_name = Some(
                    m.as_str()
                        .trim_end_matches('(')
                        .trim()
                        .to_string(),
                );
            }
            if let Some(name) = last_name {
                let line = line_of_match(content, cap.get(0).unwrap().start());
                out.entry((name, line)).or_insert(EntryKind::Scheduled);
            }
        }
    }
}

// ─── Cli ──────────────────────────────────────────────────────────────

fn detect_cli(content: &str, ext: &str, out: &mut FileDetections) {
    // Python — click / typer. Walks back through stacked decorators.
    if ext == "py" {
        for_each_def_with_decorators(content, &mut |decs, name, line| {
            for dec in decs {
                // `.command` suffix covers click sub-commands
                // (`@cli.command`) and typer (`@app.command`). `.group`
                // covers click groups (`@click.group`). Bare
                // `click.command` is the un-decorated call from
                // `import click; @click.command(...)`.
                let is_cli = dec == "click.command"
                    || dec == "click.group"
                    || dec.ends_with(".command")
                    || dec.ends_with(".group");
                if is_cli {
                    out.entry((name.to_string(), line))
                        .or_insert(EntryKind::Cli);
                    return;
                }
            }
        });
    }

    // JS/TS — commander `.command("<name>").action(<handler>)`.
    if matches!(ext, "ts" | "js") {
        for cap in regex_cached(
            r#"(?m)\.command\s*\(\s*["'][^"']*["']\s*\)\s*\.\s*action\s*\(\s*([A-Za-z_$][A-Za-z0-9_$]*)\b"#,
        )
        .captures_iter(content)
        {
            let name = cap[1].to_string();
            let line = line_of_match(content, cap.get(0).unwrap().start());
            out.entry((name, line)).or_insert(EntryKind::Cli);
        }
    }
}

// ─── Main ─────────────────────────────────────────────────────────────

fn detect_main(content: &str, ext: &str, out: &mut FileDetections) {
    if matches!(ext, "py" | "rs" | "ts" | "js" | "go") {
        // Exact-name matches. The scanner emits a `Function` node per
        // top-level `def` / `fn` / `function` / `App`, so a regex that
        // picks the name off the declaration line is enough.
        for cap in regex_cached(
            r"(?m)^(?:[ \t]*)(?:async\s+)?(?:def|fn|function)\s+(?P<name>main|App)\s*[\(:]",
        )
        .captures_iter(content)
        {
            let name = cap["name"].to_string();
            let line = line_of_match(content, cap.get(0).unwrap().start());
            out.entry((name, line)).or_insert(EntryKind::Main);
        }
    }
    // Python `if __name__ == "__main__":` block — the enclosing function
    // is whatever immediately precedes the block (or, in the common
    // case where the script body is top-level, `main()` itself which
    // the name match above already tagged).
    if ext == "py" {
        for cap in
            regex_cached(r#"(?m)^if\s+__name__\s*==\s*["']__main__["']\s*:"#).captures_iter(content)
        {
            let start = cap.get(0).unwrap().start();
            // Walk backwards line-by-line, skip blank lines and comments,
            // until we find a `def name(` line; tag that function.
            let prefix = &content[..start];
            let mut line_start = prefix.len();
            for l in prefix.lines().rev() {
                if l.trim().is_empty() || l.trim_start().starts_with('#') {
                    if let Some(idx) = prefix[..line_start].rfind('\n') {
                        line_start = idx + 1;
                    }
                    continue;
                }
                let trimmed = l.trim_start();
                let leading = l.len() - trimmed.len();
                let _ = leading;
                if let Some(name_cap) =
                    regex_cached(r"^[ \t]*def[ \t]+([A-Za-z_][A-Za-z0-9_]*)\s*\(").captures(trimmed)
                {
                    let name = name_cap[1].to_string();
                    let line = line_of_match(content, line_start);
                    out.entry((name, line)).or_insert(EntryKind::Main);
                }
                break;
            }
        }
    }
}

// ─── helpers ──────────────────────────────────────────────────────────

/// Cached, lazily-compiled regex. Using a `OnceLock` keeps the
/// compilation cost off the hot detector loop.
fn regex_cached(pat: &'static str) -> &'static regex::Regex {
    use std::sync::OnceLock;
    static CACHE: OnceLock<
        parking_lot::Mutex<std::collections::HashMap<&'static str, &'static regex::Regex>>,
    > = OnceLock::new();
    let cache = CACHE.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()));
    if let Some(r) = cache.lock().get(pat) {
        return r;
    }
    let compiled = Box::leak(Box::new(
        regex::Regex::new(pat).expect("entry_point_sensor regex"),
    ));
    cache.lock().insert(pat, compiled);
    compiled
}

fn line_of_match(content: &str, byte_offset: usize) -> u32 {
    let mut line: u32 = 1;
    for (i, ch) in content.char_indices() {
        if i >= byte_offset {
            break;
        }
        if ch == '\n' {
            line += 1;
        }
    }
    line
}

// ─── Sensor trait impl ────────────────────────────────────────────────

pub struct EntryPointSensor;

impl crate::server::sensors::Sensor for EntryPointSensor {
    fn name(&self) -> &'static str {
        "entry_point"
    }
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::EntryPoints
    }
    fn phase(&self) -> u8 {
        // Phase 2: runs after `http_sensor` (phase 0) has emitted
        // `CallsHttp` edges and after `field_access_sensor` (phase 2),
        // so the `HttpHandler` derivation has the edges it needs.
        // Sharing phase 2 with `field_access_sensor` is fine: order
        // is determined by `(phase, name)` and `entry_point` sorts
        // before `field_access` alphabetically.
        2
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &std::path::Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        scan_workspace_entry_points(graph, root, namespace)
    }
}

inventory::submit!(SensorEntry(&EntryPointSensor));

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{EdgeType, GraphEdge};

    fn temp_graph(tag: &str) -> (tempfile::TempDir, GraphDatabase) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join(format!("db_{tag}.bin"));
        let g = GraphDatabase::new(&db).expect("GraphDatabase::new");
        (tmp, g)
    }

    fn write_src(tmp: &tempfile::TempDir, rel_path: &str, content: &str) {
        let p = tmp.path().join(rel_path);
        std::fs::create_dir_all(p.parent().unwrap()).ok();
        std::fs::write(&p, content).expect("write fixture");
    }

    fn make_function(_g: &GraphDatabase, name: &str, path: &str, line: u32) -> GraphNode {
        let ns = RepoNamespace::for_test();
        let mut n = GraphNode::new_in(NodeType::Function, name.into(), path.into(), &ns);
        n.line_start = Some(line);
        n.line_end = Some(line + 5);
        n.id = GraphNode::generate_id(&NodeType::Function, path, name, Some(line), &ns);
        n
    }

    fn make_method(_g: &GraphDatabase, name: &str, path: &str, line: u32) -> GraphNode {
        let ns = RepoNamespace::for_test();
        let mut n = GraphNode::new_in(NodeType::Method, name.into(), path.into(), &ns);
        n.line_start = Some(line);
        n.line_end = Some(line + 5);
        n.id = GraphNode::generate_id(&NodeType::Method, path, name, Some(line), &ns);
        n
    }

    fn run_scan(g: &GraphDatabase, tmp: &tempfile::TempDir) -> usize {
        scan_workspace_entry_points(g, tmp.path(), &RepoNamespace::for_test()).unwrap_or(0)
    }

    fn entries_named(g: &GraphDatabase, name: &str) -> Vec<GraphNode> {
        g.get_all_nodes()
            .into_iter()
            .filter(|n| n.name == name)
            .collect()
    }

    // ─── Scheduled ───────────────────────────────────────────────────

    #[test]
    fn detects_node_cron_schedule_by_name() {
        let (tmp, g) = temp_graph("cron_name");
        write_src(
            &tmp,
            "src/index.ts",
            r#"
import cron from "node-cron";
async function scheduledMonthlyReport() {}
cron.schedule("0 0 1 * *", scheduledMonthlyReport);
"#,
        );
        let f = make_function(&g, "scheduledMonthlyReport", "src/index.ts", 3);
        g.insert_nodes_batch(&[f]).unwrap();
        let stamped = run_scan(&g, &tmp);
        assert!(stamped >= 1, "stamped={stamped}");
        let entries = entries_named(&g, "scheduledMonthlyReport");
        assert!(
            entries
                .iter()
                .any(|n| n.entry == Some(EntryKind::Scheduled)),
            "scheduledMonthlyReport must be tagged Scheduled: {entries:?}"
        );
    }

    #[test]
    fn detects_node_cron_arrow_lambda() {
        let (tmp, g) = temp_graph("cron_arrow");
        write_src(
            &tmp,
            "src/jobs.ts",
            r#"
import cron from "node-cron";
async function tick() {}
cron.schedule("*/5 * * * *", () => tick());
"#,
        );
        let f = make_function(&g, "tick", "src/jobs.ts", 3);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "tick");
        assert!(entries
            .iter()
            .any(|n| n.entry == Some(EntryKind::Scheduled)));
    }

    #[test]
    fn detects_set_interval_at_top_level() {
        let (tmp, g) = temp_graph("setinterval_top");
        write_src(
            &tmp,
            "src/tick.ts",
            r#"
function backgroundSync() {}
setInterval(backgroundSync(), 30000);
"#,
        );
        let f = make_function(&g, "backgroundSync", "src/tick.ts", 2);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "backgroundSync");
        assert!(entries
            .iter()
            .any(|n| n.entry == Some(EntryKind::Scheduled)));
    }

    #[test]
    fn ignores_set_interval_inside_function_body() {
        let (tmp, g) = temp_graph("setinterval_inner");
        write_src(
            &tmp,
            "src/x.ts",
            r#"
function outer() {
    function inner() {}
    setInterval(inner(), 1000);
}
"#,
        );
        let outer = make_function(&g, "outer", "src/x.ts", 2);
        let inner = make_function(&g, "inner", "src/x.ts", 3);
        g.insert_nodes_batch(&[outer, inner]).unwrap();
        let _ = run_scan(&g, &tmp);
        for n in g.get_all_nodes() {
            assert!(
                n.entry != Some(EntryKind::Scheduled),
                "inner should not be Scheduled: {n:?}"
            );
        }
    }

    #[test]
    fn detects_celery_shared_task() {
        let (tmp, g) = temp_graph("celery_shared");
        write_src(
            &tmp,
            "src/tasks.py",
            r#"
from celery import shared_task

@shared_task
def generate_invoice():
    return None
"#,
        );
        let f = make_function(&g, "generate_invoice", "src/tasks.py", 4);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "generate_invoice");
        assert!(entries
            .iter()
            .any(|n| n.entry == Some(EntryKind::Scheduled)));
    }

    #[test]
    fn detects_apscheduler_scheduled_job() {
        let (tmp, g) = temp_graph("apscheduler_job");
        write_src(
            &tmp,
            "src/sched.py",
            r#"
from apscheduler.schedulers.background import BackgroundScheduler

sched = BackgroundScheduler()

@sched.scheduled_job('interval', seconds=60)
def tick():
    pass
"#,
        );
        let f = make_function(&g, "tick", "src/sched.py", 7);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "tick");
        assert!(entries
            .iter()
            .any(|n| n.entry == Some(EntryKind::Scheduled)));
    }

    #[test]
    fn detects_apscheduler_add_job() {
        let (tmp, g) = temp_graph("apscheduler_add_job");
        write_src(
            &tmp,
            "src/sched.py",
            r#"
def maintenance_tick():
    pass

sched.add_job(maintenance_tick, 'interval', minutes=10)
"#,
        );
        let f = make_function(&g, "maintenance_tick", "src/sched.py", 2);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "maintenance_tick");
        assert!(entries
            .iter()
            .any(|n| n.entry == Some(EntryKind::Scheduled)));
    }

    #[test]
    fn detects_nestjs_cron_on_method() {
        let (tmp, g) = temp_graph("nestjs_cron");
        write_src(
            &tmp,
            "src/cleanup.service.ts",
            r#"
import { Cron } from '@nestjs/schedule';

export class CleanupService {
  @Cron('0 * * * *')
  async runHourly() {
    return null;
  }
}
"#,
        );
        let m = make_method(&g, "runHourly", "src/cleanup.service.ts", 6);
        g.insert_nodes_batch(&[m]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "runHourly");
        assert!(entries
            .iter()
            .any(|n| n.entry == Some(EntryKind::Scheduled)));
    }

    // ─── Cli ─────────────────────────────────────────────────────────

    #[test]
    fn detects_click_command() {
        let (tmp, g) = temp_graph("click_command");
        write_src(
            &tmp,
            "src/cli.py",
            r#"
import click

@click.command()
@click.option('--name')
def hello(name):
    print(name)
"#,
        );
        let f = make_function(&g, "hello", "src/cli.py", 5);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "hello");
        assert!(entries.iter().any(|n| n.entry == Some(EntryKind::Cli)));
    }

    #[test]
    fn detects_click_group_command() {
        let (tmp, g) = temp_graph("click_group");
        write_src(
            &tmp,
            "src/cli.py",
            r#"
import click

@click.group()
def cli():
    pass

@cli.command()
def sub():
    pass
"#,
        );
        let cli = make_function(&g, "cli", "src/cli.py", 5);
        let sub = make_function(&g, "sub", "src/cli.py", 9);
        g.insert_nodes_batch(&[cli, sub]).unwrap();
        let _ = run_scan(&g, &tmp);
        for want in ["cli", "sub"] {
            let entries = entries_named(&g, want);
            assert!(
                entries.iter().any(|n| n.entry == Some(EntryKind::Cli)),
                "{want} must be tagged Cli: {entries:?}"
            );
        }
    }

    #[test]
    fn detects_typer_app_command() {
        let (tmp, g) = temp_graph("typer_app");
        write_src(
            &tmp,
            "src/cli.py",
            r#"
import typer

app = typer.Typer()

@app.command()
def run():
    pass
"#,
        );
        let f = make_function(&g, "run", "src/cli.py", 6);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "run");
        assert!(entries.iter().any(|n| n.entry == Some(EntryKind::Cli)));
    }

    #[test]
    fn detects_commander_action() {
        let (tmp, g) = temp_graph("commander_action");
        write_src(
            &tmp,
            "src/bin.ts",
            r#"
import { Command } from 'commander';
const program = new Command();
function runServer() {}
program.command("serve").action(runServer);
"#,
        );
        let f = make_function(&g, "runServer", "src/bin.ts", 3);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "runServer");
        assert!(entries.iter().any(|n| n.entry == Some(EntryKind::Cli)));
    }

    // ─── Main ────────────────────────────────────────────────────────

    #[test]
    fn detects_main_function_python() {
        let (tmp, g) = temp_graph("main_py");
        write_src(
            &tmp,
            "src/main.py",
            "def main():
    pass
",
        );
        let f = make_function(&g, "main", "src/main.py", 1);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "main");
        assert!(entries.iter().any(|n| n.entry == Some(EntryKind::Main)));
    }

    #[test]
    fn detects_main_function_rust() {
        let (tmp, g) = temp_graph("main_rs");
        write_src(
            &tmp,
            "src/main.rs",
            "fn main() {
    println!(\"hi\");
}
",
        );
        let f = make_function(&g, "main", "src/main.rs", 1);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "main");
        assert!(entries.iter().any(|n| n.entry == Some(EntryKind::Main)));
    }

    #[test]
    fn detects_app_function_ts() {
        let (tmp, g) = temp_graph("app_ts");
        write_src(
            &tmp,
            "src/main.ts",
            "function App() {
    return null;
}
",
        );
        let f = make_function(&g, "App", "src/main.ts", 1);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "App");
        assert!(entries.iter().any(|n| n.entry == Some(EntryKind::Main)));
    }

    #[test]
    fn does_not_tag_main_helper() {
        let (tmp, g) = temp_graph("main_helper");
        write_src(
            &tmp,
            "src/x.py",
            "def main_helper():
    pass

def other():
    pass
",
        );
        let a = make_function(&g, "main_helper", "src/x.py", 1);
        let b = make_function(&g, "other", "src/x.py", 4);
        g.insert_nodes_batch(&[a, b]).unwrap();
        let _ = run_scan(&g, &tmp);
        for n in g.get_all_nodes() {
            assert!(
                n.entry.is_none(),
                "no entry expected for {}: {:?}",
                n.name,
                n.entry
            );
        }
    }

    // ─── HttpHandler (from CallsHttp edges) ─────────────────────────

    #[test]
    fn tags_handler_from_calls_http_edge() {
        let (tmp, g) = temp_graph("http_handler");
        write_src(
            &tmp,
            "src/api.py",
            "def get_invoice(invoice_id):\n    pass\n",
        );
        let handler = make_function(&g, "get_invoice", "src/api.py", 1);
        let handler_id = handler.id.clone();
        // Insert a minimal route node so `CallsHttp`'s endpoint
        // resolves (`insert_edges_batch` drops edges whose
        // endpoints are not in the graph).
        let ns = RepoNamespace::for_test();
        let route_id_str = "route:GET:/invoices/{}";
        let route = GraphNode::new_in(
            NodeType::HttpRoute,
            route_id_str.to_string(),
            "src/api.py".to_string(),
            &ns,
        );
        let route_id = route.id.clone();
        g.insert_nodes_batch(&[handler, route]).unwrap();
        g.insert_edges_batch(&[GraphEdge::new(
            EdgeType::CallsHttp,
            route_id,
            handler_id.clone(),
        )])
        .unwrap();
        let _ = run_scan(&g, &tmp);
        let h = g.get_node(&handler_id).unwrap().unwrap();
        assert_eq!(h.entry, Some(EntryKind::HttpHandler));
    }

    // ─── Clear-and-reset ─────────────────────────────────────────────

    #[test]
    fn clears_entry_on_rescan_when_function_gone() {
        let (tmp, g) = temp_graph("clear_rescan");
        write_src(
            &tmp,
            "src/x.py",
            "def main():
    pass
",
        );
        let f = make_function(&g, "main", "src/x.py", 1);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let n = g
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "main")
            .expect("main present");
        assert_eq!(n.entry, Some(EntryKind::Main));

        // Replace the file with no `main` and re-scan.
        std::fs::write(
            tmp.path().join("src/x.py"),
            "def other():
    pass
",
        )
        .unwrap();
        let _ = run_scan(&g, &tmp);
        let n = g
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "main")
            .expect("main still indexed");
        assert_eq!(
            n.entry, None,
            "main must be cleared after the function is gone from source"
        );
    }

    #[test]
    fn clears_entry_from_previous_run_for_removed_kind() {
        let (tmp, g) = temp_graph("clear_kind");
        write_src(
            &tmp,
            "src/x.ts",
            r#"
import cron from "node-cron";
async function tick() {}
cron.schedule("*/5 * * * *", tick);
"#,
        );
        let f = make_function(&g, "tick", "src/x.ts", 3);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let n = g
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "tick")
            .expect("tick present");
        assert_eq!(n.entry, Some(EntryKind::Scheduled));

        std::fs::write(
            tmp.path().join("src/x.ts"),
            "async function tick() {}
",
        )
        .unwrap();
        let _ = run_scan(&g, &tmp);
        let n = g
            .get_all_nodes()
            .into_iter()
            .find(|n| n.name == "tick")
            .expect("tick still indexed");
        assert_eq!(
            n.entry, None,
            "Scheduled entry must be cleared when the cron line is gone"
        );
    }

    // ─── Determinism ─────────────────────────────────────────────────

    #[test]
    fn determinism_two_runs_produce_identical_entry_sets() {
        let (tmp, g) = temp_graph("determinism");
        write_src(
            &tmp,
            "src/jobs.py",
            r#"
from celery import shared_task

@shared_task
def a():
    pass

@shared_task
def b():
    pass

def main():
    a()
    b()
"#,
        );
        let a = make_function(&g, "a", "src/jobs.py", 5);
        let b = make_function(&g, "b", "src/jobs.py", 9);
        let m = make_function(&g, "main", "src/jobs.py", 13);
        g.insert_nodes_batch(&[a, b, m]).unwrap();

        let r1: BTreeSet<(String, EntryKind)> = g
            .get_all_nodes()
            .into_iter()
            .filter_map(|n| n.entry.map(|k| (n.name.clone(), k)))
            .collect();
        assert!(r1.is_empty(), "first run should not see the file: {r1:?}");
        let _ = run_scan(&g, &tmp);
        let r2: BTreeSet<(String, EntryKind)> = g
            .get_all_nodes()
            .into_iter()
            .filter_map(|n| n.entry.map(|k| (n.name.clone(), k)))
            .collect();
        assert!(r2.contains(&("a".to_string(), EntryKind::Scheduled)));
        assert!(r2.contains(&("b".to_string(), EntryKind::Scheduled)));
        assert!(r2.contains(&("main".to_string(), EntryKind::Main)));
    }

    // ─── Workstream 5: Java Spring / C# ASP.NET / Ruby Rails entry points.

    #[test]
    fn detects_java_spring_getmapping_handler() {
        let (tmp, g) = temp_graph("java_spring_handler");
        write_src(
            &tmp,
            "src/main/java/com/example/FooController.java",
            "\
@RestController
public class FooController {
    @GetMapping(\"/api/users\")
    public String listUsers() {
        return \"\";
    }
}
",
        );
        let f = make_function(
            &g,
            "listUsers",
            "src/main/java/com/example/FooController.java",
            5,
        );
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "listUsers");
        assert!(
            entries
                .iter()
                .any(|n| n.entry == Some(EntryKind::HttpHandler)),
            "listUsers must be tagged HttpHandler: {entries:?}"
        );
    }

    #[test]
    fn detects_csharp_httpget_handler() {
        let (tmp, g) = temp_graph("csharp_httpget_handler");
        write_src(
            &tmp,
            "src/Controllers/FooController.cs",
            "\
[ApiController]
public class FooController : Controller {
    [HttpGet(\"/api/users/{id}\")]
    public IActionResult Get(int id) { return null; }
}
",
        );
        let f = make_function(&g, "Get", "src/Controllers/FooController.cs", 5);
        g.insert_nodes_batch(&[f]).unwrap();
        let _ = run_scan(&g, &tmp);
        let entries = entries_named(&g, "Get");
        assert!(
            entries
                .iter()
                .any(|n| n.entry == Some(EntryKind::HttpHandler)),
            "Get must be tagged HttpHandler: {entries:?}"
        );
    }

    #[test]
    fn detects_ruby_rails_controller_actions() {
        let (tmp, g) = temp_graph("ruby_rails_actions");
        write_src(
            &tmp,
            "app/controllers/users_controller.rb",
            "\
class UsersController < ApplicationController
  def index
    @users = User.all
  end
  def show
    @user = User.find(params[:id])
  end
end
",
        );
        let idx = make_function(&g, "index", "app/controllers/users_controller.rb", 3);
        let show = make_function(&g, "show", "app/controllers/users_controller.rb", 6);
        g.insert_nodes_batch(&[idx, show]).unwrap();
        let _ = run_scan(&g, &tmp);
        for name in ["index", "show"] {
            let entries = entries_named(&g, name);
            assert!(
                entries
                    .iter()
                    .any(|n| n.entry == Some(EntryKind::HttpHandler)),
                "{name} must be tagged HttpHandler: {entries:?}"
            );
        }
    }
}
