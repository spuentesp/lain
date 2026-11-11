//! Bounded live-only identity evidence from the existing LSP pool.
//! This cache is transient: snapshots must derive their own syntax evidence.
use super::{
    rust_candidate_name, RustGrpcCandidate, RustGrpcConfig, RustGrpcEvidence, RustGrpcSite,
};
use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::lsp::{LspPool, ReferenceLocation};
use crate::schema::RepoNamespace;
use crate::server::sensors::util::{language_for, parse_for_lang, Lang};
use regex::Regex;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

const MAX_PROBES: usize = 256;
const MAX_FILE_PROBES: usize = 32;
const PASS_BUDGET: Duration = Duration::from_secs(15);
const MAX_SOURCE: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DefinitionEvidence {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    pub source_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SemanticRpcEvidence {
    pub source_hash: String,
    pub config_hash: String,
    pub status: String,
    pub definitions: Vec<DefinitionEvidence>,
    pub candidate: Option<RustGrpcCandidate>,
    pub site_name: String,
    pub rpc_key: Option<String>,
}

impl SemanticRpcEvidence {
    /// Never reuse a response after its caller, mapping, or definition changed.
    pub fn current(&self, source: &str, config_hash: &str) -> bool {
        self.source_hash == hash(source)
            && self.config_hash == config_hash
            && !self.definitions.is_empty()
            && self
                .definitions
                .iter()
                .all(|d| read_source(&d.path).is_some_and(|s| hash(&s) == d.source_hash))
    }
}
fn hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

pub fn config_hash(yaml: Option<&str>, query: Option<&str>) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(
        yaml.unwrap_or(include_str!("../patterns/rust-grpc.yaml"))
            .as_bytes(),
    );
    hash.update(b"\0");
    hash.update(
        query
            .unwrap_or(include_str!("../patterns/rust/grpc-candidate.scm"))
            .as_bytes(),
    );
    hash.finalize().to_hex().to_string()
}

fn read_source(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > MAX_SOURCE {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn ancestor<'a>(mut n: Node<'a>, kind: &str) -> Option<Node<'a>> {
    loop {
        if n.kind() == kind {
            return Some(n);
        }
        n = n.parent()?;
    }
}

/// All syntactic method sites are probes, including closure receivers that the
/// conservative syntax detector cannot type. LSP must supply concrete identity.
pub fn probes(source: &str, path: &str, config: &RustGrpcConfig) -> Vec<RustGrpcCandidate> {
    let Some(tree) = parse_for_lang(Lang::Rust, source) else {
        return vec![];
    };
    if tree.root_node().has_error() {
        return vec![];
    }
    let Ok(query) = Query::new(&language_for(Lang::Rust), &config.query) else {
        return vec![];
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        let capture = |name| {
            m.captures
                .iter()
                .find(|c| query.capture_names()[c.index as usize] == name)
                .map(|c| c.node)
        };
        let (Some(call), Some(receiver), Some(method)) =
            (capture("call"), capture("receiver"), capture("method"))
        else {
            continue;
        };
        let text = &source[method.byte_range()];
        if config.deny_methods.iter().any(|s| s == text) {
            continue;
        }
        let function = ancestor(call, "function_item");
        out.push(RustGrpcCandidate {
            method: text.into(),
            receiver: source[receiver.byte_range()].into(),
            generated_type: String::new(),
            site: RustGrpcSite {
                path: path.into(),
                start_byte: method.start_byte(),
                end_byte: method.end_byte(),
                line: method.start_position().row + 1,
                column: method.start_position().column + 1,
            },
            evidence: vec![],
            enclosing_name: function
                .and_then(|f| f.child_by_field_name("name"))
                .map(|n| source[n.byte_range()].to_string()),
            enclosing_start_line: function.map(|f| f.start_position().row + 1),
        });
        if out.len() >= MAX_FILE_PROBES {
            break;
        }
    }
    out
}

fn byte_at_utf16(source: &str, line: u32, column: u32) -> Option<usize> {
    let mut start = 0;
    for _ in 0..line {
        start += source.get(start..)?.find('\n')? + 1;
    }
    let text = source.get(start..)?.split('\n').next()?;
    let mut units = 0;
    for (byte, ch) in text.char_indices() {
        if units == column {
            return Some(start + byte);
        }
        units += ch.len_utf16() as u32;
    }
    (units == column).then_some(start + text.len())
}

