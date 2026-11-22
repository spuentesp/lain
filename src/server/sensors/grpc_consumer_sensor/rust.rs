//! Bounded, snapshot-only Rust generated-client candidate discovery.
use crate::error::LainError;
use crate::server::sensors::util::{language_for, parse_for_lang, scan_files, Lang};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

#[cfg(test)]
thread_local! { static PARSE_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
pub(super) fn parse_rust_source(source: &str) -> Option<tree_sitter::Tree> {
    #[cfg(test)]
    PARSE_RUNS.with(|n| n.set(n.get() + 1));
    let tree = parse_for_lang(Lang::Rust, source)?;
    if !tree.root_node().has_error() {
        return Some(tree);
    }
    // The bundled grammar rejects some attribute placements (`#[cfg(..)]` on a
    // field of a destructuring pattern), and one error voids the whole file.
    // Retry with attributes blanked out at the same byte offsets; keep that
    // tree only when it parses cleanly, otherwise report the original.
    let blanked = blank_attributes(source);
    if blanked != source {
        if let Some(retry) = parse_for_lang(Lang::Rust, &blanked) {
            if !retry.root_node().has_error() {
                return Some(retry);
            }
        }
    }
    Some(tree)
}

/// `source` with every `#[...]` attribute replaced by spaces (newlines kept),
/// so byte offsets and line numbers are unchanged.
fn blank_attributes(source: &str) -> String {
    static ATTRIBUTE: OnceLock<Regex> = OnceLock::new();
    let re = ATTRIBUTE.get_or_init(|| Regex::new(r"#!?\[[^\]]*\]").expect("attribute regex"));
    re.replace_all(source, |m: &regex::Captures| {
        m[0].chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect::<String>()
    })
    .into_owned()
}

#[cfg(test)]
thread_local! { static CANDIDATE_QUERY_COMPILES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub(super) fn candidate_query(source: &str) -> Result<Query, LainError> {
    #[cfg(test)]
    CANDIDATE_QUERY_COMPILES.with(|count| count.set(count.get() + 1));
    Query::new(&language_for(Lang::Rust), source)
        .map_err(|error| LainError::Other(format!("Rust gRPC query: {error}")))
}

/// Byte offsets are half-open; lines and columns are one-based.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustGrpcSite {
    pub path: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustGrpcEvidence {
    pub description: String,
    pub site: RustGrpcSite,
}

/// Typed identity evidence; descriptions are display text only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RustGrpcProvenance {
    Generated,
    ConfiguredWrapper,
    FollowedWrapper,
    InferredWrapper,
}
impl RustGrpcProvenance {
    pub fn is_wrapper(self) -> bool {
        !matches!(self, Self::Generated)
    }
}

/// A generated-client receiver with a method call, not a proven protobuf RPC.
/// No destination, normalized method, contract fact, or caller edge is inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustGrpcCandidate {
    pub provenance: RustGrpcProvenance,
    pub method: String,
    pub receiver: String,
    pub generated_type: String,
    pub site: RustGrpcSite,
    pub evidence: Vec<RustGrpcEvidence>,
    pub enclosing_name: Option<String>,
    pub enclosing_start_line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustGrpcConfig {
    pub generated_type_pattern: String,
    #[serde(default)]
    pub client_module_template: Option<String>,
    #[serde(default)]
    pub client_type_template: Option<String>,
    pub constructors: Vec<String>,
    #[serde(default)]
    pub async_constructors: Vec<String>,
    pub deny_methods: Vec<String>,
    #[serde(default = "default_tonic_dispatch_patterns")]
    pub tonic_dispatch_patterns: Vec<String>,
    #[serde(default = "default_rpc_path_pattern")]
    pub rpc_path_pattern: String,
    pub accessors: Vec<Accessor>,
    #[serde(default)]
    pub wrappers: Vec<Wrapper>,
    #[serde(skip)]
    pub(super) query: String,
    #[serde(skip)]
    followed_wrappers: Vec<FollowedWrapper>,
    #[serde(skip)]
    method_counts: HashMap<String, usize>,
    /// Simple names of types the repository defines itself that wrap a
    /// generated client (no tonic dispatch in their methods). Their own
    /// module is often named `*_client`, so the generated-type pattern
    /// matches them; a call on one is not an RPC unless a wrapper maps it.
    #[serde(skip)]
    local_wrapper_types: BTreeSet<String>,
    #[serde(skip)]
    prepared: Option<Arc<PreparedRepository>>,
}

#[derive(Debug)]
struct PreparedFile {
    hash: [u8; 32],
    candidates: Vec<RustGrpcCandidate>,
    probes: Vec<RustGrpcCandidate>,
}
#[derive(Debug)]
struct PreparedRepository {
    config_hash: [u8; 32],
    wrappers: Vec<FollowedWrapper>,
    method_counts: HashMap<String, usize>,
    local_wrapper_types: BTreeSet<String>,
    files: BTreeMap<String, PreparedFile>,
}
fn repository_cache(
) -> &'static parking_lot::Mutex<lru::LruCache<[u8; 32], Arc<PreparedRepository>>> {
    static CACHE: OnceLock<parking_lot::Mutex<lru::LruCache<[u8; 32], Arc<PreparedRepository>>>> =
        OnceLock::new();
    CACHE.get_or_init(|| {
        parking_lot::Mutex::new(lru::LruCache::new(std::num::NonZeroUsize::new(8).unwrap()))
    })
}
impl RustGrpcConfig {
    fn source_config_hash(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(&serde_json::to_vec(self).expect("Rust gRPC config serialization"));
        hash.update(self.query.as_bytes());
        *hash.finalize().as_bytes()
    }
    fn prepared_file(&self, source: &str, path: &str) -> Option<&PreparedFile> {
        let repository = self.prepared.as_ref()?;
        if repository.config_hash != self.source_config_hash() {
            return None;
        }
        repository
            .files
            .get(path)
            .filter(|file| file.hash == *blake3::hash(source.as_bytes()).as_bytes())
    }
    pub(super) fn cached_probes(&self, source: &str, path: &str) -> Option<Vec<RustGrpcCandidate>> {
        self.prepared_file(source, path)
            .map(|file| file.probes.clone())
    }
}

fn default_tonic_dispatch_patterns() -> Vec<String> {
    [
        r"\.unary\(",
        r"\.server_streaming\(",
        r"\.client_streaming\(",
        r"\.streaming\(",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}
fn default_rpc_path_pattern() -> String {
    r#"PathAndQuery::from_static\("/(?P<service>[^/"]+)/(?P<method>[^/"]+)"\)"#.into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accessor {
    pub receiver_type: String,
    pub method: String,
    pub return_type: String,
    pub provenance: String,
}

/// Explicit delegation from a hand-written wrapper to a generated RPC method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wrapper {
    pub receiver_type: String,
    pub method: String,
    pub generated_type: String,
    pub rpc_method: String,
    pub provenance: String,
}

