//! Bounded, snapshot-only Rust generated-client candidate discovery.
use crate::error::LainError;
use crate::server::sensors::util::{language_for, parse_for_lang, scan_files, Lang};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

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

/// A generated-client receiver with a method call, not a proven protobuf RPC.
/// No destination, normalized method, contract fact, or caller edge is inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RustGrpcCandidate {
    pub method: String,
    pub receiver: String,
    pub generated_type: String,
    pub site: RustGrpcSite,
    pub evidence: Vec<RustGrpcEvidence>,
    pub enclosing_name: Option<String>,
    pub enclosing_start_line: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
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
    #[serde(skip)]
    pub(super) query: String,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accessor {
    pub receiver_type: String,
    pub method: String,
    pub return_type: String,
    pub provenance: String,
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
        let query = Query::new(&language_for(Lang::Rust), &self.query)
            .map_err(|e| LainError::Other(format!("Rust gRPC query: {e}")))?;
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
    let config = RustGrpcConfig::load(root)?;
    let compiled = config.compile()?;
    let mut out = Vec::new();
    for (path, content, _) in scan_files(root, |p| {
        (p.extension()?.to_str()? == "rs").then(|| "rs".to_string())
    }) {
        out.extend(detect_with_compiled_config(
            &content,
            &crate::graph::graph_path(root, &path),
            &config,
            &compiled,
        )?);
    }
    out.sort_by(|a, b| (&a.site.path, a.site.start_byte).cmp(&(&b.site.path, b.site.start_byte)));
    Ok(out)
}

pub fn detect_rust_grpc_candidates(
    src: &str,
    path: &str,
    config: &RustGrpcConfig,
) -> Result<Vec<RustGrpcCandidate>, LainError> {
    detect_with_compiled_config(src, path, config, &config.compile()?)
}

fn detect_with_compiled_config(
    src: &str,
    path: &str,
    config: &RustGrpcConfig,
    compiled: &CompiledConfig,
) -> Result<Vec<RustGrpcCandidate>, LainError> {
    let Some(tree) = parse_for_lang(Lang::Rust, src) else {
        return Ok(Vec::new());
    };
    // Damaged syntax cannot supply reliable scope/type evidence.
    if tree.root_node().has_error() {
        return Ok(Vec::new());
    }
    let mut ctx = Context {
        src,
        path,
        config,
        generated: &compiled.generated,
        syntax: HashMap::new(),
        types: HashMap::new(),
        bindings: HashMap::new(),
        patterns: HashMap::new(),
        assignments: HashMap::new(),
        structs: HashMap::new(),
        fields: HashMap::new(),
        methods: HashMap::new(),
    };
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
        if let Some((ty, evidence)) = ctx.expression_type(receiver, call, 0) {
            if ctx.generated.is_match(&ty) {
                out.push(RustGrpcCandidate {
                    method: ctx.text(method).into(),
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
    }
    out.sort_by_key(|c| c.site.start_byte);
    out.dedup_by_key(|c| c.site.start_byte);
    Ok(out)
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