/// Inspect the actual method definition, not a receiver suffix. tonic's RPC
/// method contains the transport dispatch, unlike an ordinary same-named class.
pub fn generated_definition(
    source: &str,
    location: &ReferenceLocation,
    method: &str,
    config: &RustGrpcConfig,
) -> Option<(String, String)> {
    let tree = parse_for_lang(Lang::Rust, source)?;
    if tree.root_node().has_error() {
        return None;
    }
    let byte = byte_at_utf16(source, location.line, location.col)?;
    let at = tree.root_node().descendant_for_byte_range(byte, byte)?;
    let function = ancestor(at, "function_item")?;
    let name = function.child_by_field_name("name")?;
    if source[name.byte_range()].trim_start_matches("r#") != method.trim_start_matches("r#") {
        return None;
    }
    let implementation = ancestor(function, "impl_item")?;
    if implementation.child_by_field_name("trait").is_some() {
        return None;
    }
    let ty = implementation.child_by_field_name("type")?;
    let ty = source[ty.byte_range()].split('<').next()?.trim();
    let module = ancestor(implementation, "mod_item")?.child_by_field_name("name")?;
    let qualified = format!("{}::{ty}", &source[module.byte_range()]);
    if !Regex::new(&config.generated_type_pattern)
        .ok()?
        .is_match(&qualified)
    {
        return None;
    }
    let body = source[function.child_by_field_name("body")?.byte_range()]
        .split_whitespace()
        .collect::<String>();
    // Editable call-shape and RPC-path patterns come from this repository's
    // `.lain/patterns/rust-grpc.yaml` override or the bundled conventions.
    if !config.tonic_dispatch_patterns.iter().any(|pattern| {
        Regex::new(pattern)
            .ok()
            .is_some_and(|regex| regex.is_match(&body))
    }) {
        return None;
    }
    let path = Regex::new(&config.rpc_path_pattern).ok()?;
    let captures = path.captures(&body)?;
    let service = captures.name("service")?.as_str();
    let rpc_method = captures.name("method")?.as_str();
    if service.is_empty()
        || rpc_method.contains('/')
        || crate::server::sensors::util::rust_proto_snake_case(rpc_method)
            != method.trim_start_matches("r#")
    {
        return None;
    }
    Some((qualified, format!("rpc:{service}/{rpc_method}")))
}