#[derive(Debug, Clone)]
struct FollowedWrapper {
    mapping: Wrapper,
    declaration: RustGrpcSite,
    evidence: Vec<RustGrpcEvidence>,
}

struct CompiledConfig {
    generated: Regex,
    query: Query,
}

impl RustGrpcConfig {
    /// Invalid overrides fail explicitly; they never silently broaden detection.
    pub fn load(root: &Path) -> Result<Self, LainError> {
        let dir = root.join(".lain/patterns");
        let yaml = read_override(
            &dir.join("rust-grpc.yaml"),
            include_str!("../patterns/rust-grpc.yaml"),
        )?;
        let query = read_override(
            &dir.join("rust/grpc-candidate.scm"),
            include_str!("../patterns/rust/grpc-candidate.scm"),
        )?;
        Self::from_sources(Some(&yaml), Some(&query))
    }

    /// Load pinned source blobs without a worktree or LSP connection.
    pub fn from_sources(yaml: Option<&str>, query: Option<&str>) -> Result<Self, LainError> {
        let mut config: Self =
            serde_yaml::from_str(yaml.unwrap_or(include_str!("../patterns/rust-grpc.yaml")))?;
        config.query = query
            .unwrap_or(include_str!("../patterns/rust/grpc-candidate.scm"))
            .into();
        config.compile()?;
        Ok(config)
    }

    fn compile(&self) -> Result<CompiledConfig, LainError> {
        let generated = Regex::new(&self.generated_type_pattern)
            .map_err(|e| LainError::Other(format!("Rust gRPC type pattern: {e}")))?;
        let query = candidate_query(&self.query)?;
        for capture in ["call", "receiver", "method"] {
            if !query.capture_names().contains(&capture) {
                return Err(LainError::Other(format!(
                    "Rust gRPC query missing @{capture}"
                )));
            }
        }
        for pattern in &self.tonic_dispatch_patterns {
            Regex::new(pattern)
                .map_err(|e| LainError::Other(format!("Rust gRPC dispatch pattern: {e}")))?;
        }
        let path = Regex::new(&self.rpc_path_pattern)
            .map_err(|e| LainError::Other(format!("Rust gRPC path pattern: {e}")))?;
        if path.capture_names().flatten().collect::<Vec<_>>() != ["service", "method"] {
            return Err(LainError::Other(
                "Rust gRPC path pattern must define named service and method captures".into(),
            ));
        }
        if self
            .accessors
            .iter()
            .any(|a| a.provenance.trim().is_empty())
        {
            return Err(LainError::Other(
                "Rust gRPC accessor requires provenance".into(),
            ));
        }
        let mut methods = std::collections::HashSet::new();
        for wrapper in &self.wrappers {
            if wrapper.receiver_type.trim().is_empty()
                || wrapper.method.trim().is_empty()
                || wrapper.rpc_method.trim().is_empty()
                || wrapper.provenance.trim().is_empty()
                || !generated.is_match(&wrapper.generated_type)
                || !methods.insert((&wrapper.receiver_type, &wrapper.method))
            {
                return Err(LainError::Other(
                    "Rust gRPC wrapper requires a unique receiver/method, generated target, RPC method and provenance".into(),
                ));
            }
        }
        Ok(CompiledConfig { generated, query })
    }
}

fn read_override(path: &Path, bundled: &str) -> Result<String, LainError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(bundled.to_string()),
        Err(e) => Err(e.into()),
    }
}

/// Discover candidates without LSP or graph mutation. Rust is intentionally not
/// passed into the existing `detect_stub_calls` / fact emission pipeline.
pub fn discover_rust_grpc_candidates(root: &Path) -> Result<Vec<RustGrpcCandidate>, LainError> {
    let mut config = RustGrpcConfig::load(root)?;
    let sources: Vec<_> = scan_files(root, |p| {
        (p.extension()?.to_str()? == "rs").then(|| "rs".to_string())
    })
    .map(|(path, source, _)| (crate::graph::graph_path(root, &path), source))
    .collect();
    follow_rust_grpc_wrappers(&mut config, &sources)?;
    let mut out = Vec::new();
    for (path, content) in &sources {
        out.extend(detect_rust_grpc_candidates(content, path, &config)?);
    }
    out.sort_by(|a, b| (&a.site.path, a.site.start_byte).cmp(&(&b.site.path, b.site.start_byte)));
    Ok(out)
}

pub fn detect_rust_grpc_candidates(
    src: &str,
    path: &str,
    config: &RustGrpcConfig,
) -> Result<Vec<RustGrpcCandidate>, LainError> {
    if let Some(file) = config.prepared_file(src, path) {
        return Ok(file.candidates.clone());
    }
    // The current source/config is outside the prepared generation. Rebuild
    // local proofs instead of applying an old wrapper body to changed text.
    let stale_repository = config.prepared.is_some();
    let mut local = config.clone();
    local.prepared = None;
    local.followed_wrappers.clear();
    local.method_counts.clear();
    local.local_wrapper_types.clear();
    follow_rust_grpc_wrappers(&mut local, &[(path.to_string(), src.to_string())])?;
    let mut candidates = match local.prepared_file(src, path) {
        Some(file) => file.candidates.clone(),
        None => detect_with_compiled_config(src, path, &local, &local.compile()?)?,
    };
    if stale_repository {
        // Other files may now define competing methods. Only a complete
        // repository refresh can justify name-only wrapper inference again.
        candidates.retain(|c| c.provenance != RustGrpcProvenance::InferredWrapper);
    }
    Ok(candidates)
}

/// Build a transient repository-local delegation table from the supplied source
/// revision. Explicit operator mappings are kept separate and always win.
pub fn follow_rust_grpc_wrappers(
    config: &mut RustGrpcConfig,
    sources: &[(String, String)],
) -> Result<(), LainError> {
    let config_hash = config.source_config_hash();
    let mut hash = blake3::Hasher::new();
    hash.update(&config_hash);
    for (path, source) in sources
        .iter()
        .map(|(p, s)| (p, s))
        .collect::<BTreeMap<_, _>>()
    {
        hash.update(&(path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update(blake3::hash(source.as_bytes()).as_bytes());
    }
    let key = *hash.finalize().as_bytes();
    if let Some(hit) = repository_cache().lock().get(&key).cloned() {
        config.followed_wrappers = hit.wrappers.clone();
        config.method_counts = hit.method_counts.clone();
        config.local_wrapper_types = hit.local_wrapper_types.clone();
        config.prepared = Some(hit);
        return Ok(());
    }
    let compiled = config.compile()?;
    let mut direct = config.clone();
    direct.wrappers.clear();
    direct.followed_wrappers.clear();
    direct.method_counts.clear();
    direct.local_wrapper_types.clear();
    let dispatch = config
        .tonic_dispatch_patterns
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect::<Vec<_>>();
    let mut vendored_types = BTreeSet::new();
    let mut local_types = BTreeSet::new();
    let mut wrappers = Vec::new();
    let mut counts = HashMap::new();
    let mut trees = BTreeMap::new();
    for (path, src) in sources {
        let Some(tree) = parse_rust_source(src) else {
            continue;
        };
        if tree.root_node().has_error() {
            continue;
        }
        let mut ctx = Context::new(src, path, &direct, &compiled.generated);
        ctx.index(tree.root_node(), SyntaxContext::default());
        let calls = detect_with_tree(src, path, &direct, &compiled, &tree)?;
        trees.insert(path.clone(), tree.clone());
        for (name, methods) in &ctx.methods {
            *counts.entry(name.clone()).or_insert(0) += methods.len();
            if config.deny_methods.contains(name) {
                continue;
            }
            for method in methods {
                let Some(owner) = ctx.impl_type(*method) else {
                    continue;
                };
                let owner_name = simple_type_name(&owner);
                if dispatch.iter().any(|d| d.is_match(ctx.text(*method))) {
                    vendored_types.insert(owner_name.clone());
                } else {
                    local_types.insert(owner_name.clone());
                }
                if compiled.generated.is_match(&owner) {
                    continue;
                }
                let targets: Vec<_> = calls
                    .iter()
                    .filter(|call| {
                        // A call on the wrapper type itself (`self.workspace()`) is a
                        // helper, not the delegation this body is being judged by.
                        simple_type_name(&call.generated_type) != owner_name
                    })
                    .filter(|call| {
                        call.site.start_byte >= method.start_byte()
                            && call.site.end_byte <= method.end_byte()
                            && call.enclosing_start_line == Some(method.start_position().row + 1)
                            && call.enclosing_name.as_ref() == Some(name)
                    })
                    .collect();
                let identities: BTreeSet<_> = targets
                    .iter()
                    .map(|c| (&c.generated_type, &c.method))
                    .collect();
                if identities.len() != 1 {
                    continue;
                }
                let target = targets[0];
                let mut evidence = target.evidence.clone();
                evidence.push(ctx.evidence(
                    *method,
                    format!(
                        "wrapper_followed body: {}::{} -> {}::{}",
                        owner, name, target.generated_type, target.method
                    ),
                ));
                // Cargo's conventional crates/<name>/src layout supplies the
                // external crate/module path; local unqualified types also match.
                let receiver_type = wrapper_type_path(path, &owner, *method, &ctx);
                wrappers.push(FollowedWrapper {
                    declaration: ctx.site(*method),
                    mapping: Wrapper {
                        receiver_type,
                        method: name.clone(),
                        generated_type: target.generated_type.clone(),
                        rpc_method: target.method.clone(),
                        provenance: "wrapper_followed".into(),
                    },
                    evidence,
                });
            }
        }
    }
    config.followed_wrappers = wrappers;
    config.method_counts = counts;
    config.local_wrapper_types = local_types.difference(&vendored_types).cloned().collect();
    let mut files = BTreeMap::new();
    for (path, source) in sources {
        let (candidates, probes) = if let Some(tree) = trees.get(path) {
            let candidates = detect_with_tree(source, path, config, &compiled, tree)?;
            let mapped = candidates
                .iter()
                .filter(|candidate| candidate.provenance.is_wrapper())
                .map(|candidate| candidate.site.start_byte)
                .collect();
            let probes = super::semantic::probes_with_tree(
                source,
                path,
                config,
                tree,
                &compiled.query,
                &mapped,
            );
            (candidates, probes)
        } else {
            (vec![], vec![])
        };
        files.insert(
            path.clone(),
            PreparedFile {
                hash: *blake3::hash(source.as_bytes()).as_bytes(),
                candidates,
                probes,
            },
        );
    }
    let prepared = Arc::new(PreparedRepository {
        config_hash,
        wrappers: config.followed_wrappers.clone(),
        method_counts: config.method_counts.clone(),
        local_wrapper_types: config.local_wrapper_types.clone(),
        files,
    });
    // Cache derived evidence only, never sources or parse trees. Bound each
    // repository by its serialized evidence size and retain at most eight.
    let cost: usize = prepared
        .files
        .iter()
        .map(|(path, file)| {
            // Empty files still retain their path, hash and vector headers.
            let entry_cost = path.len()
                + std::mem::size_of::<String>()
                + std::mem::size_of::<PreparedFile>()
                + 6 * std::mem::size_of::<usize>();
            entry_cost
                + file
                    .candidates
                    .iter()
                    .chain(file.probes.iter())
                    .map(|c| serde_json::to_vec(c).map_or(0, |v| v.len()))
                    .sum::<usize>()
        })
        .sum::<usize>()
        + prepared
            .method_counts
            .keys()
            .map(|name| name.len() + 32)
            .sum::<usize>()
        + prepared
            .wrappers
            .iter()
            .map(|wrapper| format!("{wrapper:?}").len())
            .sum::<usize>();
    if cost <= 4 * 1024 * 1024 {
        repository_cache().lock().put(key, prepared.clone());
    }
    config.prepared = Some(prepared);
    Ok(())
}

fn wrapper_type_path(path: &str, owner: &str, method: Node<'_>, ctx: &Context<'_>) -> String {
    if owner.contains("::") {
        return owner.to_string();
    }
    let (crate_name, module) = if let Some((crate_path, module)) = path.split_once("/src/") {
        (
            crate_path
                .rsplit('/')
                .next()
                .unwrap_or(crate_path)
                .replace('-', "_"),
            module,
        )
    } else if let Some(module) = path.strip_prefix("src/") {
        ("crate".to_string(), module)
    } else {
        return owner.to_string();
    };
    let module = module.trim_end_matches(".rs").trim_end_matches("/mod");
    let mut parts = vec![crate_name];
    if !matches!(module, "lib" | "main" | "mod") {
        parts.extend(module.split('/').map(str::to_string));
    }
    let mut inline = Vec::new();
    let mut node = ctx.parent(method);
    while let Some(n) = node {
        if n.kind() == "mod_item" {
            if let Some(name) = ctx.field(n, "name") {
                inline.push(ctx.text(name).to_string());
            }
        }
        node = ctx.parent(n);
    }
    parts.extend(inline.into_iter().rev());
    parts.push(owner.to_string());
    parts.join("::")
}

#[cfg(test)]
thread_local! { static DETECTION_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

fn detect_with_compiled_config(
    src: &str,
    path: &str,
    config: &RustGrpcConfig,
    compiled: &CompiledConfig,
) -> Result<Vec<RustGrpcCandidate>, LainError> {
    #[cfg(test)]
    DETECTION_RUNS.with(|n| n.set(n.get() + 1));
    let Some(tree) = parse_rust_source(src) else {
        return Ok(Vec::new());
    };
    detect_with_tree(src, path, config, compiled, &tree)
}

fn detect_with_tree(
    src: &str,
    path: &str,
    config: &RustGrpcConfig,
    compiled: &CompiledConfig,
    tree: &tree_sitter::Tree,
) -> Result<Vec<RustGrpcCandidate>, LainError> {
    // Damaged syntax cannot supply reliable scope/type evidence.
    if tree.root_node().has_error() {
        return Ok(Vec::new());
    }
    let mut ctx = Context::new(src, path, config, &compiled.generated);
    ctx.index(tree.root_node(), SyntaxContext::default());
    let query = &compiled.query;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), src.as_bytes());
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
        if config.deny_methods.iter().any(|x| x == ctx.text(method)) {
            continue;
        }
        let resolved = ctx.expression_type(receiver, call, 0);
        let known_type = resolved.as_ref().map(|(ty, _)| ty.as_str());
        let explicit = known_type.and_then(|ty| {
            config
                .wrappers
                .iter()
                .find(|w| w.receiver_type == ty && w.method == ctx.text(method))
        });
        let followed: Vec<_> = config
            .followed_wrappers
            .iter()
            .filter(|w| {
                w.mapping.method == ctx.text(method)
                    && match known_type {
                        Some(ty) => ctx.wrapper_type_matches(w, ty, call),
                        None => {
                            config.method_counts.get(ctx.text(method)) == Some(&1)
                                && receiver_matches_wrapper(
                                    ctx.text(receiver),
                                    &w.mapping.receiver_type,
                                )
                        }
                    }
            })
            .collect();
        let automatic = (followed.len() == 1).then(|| followed[0]);
        let type_known = known_type.is_some();
        let (mut ty, mut evidence) = resolved.unwrap_or_default();
        let rpc_method = if let Some(wrapper) = explicit {
            evidence.push(ctx.evidence(
                method,
                format!(
                    "configured wrapper: {}::{} -> {}::{} ({})",
                    ty,
                    wrapper.method,
                    wrapper.generated_type,
                    wrapper.rpc_method,
                    wrapper.provenance
                ),
            ));
            ty = wrapper.generated_type.clone();
            wrapper.rpc_method.as_str()
        } else if let Some(wrapper) = automatic {
            evidence.extend(wrapper.evidence.clone());
            evidence.push(ctx.evidence(
                method,
                format!(
                    "wrapper_followed: {}::{} -> {}::{}; {}",
                    wrapper.mapping.receiver_type,
                    wrapper.mapping.method,
                    wrapper.mapping.generated_type,
                    wrapper.mapping.rpc_method,
                    if type_known {
                        "receiver_type; confidence=1.0"
                    } else {
                        "unique_method_name; confidence=0.6"
                    }
                ),
            ));
            ty = wrapper.mapping.generated_type.clone();
            wrapper.mapping.rpc_method.as_str()
        } else {
            ctx.text(method)
        };
        // A locally defined wrapper type is not a generated client: an unmapped
        // call on it (`client.workspace()`) is ordinary code, not an RPC stub.
        let unmapped_local_wrapper = explicit.is_none()
            && automatic.is_none()
            && config.local_wrapper_types.contains(&simple_type_name(&ty));
        if ctx.generated.is_match(&ty)
            && !unmapped_local_wrapper
            && !config.deny_methods.iter().any(|m| m == rpc_method)
        {
            out.push(RustGrpcCandidate {
                provenance: if explicit.is_some() {
                    RustGrpcProvenance::ConfiguredWrapper
                } else if automatic.is_some() && type_known {
                    RustGrpcProvenance::FollowedWrapper
                } else if automatic.is_some() {
                    RustGrpcProvenance::InferredWrapper
                } else {
                    RustGrpcProvenance::Generated
                },
                method: rpc_method.into(),
                receiver: ctx.text(receiver).into(),
                generated_type: ty,
                site: ctx.site(method),
                evidence,
                enclosing_name: ctx
                    .enclosing_function(call)
                    .and_then(|f| f.child_by_field_name("name"))
                    .map(|n| ctx.text(n).to_string()),
                enclosing_start_line: ctx
                    .enclosing_function(call)
                    .map(|f| f.start_position().row + 1),
            });
        }
    }
    out.sort_by_key(|c| c.site.start_byte);
    out.dedup_by_key(|c| c.site.start_byte);
    Ok(out)
}