pub async fn enrich(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
    pool: &LspPool,
    cancel: &CancellationToken,
) -> Result<(), LainError> {
    // Replace, never append. A cancelled/unavailable pass cannot reuse an old
    // type answer. The bounded cache is never written to graph persistence.
    graph.clear_rust_rpc_semantics();
    let config = RustGrpcConfig::load(root)?;
    let yaml = std::fs::read_to_string(root.join(".lain/patterns/rust-grpc.yaml")).ok();
    let query = std::fs::read_to_string(root.join(".lain/patterns/rust/grpc-candidate.scm")).ok();
    let config_hash = config_hash(yaml.as_deref(), query.as_deref());
    let start = Instant::now();
    let mut remaining = MAX_PROBES;
    for (path, source, _) in crate::server::sensors::util::scan_files(root, |p| {
        (p.extension()?.to_str()? == "rs").then(|| "rs".to_string())
    }) {
        if remaining == 0 || start.elapsed() >= PASS_BUDGET || cancel.is_cancelled() {
            break;
        }
        let relative = crate::graph::graph_path(root, &path);
        let mux = pool.next();
        let mut evidence = Vec::new();
        for probe in probes(&source, &relative, &config) {
            if remaining == 0 || start.elapsed() >= PASS_BUDGET || cancel.is_cancelled() {
                break;
            }
            remaining -= 1;
            let prefix = &source[..probe.site.start_byte];
            let col = prefix
                .rsplit('\n')
                .next()
                .unwrap_or("")
                .encode_utf16()
                .count() as u32;
            let response = tokio::select! {
                _ = cancel.cancelled() => break,
                response = tokio::time::timeout(PASS_BUDGET.saturating_sub(start.elapsed()), async {
                    mux.lock().await.get_definitions(&path, probe.site.line as u32 - 1, col, false).await
                }) => response,
            };
            let Ok(Ok(locations)) = response else {
                continue;
            };
            if locations.is_empty() {
                continue;
            }
            let mut definitions = Vec::new();
            let mut candidates = Vec::new();
            let mut missing = false;
            for location in locations.iter().take(9) {
                let Some(definition) = read_source(&location.path) else {
                    missing = true;
                    break;
                };
                let ty = generated_definition(&definition, location, &probe.method, &config);
                definitions.push(DefinitionEvidence {
                    path: location.path.clone(),
                    line: location.line,
                    column: location.col,
                    source_hash: hash(&definition),
                });
                if let Some(ty) = ty {
                    candidates.push(ty);
                }
            }
            if missing || locations.len() > 8 {
                continue;
            }
            // All returned targets must establish one generated identity.
            let candidate = if !candidates.is_empty()
                && candidates.len() == locations.len()
                && candidates.iter().all(|t| t == &candidates[0])
            {
                let mut candidate = probe.clone();
                candidate.generated_type = candidates[0].0.clone();
                candidate.evidence.push(RustGrpcEvidence {
                    site: probe.site.clone(),
                    description: format!(
                        "LSP definition resolved to {}:{}:{}",
                        locations[0].path.display(),
                        locations[0].line + 1,
                        locations[0].col + 1
                    ),
                });
                Some(candidate)
            } else {
                None
            };
            let status = if candidate.is_some() {
                "resolved_definition"
            } else {
                "unproven_definition"
            };
            if let Some(candidate) = &candidate {
                let (node, edges) = super::rust_candidate_anchor(graph, candidate, namespace);
                graph.insert_nodes_batch(&[node])?;
                graph.insert_edges_batch(&edges)?;
            }
            evidence.push(SemanticRpcEvidence {
                source_hash: hash(&source),
                config_hash: config_hash.clone(),
                status: status.into(),
                definitions,
                rpc_key: candidate.as_ref().map(|_| candidates[0].1.clone()),
                candidate,
                site_name: rust_candidate_name(&probe),
            });
        }
        if !evidence.is_empty() {
            graph.set_rust_rpc_semantics(relative, evidence);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{test_support::FakeLspServer, LspMultiplexer};
    use crate::schema::{GraphNode, NodeType};
    use serde_json::json;

    const GENERATED: &str = r#"pub mod orders_client {
        pub struct OrdersClient<T> { inner: T }
        impl<T> OrdersClient<T> {
            pub async fn get(&mut self, req: Request) {
                let path = http::uri::PathAndQuery::from_static("/orders.v1.Orders/Get");
                self.inner.unary(req, path, codec).await
            }
        }
    }"#;

    #[test]
    fn method_definition_requires_tonic_dispatch_and_exact_rpc_path() {
        let config = RustGrpcConfig::from_sources(None, None).unwrap();
        let at = GENERATED.find("get(").unwrap();
        let before = &GENERATED[..at];
        let location = ReferenceLocation {
            path: PathBuf::from("generated.rs"),
            line: before.bytes().filter(|b| *b == b'\n').count() as u32,
            col: before.rsplit('\n').next().unwrap().encode_utf16().count() as u32,
            context: String::new(),
        };
        assert_eq!(
            generated_definition(GENERATED, &location, "get", &config),
            Some((
                "orders_client::OrdersClient".into(),
                "rpc:orders.v1.Orders/Get".into()
            ))
        );
        assert!(generated_definition(GENERATED, &location, "delete", &config).is_none());
        assert!(generated_definition(
            &GENERATED.replace(".unary(", ".ordinary("),
            &location,
            "get",
            &config
        )
        .is_none());
        assert!(generated_definition(
            &GENERATED.replace("/orders.v1.Orders/Get", "/orders.v1.Orders/Delete"),
            &location,
            "get",
            &config
        )
        .is_none());
        let mut custom = config.clone();
        custom.tonic_dispatch_patterns = vec![r"\.unary\(".into()];
        custom.rpc_path_pattern =
            r#"PathAndQuery::from_static\("/(?P<service>[^/]+)/(?P<method>[^/]+)"\)"#.into();
        assert!(generated_definition(GENERATED, &location, "get", &custom).is_some());
        custom.rpc_path_pattern =
            r#"OtherPath::static\("/(?P<service>[^/]+)/(?P<method>[^/]+)"\)"#.into();
        assert!(generated_definition(GENERATED, &location, "get", &custom).is_none());
        let custom = RustGrpcConfig::from_sources(
            Some(
                &include_str!("../patterns/rust-grpc.yaml")
                    .replace("PathAndQuery::from_static", "OtherPath::static"),
            ),
            None,
        );
        assert!(custom.is_ok());
        assert_eq!(byte_at_utf16("a😀b", 0, 3), Some(5));
        assert_eq!(byte_at_utf16("a😀b", 0, 2), None);
    }

    #[tokio::test]
    async fn live_lsp_resolves_a_closure_receiver_and_stale_evidence_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let caller = root.path().join("caller.rs");
        // A non-ASCII prefix verifies that requests use UTF-16, not byte columns.
        let source = "fn wrapper() { let label = \"😀\"; unary(|mut grpc| async move { grpc.get(req).await }); }";
        std::fs::write(&caller, source).unwrap();
        let generated = root.path().join("generated.rs");
        std::fs::write(&generated, GENERATED).unwrap();
        let config = RustGrpcConfig::from_sources(None, None).unwrap();
        assert!(
            super::super::detect_rust_grpc_candidates(source, "caller.rs", &config)
                .unwrap()
                .is_empty()
        );
        let probe = probes(source, "caller.rs", &config)
            .into_iter()
            .find(|p| p.method == "get")
            .unwrap();
        let prefix = &source[..probe.site.start_byte];
        let column = prefix.encode_utf16().count() as u32;
        let before = &GENERATED[..GENERATED.find("get(").unwrap()];
        let line = before.bytes().filter(|b| *b == b'\n').count() as u32;
        let col = before.rsplit('\n').next().unwrap().encode_utf16().count() as u32;
        let server = FakeLspServer::bind().await.unwrap();
        // The fixture shares canned location responses across identity/reference
        // methods, while recording each wire method independently.
        server.set_references(&crate::lsp::file_uri(&caller), 0, column, json!([{
            "targetUri": crate::lsp::file_uri(&generated),
            "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 8, "character": 0}},
            "targetSelectionRange": {"start": {"line": line, "character": col}, "end": {"line": line, "character": col + 3}}
        }])).await;
        let mux = LspMultiplexer::with_server_url(
            root.path(),
            &server.host(),
            server.port(),
            &crate::tuning::RuntimeConfig::default(),
        )
        .await
        .unwrap();
        let handle = server.spawn();
        let pool = LspPool::from_test_multiplexer(mux);
        let graph = GraphDatabase::empty_writable();
        let namespace = RepoNamespace::for_test();
        let mut enclosing = GraphNode::new_in(
            NodeType::Function,
            "wrapper".into(),
            "caller.rs".into(),
            &namespace,
        );
        enclosing.line_start = Some(0);
        enclosing.line_end = Some(0);
        graph.insert_nodes_batch(&[enclosing]).unwrap();
        enrich(
            &graph,
            root.path(),
            &namespace,
            &pool,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let evidence = graph.rust_rpc_semantics("caller.rs");
        let evidence = evidence
            .iter()
            .find(|e| e.site_name == rust_candidate_name(&probe))
            .unwrap();
        assert_eq!(evidence.status, "resolved_definition");
        assert_eq!(
            evidence.rpc_key.as_deref(),
            Some("rpc:orders.v1.Orders/Get")
        );
        assert!(evidence.current(source, &config_hash(None, None)));
        assert!(!evidence.current(&format!("{source} // changed"), &config_hash(None, None)));
        assert!(!evidence.current(source, "changed-mapping"));
        assert!(graph
            .get_nodes_in_path("caller.rs")
            .iter()
            .any(|n| n.name == rust_candidate_name(&probe)
                && n.container.as_deref() == Some("wrapper")));
        assert!(!handle.received().definitions.lock().unwrap().is_empty());
        std::fs::write(&generated, "struct Other;").unwrap();
        assert!(!evidence.current(source, &config_hash(None, None)));
        handle.shutdown().await;
    }
}