/// `a::b::Type<T>` -> `Type`.
fn simple_type_name(ty: &str) -> String {
    ty.split('<')
        .next()
        .unwrap_or(ty)
        .trim()
        .rsplit("::")
        .next()
        .unwrap_or(ty)
        .trim()
        .to_string()
}

fn receiver_matches_wrapper(receiver: &str, wrapper: &str) -> bool {
    let receiver = receiver.rsplit('.').next().unwrap_or(receiver).trim();
    let wrapper = wrapper.rsplit("::").next().unwrap_or(wrapper);
    let normalize = |s: &str| {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    !receiver.is_empty() && normalize(receiver) == normalize(wrapper)
}

#[derive(Clone, Copy, Default)]
// Strict ancestors of each node, matching the original parent-based scope rules.
struct SyntaxContext<'a> {
    parent: Option<Node<'a>>,
    scope: Option<Node<'a>>,
    function: Option<Node<'a>>,
    implementation: Option<Node<'a>>,
    in_closure: bool,
}

struct TypeBinding<'a> {
    node: Node<'a>,
    // Nominal declarations retain the caller's spelling. Aliases and imports
    // carry their explicit expansion, resolved at the declaration's own site.
    target: Option<String>,
}

struct Context<'a> {
    src: &'a str,
    path: &'a str,
    config: &'a RustGrpcConfig,
    generated: &'a Regex,
    // Transient per-file indexes: receiver resolution must not repeatedly scan
    // every AST node or walk tree-sitter parents in large, wide source files.
    syntax: HashMap<usize, SyntaxContext<'a>>,
    types: HashMap<String, Vec<TypeBinding<'a>>>,
    bindings: HashMap<Option<usize>, HashMap<String, Vec<Node<'a>>>>,
    patterns: HashMap<String, Vec<Node<'a>>>,
    assignments: HashMap<String, Vec<Node<'a>>>,
    structs: HashMap<String, Vec<Node<'a>>>,
    fields: HashMap<String, Vec<Node<'a>>>,
    methods: HashMap<String, Vec<Node<'a>>>,
}
type Resolved = (String, Vec<RustGrpcEvidence>);

impl<'a> Context<'a> {
    fn new(src: &'a str, path: &'a str, config: &'a RustGrpcConfig, generated: &'a Regex) -> Self {
        Self {
            src,
            path,
            config,
            generated,
            syntax: HashMap::new(),
            types: HashMap::new(),
            bindings: HashMap::new(),
            patterns: HashMap::new(),
            assignments: HashMap::new(),
            structs: HashMap::new(),
            fields: HashMap::new(),
            methods: HashMap::new(),
        }
    }

    fn wrapper_type_matches(&self, wrapper: &FollowedWrapper, ty: &str, at: Node<'a>) -> bool {
        if ty.contains("::") {
            return wrapper.mapping.receiver_type == ty;
        }
        if wrapper.mapping.receiver_type.rsplit("::").next() != Some(ty) {
            return false;
        }
        // A visible local nominal type must use its own impl, even when an
        // unrelated module/repository file defines a wrapper with the same name.
        if self
            .types
            .get(ty)
            .into_iter()
            .flatten()
            .any(|binding| binding.target.is_none() && self.visible(binding.node, at))
        {
            return wrapper.declaration.path == self.path
                && self
                    .methods
                    .get(&wrapper.mapping.method)
                    .into_iter()
                    .flatten()
                    .any(|method| {
                        method.start_byte() == wrapper.declaration.start_byte
                            && self
                                .enclosing_impl(*method)
                                .is_some_and(|implementation| self.visible(implementation, at))
                    });
        }
        true
    }

    fn index(&mut self, node: Node<'a>, context: SyntaxContext<'a>) {
        self.syntax.insert(node.id(), context);
        match node.kind() {
            "struct_item" | "enum_item" | "union_item" | "type_item" => {
                if let Some(name) = self.field(node, "name") {
                    let target = (node.kind() == "type_item")
                        .then(|| self.field(node, "type").map(|ty| self.text(ty).to_string()))
                        .flatten();
                    self.types
                        .entry(self.text(name).to_string())
                        .or_default()
                        .push(TypeBinding { node, target });
                    if node.kind() == "struct_item" {
                        self.structs
                            .entry(self.text(name).to_string())
                            .or_default()
                            .push(node);
                    }
                }
            }
            "use_declaration" => {
                if let Some(argument) = self.field(node, "argument") {
                    let mut imports = Vec::new();
                    self.imports(argument, "", &mut imports);
                    for (name, target) in imports {
                        self.types.entry(name).or_default().push(TypeBinding {
                            node,
                            target: Some(target),
                        });
                    }
                }
            }
            "let_declaration" | "parameter" | "for_expression" | "match_arm" | "let_condition" => {
                if let Some(pattern) = self.field(node, "pattern") {
                    let mut names = Vec::new();
                    self.pattern_names(pattern, &mut names);
                    names.sort_unstable();
                    names.dedup();
                    for name in names {
                        if matches!(node.kind(), "let_declaration" | "parameter") {
                            self.bindings
                                .entry(context.function.map(|f| f.id()))
                                .or_default()
                                .entry(name.to_string())
                                .or_default()
                                .push(node);
                        } else {
                            self.patterns
                                .entry(name.to_string())
                                .or_default()
                                .push(node);
                        }
                    }
                }
            }
            "assignment_expression" => {
                if let Some(left) = self.field(node, "left") {
                    self.assignments
                        .entry(self.text(left).to_string())
                        .or_default()
                        .push(node);
                }
            }
            "field_declaration" | "function_item" => {
                if let Some(name) = self.field(node, "name") {
                    let name = self.text(name).to_string();
                    if node.kind() == "field_declaration" {
                        self.fields.entry(name).or_default().push(node);
                    } else {
                        self.methods.entry(name).or_default().push(node);
                    }
                }
            }
            _ => {}
        }
        let mut children = context;
        children.parent = Some(node);
        match node.kind() {
            "block" | "source_file" | "declaration_list" => children.scope = Some(node),
            "function_item" => children.function = Some(node),
            "impl_item" => children.implementation = Some(node),
            "closure_expression" => children.in_closure = true,
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.index(child, children);
        }
    }

    fn pattern_names(&self, pattern: Node<'a>, names: &mut Vec<&'a str>) {
        if matches!(pattern.kind(), "identifier" | "shorthand_field_identifier") {
            names.push(self.text(pattern));
        }
        let mut cursor = pattern.walk();
        for child in pattern.named_children(&mut cursor) {
            self.pattern_names(child, names);
        }
    }

    fn parent(&self, node: Node<'a>) -> Option<Node<'a>> {
        self.syntax.get(&node.id())?.parent
    }

    fn enclosing_function(&self, node: Node<'a>) -> Option<Node<'a>> {
        self.syntax.get(&node.id())?.function
    }

    fn text(&self, n: Node<'_>) -> &'a str {
        &self.src[n.byte_range()]
    }
    fn site(&self, n: Node<'_>) -> RustGrpcSite {
        RustGrpcSite {
            path: self.path.into(),
            start_byte: n.start_byte(),
            end_byte: n.end_byte(),
            line: n.start_position().row + 1,
            column: n.start_position().column + 1,
        }
    }
    fn evidence(&self, n: Node<'_>, description: String) -> RustGrpcEvidence {
        RustGrpcEvidence {
            description,
            site: self.site(n),
        }
    }
    fn field(&self, n: Node<'a>, field: &str) -> Option<Node<'a>> {
        n.child_by_field_name(field)
    }
    fn visible(&self, decl: Node<'a>, at: Node<'a>) -> bool {
        if matches!(decl.kind(), "parameter" | "let_declaration") {
            if self.enclosing_function(decl).map(|n| n.id())
                != self.enclosing_function(at).map(|n| n.id())
            {
                return false;
            }
        }
        self.syntax
            .get(&decl.id())
            .and_then(|context| context.scope)
            .is_some_and(|s| s.start_byte() <= at.start_byte() && s.end_byte() >= at.end_byte())
    }
    fn enclosing_impl(&self, at: Node<'a>) -> Option<Node<'a>> {
        self.syntax.get(&at.id())?.implementation
    }
    fn impl_type(&self, at: Node<'a>) -> Option<String> {
        Some(
            self.text(self.field(self.enclosing_impl(at)?, "type")?)
                .into(),
        )
    }
    // Expand only visible explicit imports and local aliases. No glob guessing,
    // filesystem traversal, wrapper inference, or unbounded alias recursion.
    fn resolve_type(&self, raw: &str, at: Node<'a>, depth: usize) -> Option<Resolved> {
        if depth > 8 {
            return None;
        }
        let raw = raw
            .trim()
            .trim_start_matches('&')
            .trim_start_matches("mut ")
            .trim();
        let raw = raw.split('<').next()?.trim();
        let first = raw.split("::").next()?;
        // Generic type parameters take precedence over imports. Their bounds
        // or defaults do not establish a concrete generated-client identity.
        let mut scope = Some(at);
        while let Some(node) = scope {
            if let Some(parameters) = self.field(node, "type_parameters") {
                let mut cursor = parameters.walk();
                if parameters.named_children(&mut cursor).any(|parameter| {
                    parameter.kind() == "type_parameter"
                        && self
                            .field(parameter, "name")
                            .is_some_and(|name| self.text(name) == first)
                }) {
                    return None;
                }
            }
            scope = self.parent(node);
        }
        let mut bindings = Vec::new();
        for binding in self.types.get(first).into_iter().flatten() {
            if self.visible(binding.node, at) {
                bindings.push((
                    binding.target.as_deref().unwrap_or(raw).to_string(),
                    binding.node,
                ));
            }
        }
        if bindings.len() > 1 {
            return None;
        }
        if let Some((target, node)) = bindings.pop() {
            let expanded = format!("{}{}", target, &raw[first.len()..]);
            if expanded == raw {
                return Some((raw.into(), vec![]));
            }
            let (ty, mut evidence) = self.resolve_type(&expanded, node, depth + 1)?;
            evidence
                .push(self.evidence(node, format!("explicit type/import: {raw} -> {expanded}")));
            Some((ty, evidence))
        } else {
            Some((raw.into(), vec![]))
        }
    }
    fn imports(&self, n: Node<'a>, prefix: &str, out: &mut Vec<(String, String)>) {
        match n.kind() {
            "scoped_use_list" => {
                let path = self.field(n, "path").map(|p| self.text(p)).unwrap_or("");
                if let Some(list) = self.field(n, "list") {
                    self.imports(list, &format!("{prefix}{path}::"), out);
                }
            }
            "use_list" => {
                let mut c = n.walk();
                for child in n.named_children(&mut c) {
                    self.imports(child, prefix, out);
                }
            }
            "use_as_clause" => {
                if let (Some(path), Some(alias)) = (self.field(n, "path"), self.field(n, "alias")) {
                    out.push((
                        self.text(alias).into(),
                        format!("{prefix}{}", self.text(path)),
                    ));
                }
            }
            "identifier" | "scoped_identifier" => {
                let text = self.text(n);
                out.push((
                    text.rsplit("::").next().unwrap_or(text).into(),
                    format!("{prefix}{text}"),
                ));
            }
            _ => {}
        }
    }
    fn typed(&self, ty: Node<'a>, _at: Node<'a>, depth: usize) -> Option<Resolved> {
        // References may include lifetimes and mutability. Follow the syntax
        // node instead of stripping text, so &'a mut Alias resolves as Alias.
        let mut inner = ty;
        while inner.kind() == "reference_type" {
            inner = self.field(inner, "type")?;
        }
        let (resolved, mut evidence) = self.resolve_type(self.text(inner), inner, depth + 1)?;
        evidence.push(self.evidence(ty, format!("explicit type: {}", self.text(ty))));
        Some((resolved, evidence))
    }
    fn pattern_binding_visible(&self, binding: Node<'a>, at: Node<'a>) -> bool {
        let contains =
            |n: Node<'a>| n.start_byte() <= at.start_byte() && n.end_byte() >= at.end_byte();
        match binding.kind() {
            "for_expression" => self.field(binding, "body").is_some_and(contains),
            "match_arm" => contains(binding),
            "let_condition" => {
                let mut parent = self.parent(binding);
                while let Some(n) = parent {
                    match n.kind() {
                        "if_expression" => {
                            return self.field(n, "consequence").is_some_and(contains)
                                || self.field(n, "condition").is_some_and(contains);
                        }
                        "while_expression" => {
                            return self.field(n, "body").is_some_and(contains)
                                || self.field(n, "condition").is_some_and(contains);
                        }
                        _ => parent = self.parent(n),
                    }
                }
                false
            }
            _ => false,
        }
    }
    fn constructor_type(&self, expr: Node<'a>, at: Node<'a>, depth: usize) -> Option<Resolved> {
        let mut call = expr;
        if call.kind() == "try_expression" {
            call = call.named_child(0)?;
        }
        let awaited = call.kind() == "await_expression";
        if awaited {
            call = call.named_child(0)?;
        }
        if call.kind() != "call_expression" {
            return None;
        }
        let function = self.field(call, "function")?;
        if function.kind() != "scoped_identifier" {
            return None;
        }
        let method = self.text(self.field(function, "name")?);
        let constructors = if awaited {
            &self.config.async_constructors
        } else {
            &self.config.constructors
        };
        if !constructors.iter().any(|x| x == method) {
            return None;
        }
        let (ty, mut evidence) = self.typed(self.field(function, "path")?, at, depth)?;
        if !self.generated.is_match(&ty) {
            return None;
        }
        evidence.push(self.evidence(
            expr,
            format!("configured generated-client constructor: {method}, awaited={awaited}"),
        ));
        Some((ty, evidence))
    }
    fn expression_type(&self, expr: Node<'a>, at: Node<'a>, depth: usize) -> Option<Resolved> {
        if depth > 8 {
            return None;
        }
        // Closure capture/argument tracing is a later phase. In particular, a
        // closure parameter must not inherit an outer binding's identity.
        if self
            .syntax
            .get(&expr.id())
            .is_some_and(|context| context.in_closure)
        {
            return None;
        }
        match expr.kind() {
            "try_expression" | "await_expression" => self.constructor_type(expr, at, depth),
            "identifier" => {
                let name = self.text(expr);
                if self.patterns.get(name).into_iter().flatten().any(|&n| {
                    n.start_byte() < at.start_byte()
                        && self.visible(n, at)
                        && self.pattern_binding_visible(n, at)
                }) {
                    return None;
                }
                let function = self.enclosing_function(at).map(|n| n.id());
                let bindings: Vec<_> = self
                    .bindings
                    .get(&function)
                    .and_then(|bindings| bindings.get(name))
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&n| n.start_byte() < at.start_byte() && self.visible(n, at))
                    .collect();
                // Multiple bindings (including shadowing) remain unproven.
                if bindings.len() != 1 {
                    return None;
                }
                let n = bindings[0];
                if self.text(self.field(n, "pattern")?) != name {
                    return None;
                }
                // Inferred locals lose their evidence after reassignment. Even
                // a conditional assignment makes the identity uncertain here.
                if self.field(n, "type").is_none()
                    && self
                        .assignments
                        .get(name)
                        .into_iter()
                        .flatten()
                        .any(|a| a.start_byte() > n.end_byte() && a.start_byte() < at.start_byte())
                {
                    return None;
                }
                if let Some(ty) = self.field(n, "type") {
                    self.typed(ty, at, depth)
                } else {
                    self.expression_type(self.field(n, "value")?, n, depth + 1)
                }
            }
            "self" => self.resolve_type(&self.impl_type(at)?, at, depth + 1),
            "field_expression" => {
                let owner = self.field(expr, "value")?;
                let (owner_type, mut evidence) = self.expression_type(owner, at, depth + 1)?;
                let name = self.text(self.field(expr, "field")?);
                let structs: Vec<_> = self
                    .structs
                    .get(&owner_type)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|n| self.visible(*n, at))
                    .collect();
                if structs.len() != 1 {
                    return None;
                }
                let fields: Vec<_> = self
                    .fields
                    .get(name)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|n| {
                        n.start_byte() > structs[0].start_byte()
                            && n.end_byte() < structs[0].end_byte()
                    })
                    .collect();
                if fields.len() != 1 {
                    return None;
                }
                let (ty, proof) = self.typed(self.field(fields[0], "type")?, at, depth)?;
                evidence.extend(proof);
                Some((ty, evidence))
            }
            "call_expression" => {
                let function = self.field(expr, "function")?;
                if function.kind() == "scoped_identifier" {
                    return self.constructor_type(expr, at, depth);
                }
                if function.kind() != "field_expression" {
                    return None;
                }
                let method = self.text(self.field(function, "field")?);
                let (owner, mut evidence) =
                    self.expression_type(self.field(function, "value")?, at, depth + 1)?;
                let accessors: Vec<_> = self
                    .config
                    .accessors
                    .iter()
                    .filter(|a| a.receiver_type == owner && a.method == method)
                    .collect();
                let methods: Vec<_> = self
                    .methods
                    .get(method)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|n| {
                        self.impl_type(*n).as_deref() == Some(owner.as_str())
                            && self.enclosing_impl(*n).is_some_and(|i| self.visible(i, at))
                    })
                    .collect();
                // `Clone::clone` returns its receiver's type. Tonic clients are
                // cloned per call (`self.client.clone().rpc(..)`); without this
                // the receiver's identity is lost. A type's own `clone` method
                // (counted above) still takes precedence.
                if accessors.is_empty() && methods.is_empty() && method == "clone" {
                    evidence.push(
                        self.evidence(expr, "Clone::clone preserves the receiver type".to_string()),
                    );
                    return Some((owner, evidence));
                }
                if accessors.len() + methods.len() != 1 {
                    return None;
                }
                let (ty, proof) = if let Some(a) = accessors.first() {
                    let (ty, mut proof) = self.resolve_type(&a.return_type, at, depth + 1)?;
                    proof.push(self.evidence(
                        expr,
                        format!(
                            "configured accessor {}::{}: {}",
                            owner, method, a.provenance
                        ),
                    ));
                    (ty, proof)
                } else {
                    self.typed(self.field(methods[0], "return_type")?, at, depth)?
                };
                evidence.extend(proof);
                Some((ty, evidence))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod review_cache_regressions {
    use super::*;
    #[test]
    fn repository_analysis_parses_once_and_invalidates_source_and_config() {
        let source =
            "fn once_review(c: api::orders_client::OrdersClient<Channel>) { c.review_rpc(); }";
        let mut sources = vec![("src/once_review.rs".into(), source.to_string())];
        let before = PARSE_RUNS.with(|n| n.get());
        let mut config = RustGrpcConfig::from_sources(None, None).unwrap();
        follow_rust_grpc_wrappers(&mut config, &sources).unwrap();
        assert_eq!(PARSE_RUNS.with(|n| n.get()), before + 1);
        assert_eq!(
            detect_rust_grpc_candidates(source, &sources[0].0, &config)
                .unwrap()
                .len(),
            1
        );
        let _ = super::super::semantic::probes(source, &sources[0].0, &config);
        let mut next_pass = RustGrpcConfig::from_sources(None, None).unwrap();
        follow_rust_grpc_wrappers(&mut next_pass, &sources).unwrap();
        assert_eq!(PARSE_RUNS.with(|n| n.get()), before + 1);
        sources[0].1.push_str("\nfn edited_review() {}");
        follow_rust_grpc_wrappers(&mut next_pass, &sources).unwrap();
        assert_eq!(PARSE_RUNS.with(|n| n.get()), before + 2);
        next_pass.deny_methods.push("review_rpc".into());
        follow_rust_grpc_wrappers(&mut next_pass, &sources).unwrap();
        assert_eq!(PARSE_RUNS.with(|n| n.get()), before + 3);
        assert!(
            detect_rust_grpc_candidates(&sources[0].1, &sources[0].0, &next_pass)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn candidate_cache_reuses_content_and_invalidates_edits() {
        let src = "fn review_cache_regression(c: api::orders_client::OrdersClient<Channel>) { c.fetch_review(); }";
        let mut config = RustGrpcConfig::from_sources(None, None).unwrap();
        follow_rust_grpc_wrappers(&mut config, &[("src/review_cache.rs".into(), src.into())])
            .unwrap();
        let first = detect_rust_grpc_candidates(src, "src/review_cache.rs", &config).unwrap();
        assert_eq!(first.len(), 1);
        let before = DETECTION_RUNS.with(|n| n.get());
        assert_eq!(
            detect_rust_grpc_candidates(src, "src/review_cache.rs", &config).unwrap(),
            first
        );
        assert_eq!(DETECTION_RUNS.with(|n| n.get()), before);
        let edited = src.replace("fetch_review", "updated_review");
        assert_eq!(
            detect_rust_grpc_candidates(&edited, "src/review_cache.rs", &config).unwrap()[0].method,
            "updated_review"
        );
    }
}

#[cfg(test)]
mod followup_review {
    use super::*;

    #[test]
    fn prepare_repository_compiles_one_query_for_all_files() {
        let mut config = RustGrpcConfig::from_sources(None, None).unwrap();
        let sources: Vec<_> = (0..4)
            .map(|i| {
                (
                    format!("src/review_query_reuse_{i}.rs"),
                    format!("// query reuse review fixture {i}"),
                )
            })
            .collect();
        let before = CANDIDATE_QUERY_COMPILES.with(|count| count.get());
        follow_rust_grpc_wrappers(&mut config, &sources).unwrap();
        assert_eq!(
            CANDIDATE_QUERY_COMPILES.with(|count| count.get()) - before,
            1,
            "candidate discovery and semantic probes must share the repository query"
        );
    }

    #[test]
    fn edited_wrapper_body_does_not_reuse_prepared_delegation() {
        let path = "src/edited_wrapper.rs";
        let source = "struct Cached { inner: api::orders_client::OrdersClient<Channel> } impl Cached { fn draft(&mut self) { self.inner.get(req); } } fn send(c: Cached) { c.draft(); }";
        let mut config = RustGrpcConfig::from_sources(None, None).unwrap();
        follow_rust_grpc_wrappers(&mut config, &[(path.into(), source.into())]).unwrap();
        let edited = source.replace("inner.get(req)", "inner.list(req)");
        let candidates = detect_rust_grpc_candidates(&edited, path, &config).unwrap();
        let call = candidates
            .iter()
            .find(|c| c.enclosing_name.as_deref() == Some("send"))
            .unwrap();
        assert_eq!(
            call.method, "list",
            "an edited wrapper cannot keep the old RPC identity"
        );
        assert!(super::super::semantic::probes(&edited, path, &config)
            .iter()
            .all(|probe| probe.site.start_byte != call.site.start_byte));
    }

    #[test]
    fn edited_file_cannot_reenable_inference_without_other_files_method_counts() {
        let path = "src/edited_inference.rs";
        let source = "struct Cached { inner: api::orders_client::OrdersClient<Channel> } impl Cached { fn draft(&mut self) { self.inner.get(req); } } fn send() { cached.draft(); }";
        let mut config = RustGrpcConfig::from_sources(None, None).unwrap();
        follow_rust_grpc_wrappers(
            &mut config,
            &[
                (path.into(), source.into()),
                (
                    "src/other.rs".into(),
                    "struct Other; impl Other { fn draft(&mut self) {} }".into(),
                ),
            ],
        )
        .unwrap();
        let edited = source.replace("inner.get(req)", "inner.list(req)");
        let candidates = detect_rust_grpc_candidates(&edited, path, &config).unwrap();
        assert!(
            candidates
                .iter()
                .all(|c| c.enclosing_name.as_deref() != Some("send")),
            "local reanalysis must not forget competing methods from other files"
        );
        assert!(super::super::semantic::probes(&edited, path, &config)
            .iter()
            .any(|probe| probe.enclosing_name.as_deref() == Some("send")));
    }

    #[test]
    fn file_paths_count_toward_repository_cache_limit() {
        let sources: Vec<_> = (0..1600)
            .map(|i| {
                (
                    format!("src/{}/file_{i}.rs", "p".repeat(3000)),
                    String::new(),
                )
            })
            .collect();
        let mut config = RustGrpcConfig::from_sources(None, None).unwrap();
        follow_rust_grpc_wrappers(&mut config, &sources).unwrap();
        let prepared = config.prepared.as_ref().unwrap();
        assert!(!repository_cache().lock().iter().any(|(_, cached)| Arc::ptr_eq(cached, prepared)),
            "over four MiB of file paths must not bypass the cache limit just because there are no candidates");
    }

    fn candidate_types(src: &str) -> Vec<(String, String)> {
        let mut config = RustGrpcConfig::from_sources(None, None).unwrap();
        follow_rust_grpc_wrappers(
            &mut config,
            &[("crates/c/src/grpc_client.rs".into(), src.into())],
        )
        .unwrap();
        let out = detect_rust_grpc_candidates(src, "crates/c/src/grpc_client.rs", &config)
            .unwrap()
            .into_iter()
            .map(|c| (c.method, c.generated_type))
            .collect();
        // The repository cache is process-wide with eight slots; a test that
        // only needs the answer must not evict entries other tests rely on.
        if let Some(prepared) = config.prepared.as_ref() {
            let mut cache = repository_cache().lock();
            let keys: Vec<_> = cache
                .iter()
                .filter(|(_, v)| Arc::ptr_eq(v, prepared))
                .map(|(k, _)| *k)
                .collect();
            for key in keys {
                cache.pop(&key);
            }
        }
        out
    }

    #[test]
    fn cloned_field_receiver_keeps_the_generated_client_type() {
        let src = "use crate::proto::open_shell_client::OpenShellClient; pub struct W { client: OpenShellClient<Chan> } impl W { pub async fn get_draft_policy(&self) { let r = self.client.clone().get_draft_policy(req).await; } }";
        assert_eq!(candidate_types(src).len(), 1, "{:?}", candidate_types(src));
    }

    #[test]
    fn cloned_field_receiver_with_a_nested_import_keeps_the_type() {
        let src = "use crate::proto::{Foo, open_shell_client::OpenShellClient}; pub struct W { client: OpenShellClient<Chan> } impl W { pub async fn get_draft_policy(&self) { let r = self.client.clone().get_draft_policy(req).await; } }";
        assert_eq!(candidate_types(src).len(), 1, "{:?}", candidate_types(src));
    }

    #[test]
    fn field_receiver_with_a_nested_import_keeps_the_type() {
        let src = "use crate::proto::{Foo, open_shell_client::OpenShellClient}; pub struct W { client: OpenShellClient<Chan> } impl W { pub async fn get_draft_policy(&mut self) { let r = self.client.get_draft_policy(req).await; } }";
        assert_eq!(candidate_types(src).len(), 1, "{:?}", candidate_types(src));
    }

    #[test]
    fn cfg_attribute_on_a_pattern_field_does_not_void_the_file() {
        let src = "use crate::proto::open_shell_client::OpenShellClient; struct T { a: u8, b: u8 } pub struct W { client: OpenShellClient<Chan> } impl W { pub async fn draft(&self, t: T) { let T { #[cfg(feature = \"jwt\")] a, b } = t; let r = self.client.clone().get_draft_policy(req).await; } }";
        assert!(
            parse_for_lang(Lang::Rust, src)
                .unwrap()
                .root_node()
                .has_error(),
            "fixture must trip the grammar"
        );
        let types = candidate_types(src);
        assert_eq!(types.len(), 1, "{types:?}");
        assert_eq!(types[0].0, "get_draft_policy");
    }

    #[test]
    fn blank_attributes_keeps_offsets_and_newlines() {
        let src = "#[cfg(a)]\nfn f() {}\n#![allow(x)]\n";
        let out = blank_attributes(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(out.matches('\n').count(), 3);
        assert!(out.contains("fn f() {}"));
        assert!(!out.contains('#'));
    }

    const WRAPPER_FILE: &str = "crates/openshell-core/src/grpc_client.rs";
    const WRAPPER_SRC: &str = "use crate::proto::{Foo, open_shell_client::OpenShellClient}; pub struct CachedOpenShellClient { client: OpenShellClient<Chan> } impl CachedOpenShellClient { pub async fn connect(g: &str) -> Self { todo!() } pub fn workspace(&self) -> &str { \"w\" } pub async fn poll_settings(&self, name: &str) { let scope = self.workspace(); let r = self.client.clone().get_sandbox_config(req(scope, name)).await; } }";
    const CALLER_FILE: &str = "crates/openshell-supervisor/src/lib.rs";
    const CALLER_SRC: &str = "use openshell_core::grpc_client::CachedOpenShellClient; async fn flush(g: &str) -> Result<(), E> { let client = CachedOpenShellClient::connect(g).await?; client.poll_settings(\"sb\").await; let w = client.workspace(); Ok(()) }";

    fn two_file_candidates() -> Vec<RustGrpcCandidate> {
        let mut config = RustGrpcConfig::from_sources(None, None).unwrap();
        let sources = vec![
            (WRAPPER_FILE.to_string(), WRAPPER_SRC.to_string()),
            (CALLER_FILE.to_string(), CALLER_SRC.to_string()),
        ];
        follow_rust_grpc_wrappers(&mut config, &sources).unwrap();
        let out = detect_rust_grpc_candidates(CALLER_SRC, CALLER_FILE, &config).unwrap();
        if let Some(prepared) = config.prepared.as_ref() {
            let mut cache = repository_cache().lock();
            let keys: Vec<_> = cache
                .iter()
                .filter(|(_, v)| Arc::ptr_eq(v, prepared))
                .map(|(k, _)| *k)
                .collect();
            for key in keys {
                cache.pop(&key);
            }
        }
        out
    }

    #[test]
    fn wrapper_with_a_self_helper_call_still_maps_to_its_rpc() {
        let calls = two_file_candidates();
        let poll = calls
            .iter()
            .find(|c| c.site.line == 1 && c.receiver == "client" && c.method != "workspace");
        let poll = poll.unwrap_or_else(|| panic!("poll_settings not mapped: {calls:?}"));
        assert_eq!(poll.method, "get_sandbox_config");
        assert_eq!(poll.provenance, RustGrpcProvenance::FollowedWrapper);
        assert!(
            poll.generated_type
                .ends_with("open_shell_client::OpenShellClient"),
            "{}",
            poll.generated_type
        );
    }

    #[test]
    fn unmapped_call_on_a_local_wrapper_type_is_not_an_rpc_stub() {
        let calls = two_file_candidates();
        assert!(
            calls.iter().all(|c| c.method != "workspace"),
            "a helper on the wrapper type is ordinary code: {calls:?}"
        );
    }
}
