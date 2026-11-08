//! Bounded, source-only evaluation of JavaScript URL expressions. Never execute
//! code or read the process environment. Imports stay inside the scanned repo.
use super::*;
use std::collections::BTreeSet;
use std::path::PathBuf;

const DEPTH: usize = 32;
const MODULE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub(super) enum Value {
    Parts(Vec<UrlPart>),
    Object(BTreeMap<String, Value>),
    Callable(Box<Value>),
}

impl Value {
    fn parts(self, fallback: &str) -> Vec<UrlPart> {
        match self {
            Self::Parts(p) if !p.is_empty() => p,
            _ => vec![UrlPart::Hole(fallback.to_owned())],
        }
    }
}

fn unknown(raw: &str) -> Value {
    Value::Parts(vec![UrlPart::Hole(raw.to_owned())])
}

pub(super) fn parts(node: Node, src: &[u8], ctx: &FileContext, scopes: &[u32]) -> Vec<UrlPart> {
    let value = evaluate(node, src, ctx, scopes);
    let value = match value {
        Value::Object(mut fields) => fields
            .remove("url")
            .unwrap_or_else(|| unknown(&text_of(node, src).unwrap_or_default())),
        v => v,
    };
    value.parts(&text_of(node, src).unwrap_or_default())
}

fn expression(
    raw: &str,
    ctx: &FileContext,
    scopes: &[u32],
    depth: usize,
    fuel: &std::cell::Cell<usize>,
) -> Value {
    if depth >= DEPTH || raw.len() > MODULE_BYTES as usize {
        return unknown(raw);
    }
    let source = format!("const __lain_url = ({raw});");
    let Some(tree) = parse_for_lang(Lang::TsJs, &source) else {
        return unknown(raw);
    };
    if tree.root_node().has_error() {
        return unknown(raw);
    }
    let value = tree
        .root_node()
        .named_child(0)
        .and_then(|n| n.named_child(0))
        .and_then(|n| n.child_by_field_name("value"));
    match value {
        Some(n) => eval(n, source.as_bytes(), ctx, scopes, depth + 1, fuel),
        None => unknown(raw),
    }
}

fn evaluate(node: Node, src: &[u8], ctx: &FileContext, scopes: &[u32]) -> Value {
    let fuel = std::cell::Cell::new(256);
    let value = eval(node, src, ctx, scopes, 0, &fuel);
    if fuel.get() == 0 {
        unknown(&text_of(node, src).unwrap_or_default())
    } else {
        value
    }
}

fn eval(
    node: Node,
    src: &[u8],
    ctx: &FileContext,
    scopes: &[u32],
    depth: usize,
    fuel: &std::cell::Cell<usize>,
) -> Value {
    let raw = text_of(node, src).unwrap_or_default();
    if depth >= DEPTH || fuel.get() == 0 {
        return unknown(&raw);
    }
    fuel.set(fuel.get() - 1);
    let child = |n| eval(n, src, ctx, scopes, depth + 1, fuel);
    match node.kind() {
        "parenthesized_expression" => node.named_child(0).map_or_else(|| unknown(&raw), child),
        "string" => {
            // Escapes require language decoding; never quietly strip them.
            if raw.contains('\\') {
                return unknown(&raw);
            }
            Value::Parts(vec![UrlPart::Literal(
                strip_js_string_quotes(&raw).to_owned(),
            )])
        }
        "identifier" | "shorthand_property_identifier" => {
            let local_binding = scopes.iter().enumerate().find_map(|(i, scope)| {
                let key = (*scope, raw.clone());
                ctx.fn_assign_counts.get(&key).map(|count| (i, key, *count))
            });
            if let Some((i, key, count)) = local_binding {
                if count != 1 {
                    return unknown(&raw);
                }
                match ctx.fn_assignments.get(&key) {
                    Some(value) => preserve_unresolved_name(
                        expression(value, ctx, &scopes[i..], depth + 1, fuel),
                        &raw,
                    ),
                    None => unknown(&raw),
                }
            } else if ctx.module_assign_counts.get(&raw) == Some(&1) {
                match ctx.module_assignments.get(&raw) {
                    Some(value) => {
                        preserve_unresolved_name(expression(value, ctx, &[], depth + 1, fuel), &raw)
                    }
                    None => unknown(&raw),
                }
            } else if scopes
                .iter()
                .any(|s| ctx.fn_assign_counts.contains_key(&(*s, raw.clone())))
                || ctx.module_assign_counts.contains_key(&raw)
            {
                unknown(&raw)
            } else {
                ctx.imported
                    .get(&raw)
                    .cloned()
                    .unwrap_or_else(|| unknown(&raw))
            }
        }
        "template_string" => {
            let mut out = Vec::new();
            let mut cursor = node.walk();
            for n in node.named_children(&mut cursor) {
                match n.kind() {
                    "string_fragment" => {
                        out.push(UrlPart::Literal(text_of(n, src).unwrap_or_default()))
                    }
                    "template_substitution" => {
                        if let Some(n) = n.named_child(0) {
                            out.extend(child(n).parts(&text_of(n, src).unwrap_or_default()));
                        }
                    }
                    // Escapes cannot disappear from an apparent route.
                    _ => return unknown(&raw),
                }
            }
            if out.is_empty() {
                out.push(UrlPart::Literal(String::new()));
            }
            if let [UrlPart::Hole(protocol), UrlPart::Literal(separator), UrlPart::Hole(host)] =
                out.as_slice()
            {
                if separator == "//"
                    && protocol
                        .strip_suffix(".protocol")
                        .is_some_and(|base| host.strip_suffix(".host") == Some(base))
                {
                    return unknown(&raw);
                }
            }
            Value::Parts(out)
        }
        "binary_expression" => {
            let operator = node
                .child_by_field_name("operator")
                .and_then(|n| text_of(n, src));
            if matches!(operator.as_deref(), Some("||" | "??")) {
                return unknown(&format!("conditional: {raw}"));
            }
            if operator.as_deref() != Some("+") {
                return unknown(&raw);
            }
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                return unknown(&raw);
            };
            let mut out = child(left).parts(&raw);
            out.extend(child(right).parts(&raw));
            Value::Parts(out)
        }
        "object" => {
            let mut values = BTreeMap::new();
            let mut cursor = node.walk();
            for n in node.named_children(&mut cursor) {
                let (key, value) = if n.kind() == "shorthand_property_identifier" {
                    (text_of(n, src).unwrap_or_default(), child(n))
                } else if n.kind() == "pair" {
                    let (Some(k), Some(v)) =
                        (n.child_by_field_name("key"), n.child_by_field_name("value"))
                    else {
                        return unknown(&raw);
                    };
                    if !matches!(k.kind(), "property_identifier" | "string") {
                        return unknown(&raw);
                    }
                    (
                        strip_js_string_quotes(&text_of(k, src).unwrap_or_default()).to_owned(),
                        child(v),
                    )
                } else {
                    return unknown(&raw);
                };
                if values.insert(key, value).is_some() {
                    return unknown(&raw);
                }
            }
            Value::Object(values)
        }
        "member_expression" => {
            let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            ) else {
                return unknown(&raw);
            };
            match child(object) {
                Value::Object(mut values) => values
                    .remove(&text_of(property, src).unwrap_or_default())
                    .unwrap_or_else(|| unknown(&raw)),
                _ => unknown(&raw),
            }
        }
        "arrow_function" | "function_expression" => {
            if raw.trim_start().starts_with("async ") {
                return unknown(&raw);
            }
            let params = node.child_by_field_name("parameters");
            if node.child_by_field_name("parameter").is_some()
                || params.is_some_and(|p| p.named_child_count() != 0)
            {
                return unknown(&raw);
            }
            let Some(body) = node.child_by_field_name("body") else {
                return unknown(&raw);
            };
            let result = if body.kind() != "statement_block" {
                child(body)
            } else {
                // Only straight-line const declarations followed by one return.
                // Branches, mutation and side effects make a helper opaque.
                let mut local = ctx.clone();
                let mut returned = None;
                let mut cursor = body.walk();
                for stmt in body.named_children(&mut cursor) {
                    match stmt.kind() {
                        "comment" => {}
                        "lexical_declaration"
                            if returned.is_none()
                                && text_of(stmt, src).is_some_and(|s| s.starts_with("const ")) =>
                        {
                            let mut c = stmt.walk();
                            for decl in stmt.named_children(&mut c) {
                                let Some((name, value)) = read_var_declarator(decl, src) else {
                                    return unknown(&raw);
                                };
                                // An unused initializer can still mutate a
                                // captured object. Do not infer through calls
                                // whose effects this bounded evaluator cannot follow.
                                let mut effectful = false;
                                if let Some(initializer) = decl.child_by_field_name("value") {
                                    walk(initializer, &mut |n| {
                                        effectful |= matches!(
                                            n.kind(),
                                            "call_expression"
                                                | "new_expression"
                                                | "assignment_expression"
                                                | "augmented_assignment_expression"
                                                | "update_expression"
                                                | "await_expression"
                                                | "yield_expression"
                                        );
                                    });
                                }
                                if effectful {
                                    return unknown(&raw);
                                }
                                record_assignment(
                                    &mut local,
                                    &AssignmentScope::Function(u32::MAX),
                                    &name,
                                    &value,
                                );
                            }
                        }
                        "return_statement" if returned.is_none() => returned = stmt.named_child(0),
                        _ => return unknown(&raw),
                    }
                }
                let mut chain = vec![u32::MAX];
                chain.extend_from_slice(scopes);
                returned.map_or_else(
                    || unknown(&raw),
                    |n| eval(n, src, &local, &chain, depth + 1, fuel),
                )
            };
            Value::Callable(Box::new(result))
        }
        "call_expression" | "new_expression" => {
            let args = args_of(node);
            let Some(function) = node
                .child_by_field_name("function")
                .or_else(|| node.child_by_field_name("constructor"))
            else {
                return unknown(&raw);
            };
            let function_text = text_of(function, src).unwrap_or_default();
            let shadowed_constructor = ctx.module_assign_counts.contains_key("URL")
                || ctx.imported.contains_key("URL")
                || scopes
                    .iter()
                    .any(|s| ctx.fn_assign_counts.contains_key(&(*s, "URL".to_owned())));
            if node.kind() == "new_expression"
                && function_text == "URL"
                && args.len() == 2
                && !shadowed_constructor
            {
                let path = child(args[0]).parts(&raw);
                let base = child(args[1]).parts(&raw);
                return Value::Parts(join_url(
                    path,
                    base,
                    &raw,
                    &text_of(args[1], src).unwrap_or_default(),
                ));
            }
            if function_text == "urljoin" && args.len() == 2 {
                return Value::Parts(join_url(
                    child(args[1]).parts(&raw),
                    child(args[0]).parts(&raw),
                    &raw,
                    &text_of(args[0], src).unwrap_or_default(),
                ));
            }
            if args.is_empty() && function.kind() == "member_expression" {
                let property = function
                    .child_by_field_name("property")
                    .and_then(|n| text_of(n, src));
                if property.as_deref() == Some("toString") {
                    if let Some(object) = function.child_by_field_name("object") {
                        // Only URL instances have the URL string conversion contract.
                        if object.kind() == "new_expression" {
                            return child(object);
                        }
                    }
                }
            }
            if args.is_empty() {
                if let Value::Callable(result) = child(function) {
                    return *result;
                }
            }
            unknown(&raw)
        }
        _ => unknown(&raw),
    }
}

/// Preserve the identifier a caller actually used when its assignment only
/// leads to another opaque expression. Host aliases are declared against that
/// identifier, and expanding it to the initializer text makes the alias stop
/// matching even though the host is still unresolved.
fn preserve_unresolved_name(value: Value, name: &str) -> Value {
    match value {
        Value::Parts(parts) if matches!(parts.as_slice(), [UrlPart::Hole(_)]) => unknown(name),
        other => other,
    }
}

fn join_url(path: Vec<UrlPart>, base: Vec<UrlPart>, raw: &str, base_source: &str) -> Vec<UrlPart> {
    if let [UrlPart::Literal(p)] = path.as_slice() {
        if p.contains("://") {
            return path;
        }
        if let [UrlPart::Literal(b)] = base.as_slice() {
            // Use the URL parser for replacement semantics, not concatenation.
            if let Ok(b) = reqwest::Url::parse(b) {
                if let Ok(joined) = b.join(p) {
                    return vec![UrlPart::Literal(joined.to_string())];
                }
            }
        }
        if p.starts_with('/') && !p.starts_with("//") {
            // Unknown origin, known absolute route. Retain the origin expression.
            return vec![
                UrlPart::Hole(base_source.to_owned()),
                UrlPart::Literal(p.clone()),
            ];
        }
    }
    vec![UrlPart::Hole(raw.to_owned())]
}

pub(super) fn imports(root: &Path, path: &Path, tree: Node, src: &[u8]) -> BTreeMap<String, Value> {
    let Ok(root) = root.canonicalize() else {
        return BTreeMap::new();
    };
    let mut visited = BTreeSet::new();
    let mut budget = 24;
    imports_inner(&root, path, tree, src, &mut visited, &mut budget)
}

fn imports_inner(
    root: &Path,
    path: &Path,
    tree: Node,
    src: &[u8],
    visited: &mut BTreeSet<PathBuf>,
    budget: &mut usize,
) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let mut cursor = tree.walk();
    for statement in tree.named_children(&mut cursor) {
        if statement.kind() != "import_statement" {
            continue;
        }
        let Some(source) = statement
            .child_by_field_name("source")
            .and_then(|n| text_of(n, src))
        else {
            continue;
        };
        let spec = strip_js_string_quotes(&source);
        let Some(module) = module_path(root, path, spec) else {
            continue;
        };
        let exports = exports(root, &module, visited, budget);
        walk(statement, &mut |n| {
            if n.kind() == "import_specifier" {
                let Some(name) = n.child_by_field_name("name").and_then(|v| text_of(v, src)) else {
                    return;
                };
                let alias = n
                    .child_by_field_name("alias")
                    .and_then(|v| text_of(v, src))
                    .unwrap_or_else(|| name.clone());
                if let Some(value) = exports.get(&name) {
                    out.entry(alias.clone())
                        .and_modify(|existing| *existing = unknown(&alias))
                        .or_insert_with(|| value.clone());
                }
            }
        });
    }
    let context = FileContext::collect(tree, src, Lang::TsJs);
    let mut namespaces = BTreeMap::new();
    walk(tree, &mut |n| {
        if n.kind() != "variable_declarator"
            || !is_module_level(n)
            || n.child_by_field_name("name")
                .is_some_and(|name| name.kind() != "identifier")
        {
            return;
        }
        let Some(value) = n.child_by_field_name("value") else {
            return;
        };
        if value
            .child_by_field_name("function")
            .and_then(|n| text_of(n, src))
            .as_deref()
            != Some("require")
        {
            return;
        }
        let args = args_of(value);
        if args.len() != 1 || args[0].kind() != "string" {
            return;
        }
        let name = text_of(n.child_by_field_name("name").unwrap(), src).unwrap();
        if context.module_assign_counts.get(&name) != Some(&1) {
            return;
        }
        let source = text_of(args[0], src).unwrap_or_default();
        let Some(module) = module_path(root, path, strip_js_string_quotes(&source)) else {
            return;
        };
        namespaces.insert(name, exports(root, &module, visited, budget));
    });

    // CommonJS destructuring, either directly from require or through a
    // single-assignment namespace alias.
    walk(tree, &mut |n| {
        if n.kind() != "variable_declarator" {
            return;
        }
        let (Some(name), Some(value)) = (
            n.child_by_field_name("name"),
            n.child_by_field_name("value"),
        ) else {
            return;
        };
        if !is_module_level(n) || name.kind() != "object_pattern" {
            return;
        }
        let exports = if value.kind() == "identifier" {
            let namespace = text_of(value, src).unwrap_or_default();
            if context.module_assign_counts.get(&namespace) != Some(&1) {
                return;
            }
            namespaces.get(&namespace).cloned().unwrap_or_default()
        } else {
            if value
                .child_by_field_name("function")
                .and_then(|n| text_of(n, src))
                .as_deref()
                != Some("require")
            {
                return;
            }
            let args = args_of(value);
            if args.len() != 1 || args[0].kind() != "string" {
                return;
            }
            let source = text_of(args[0], src).unwrap_or_default();
            let Some(module) = module_path(root, path, strip_js_string_quotes(&source)) else {
                return;
            };
            exports(root, &module, visited, budget)
        };
        let mut c = name.walk();
        for property in name.named_children(&mut c) {
            let pair = if property.kind() == "shorthand_property_identifier_pattern" {
                text_of(property, src).map(|n| (n.clone(), n))
            } else if property.kind() == "pair_pattern" {
                property
                    .child_by_field_name("key")
                    .and_then(|k| text_of(k, src))
                    .zip(
                        property
                            .child_by_field_name("value")
                            .and_then(|v| text_of(v, src)),
                    )
            } else {
                None
            };
            if let Some((name, alias)) = pair {
                if let Some(value) = exports.get(&name) {
                    out.entry(alias.clone())
                        .and_modify(|existing| *existing = unknown(&alias))
                        .or_insert_with(|| value.clone());
                }
            }
        }
    });
    out
}

fn module_path(root: &Path, from: &Path, spec: &str) -> Option<PathBuf> {
    let base = if spec.starts_with('.') {
        from.parent()?.join(spec)
    } else {
        // Honor declared tsconfig paths; never guess that @ means repo root.
        let config_path = root.join("tsconfig.json").canonicalize().ok()?;
        if !config_path.starts_with(root) || !analysis_input(root, &config_path) {
            return None;
        }
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(config_path).ok()?).ok()?;
        let options = config.get("compilerOptions")?;
        let aliases = options.get("paths")?.as_object()?;
        let mut matches = Vec::new();
        for (pattern, targets) in aliases {
            let substitution = if let Some(prefix) = pattern.strip_suffix('*') {
                spec.strip_prefix(prefix)
            } else if pattern == spec {
                Some("")
            } else {
                None
            };
            if let Some(sub) = substitution {
                let targets = targets.as_array()?;
                if targets.len() != 1 {
                    return None;
                }
                matches.push(
                    root.join(
                        options
                            .get("baseUrl")
                            .and_then(|v| v.as_str())
                            .unwrap_or("."),
                    )
                    .join(targets[0].as_str()?.replace('*', sub)),
                );
            }
        }
        if matches.len() != 1 {
            return None;
        }
        matches.pop()?
    };
    let candidates = if base.extension().is_some() {
        vec![base]
    } else {
        ["ts", "tsx", "js", "jsx", "mjs", "cjs"]
            .into_iter()
            .flat_map(|ext| [base.with_extension(ext), base.join(format!("index.{ext}"))])
            .collect()
    };
    let candidates: BTreeSet<_> = candidates
        .into_iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| p.starts_with(root) && p.is_file() && analysis_input(root, p))
        .collect();
    if candidates.len() != 1 {
        return None;
    }
    candidates.into_iter().next()
}

fn analysis_input(root: &Path, path: &Path) -> bool {
    let Ok(repo) = git2::Repository::open(root) else {
        return true;
    };
    let Some(workdir) = repo.workdir().and_then(|p| p.canonicalize().ok()) else {
        return false;
    };
    let Ok(relative) = path.strip_prefix(&workdir) else {
        return false;
    };
    match repo.is_path_ignored(relative) {
        Ok(false) => true,
        Ok(true) => repo
            .index()
            .is_ok_and(|index| index.get_path(relative, 0).is_some()),
        Err(_) => false,
    }
}

fn exports(
    root: &Path,
    path: &Path,
    visited: &mut BTreeSet<PathBuf>,
    budget: &mut usize,
) -> BTreeMap<String, Value> {
    if *budget == 0 || visited.len() >= 8 || !visited.insert(path.to_owned()) {
        return BTreeMap::new();
    }
    *budget -= 1;
    let result = (|| {
        if std::fs::metadata(path).ok()?.len() > MODULE_BYTES {
            return None;
        }
        let source = std::fs::read_to_string(path).ok()?;
        let tree = parse_for_lang(Lang::TsJs, &source)?;
        let src = source.as_bytes();
        let mut ctx = FileContext::collect(tree.root_node(), src, Lang::TsJs);
        // Exported let/var values can be changed by code outside this module's
        // initialization path. Only immutable declarations provide constants.
        walk(tree.root_node(), &mut |n| {
            if matches!(n.kind(), "lexical_declaration" | "variable_declaration")
                && is_module_level(n)
                && !text_of(n, src).is_some_and(|s| s.starts_with("const "))
            {
                let mut c = n.walk();
                for decl in n.named_children(&mut c) {
                    if let Some((name, _)) = read_var_declarator(decl, src) {
                        ctx.module_assign_counts.insert(name, 2);
                    }
                }
            }
        });
        ctx.imported = imports_inner(root, path, tree.root_node(), src, visited, budget);
        let mut out = BTreeMap::new();
        let mut cursor = tree.root_node().walk();
        for stmt in tree.root_node().named_children(&mut cursor) {
            if stmt.kind() == "export_statement" && stmt.child_by_field_name("source").is_none() {
                if let Some(declaration) = stmt.child_by_field_name("declaration") {
                    if declaration.kind() == "function_declaration" {
                        if let Some(name) = declaration
                            .child_by_field_name("name")
                            .and_then(|n| text_of(n, src))
                        {
                            out.insert(
                                name.clone(),
                                expression(&name, &ctx, &[], 0, &std::cell::Cell::new(256)),
                            );
                        }
                    } else if declaration.kind() == "lexical_declaration" {
                        let mut c = declaration.walk();
                        for decl in declaration.named_children(&mut c) {
                            if let Some((name, _)) = read_var_declarator(decl, src) {
                                out.insert(
                                    name.clone(),
                                    expression(&name, &ctx, &[], 0, &std::cell::Cell::new(256)),
                                );
                            }
                        }
                    }
                }
                walk(stmt, &mut |n| {
                    if n.kind() == "export_specifier" {
                        if let Some(name) =
                            n.child_by_field_name("name").and_then(|v| text_of(v, src))
                        {
                            let alias = n
                                .child_by_field_name("alias")
                                .and_then(|v| text_of(v, src))
                                .unwrap_or_else(|| name.clone());
                            out.insert(
                                alias,
                                expression(&name, &ctx, &[], 0, &std::cell::Cell::new(256)),
                            );
                        }
                    }
                });
            }
            if stmt.kind() == "expression_statement" {
                if let Some(assignment) = stmt.named_child(0) {
                    if assignment.kind() == "assignment_expression"
                        && assignment
                            .child_by_field_name("left")
                            .and_then(|n| text_of(n, src))
                            .as_deref()
                            == Some("module.exports")
                    {
                        if let Some(value) = assignment.child_by_field_name("right") {
                            if let Value::Object(values) = evaluate(value, src, &ctx, &[]) {
                                out.extend(values);
                            }
                        }
                    }
                }
            }
        }
        Some(out)
    })()
    .unwrap_or_default();
    visited.remove(path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(source: &str) -> Vec<HttpClientCall> {
        detect_calls(
            Path::new("client.js"),
            source,
            Lang::TsJs,
            Patterns::patterns(),
        )
    }

    #[test]
    fn local_helpers_members_and_url_replacement() {
        let found = calls(
            r#"
            const ROUTES = { INIT: '/init' };
            const base = 'http://backend/old/prefix/';
            const make = () => { const route = ROUTES.INIT; return new URL(route, base).toString(); };
            const target = make();
            fetch(target, {method: 'POST'});
        "#,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].url.host, HostPart::Literal("backend".into()));
        assert_eq!(found[0].url.template.as_deref(), Some("/init"));
        let relative = calls("fetch(new URL('../next', 'http://backend/base/dir/').toString())");
        assert_eq!(relative[0].url.template.as_deref(), Some("/base/next"));
        let unknown_origin = calls(
            "const ORIGIN=`${upstream.protocol}//${upstream.host}`; const target=new URL('/init', ORIGIN).toString(); fetch(target);",
        );
        assert_eq!(unknown_origin[0].url.host, HostPart::Expr("ORIGIN".into()));
        assert_eq!(unknown_origin[0].url.template.as_deref(), Some("/init"));
    }

    #[test]
    fn unresolved_assigned_origin_keeps_its_alias_name() {
        let found = calls(
            r#"const UPSTREAM_HTTP_ORIGIN = `${upstream.protocol}//${upstream.host}`;
               const backendPath = pathname.substring(3) || '/';
               const targetUrl = `${UPSTREAM_HTTP_ORIGIN}${backendPath}`;
               fetch(targetUrl, { method: 'POST' });"#,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].url.host,
            HostPart::Expr("UPSTREAM_HTTP_ORIGIN".into())
        );
        assert_eq!(found[0].url.template, None);
    }

    #[test]
    fn aliases_are_bounded_and_opaque_urls_do_not_become_root() {
        for source in [
            "const A = B; const B = A; fetch(A)",
            "const A = () => B(); const B = () => A(); fetch(A())",
            "fetch(unknown())",
            "const target = flag ? '/a' : '/b'; fetch(target)",
            "const target = process.env.ROUTE || '/default'; fetch(target)",
        ] {
            let found = calls(source);
            assert_eq!(found.len(), 1, "{source}");
            assert_eq!(found[0].url.template, None, "{source}");
        }
    }

    #[test]
    fn parameter_mutation_and_effectful_helper_cannot_supply_a_constant() {
        for source in [
            "const PATH = '/wrong'; function f(PATH) { fetch(PATH); }",
            "const R = {path: '/wrong'}; R.path = runtime(); fetch(R.path);",
            "const path = '/wrong'; path += suffix; fetch(path);",
            "const make = () => { sideEffect(); return '/wrong'; }; fetch(make());",
            "const R={path:'/wrong'}; const make=()=>{ const effect=mutate(R); return R.path; }; fetch(make());",
            "const make = (path) => '/wrong'; fetch(make());",
            "const make = async () => '/wrong'; fetch(make());",
            "function run(URL) { fetch(new URL('/wrong', 'http://backend')); }",
        ] {
            let found = calls(source);
            assert_eq!(found.len(), 1, "{source}");
            assert_eq!(found[0].url.template, None, "{source}");
        }
    }

    #[test]
    fn aliases_use_their_declaration_scope() {
        let found = calls(
            "const ROOT='/right'; const TARGET=ROOT; function run(){ const ROOT='/wrong'; fetch(TARGET); }",
        );
        assert_eq!(found[0].url.template.as_deref(), Some("/right"));
    }

    #[test]
    fn request_method_obeys_object_overwrite_order() {
        for (options, expected) in [
            ("options", MethodSpec::Unknown),
            ("{...options}", MethodSpec::Unknown),
            ("{method:'GET', ...options}", MethodSpec::Unknown),
            (
                "{...options, method:'POST'}",
                MethodSpec::Known(HttpMethod::Post),
            ),
            (
                "{method:'GET', method:'DELETE'}",
                MethodSpec::Known(HttpMethod::Delete),
            ),
            ("{'method':'PATCH'}", MethodSpec::Known(HttpMethod::Patch)),
            ("{method}", MethodSpec::Unknown),
            ("{[key]:'POST'}", MethodSpec::Unknown),
            ("{headers}", MethodSpec::Known(HttpMethod::Get)),
        ] {
            let found = calls(&format!("fetch('/endpoint', {options});"));
            assert_eq!(found[0].method, expected, "{options}");
        }
        let found = calls("axios({url:'/endpoint', method:'GET', ...options});");
        assert_eq!(found[0].method, MethodSpec::Unknown);
    }

    fn workspace_calls(root: &Path, source: &str) -> Vec<HttpClientCall> {
        let mut queries = OutboundQueries::new();
        let patterns = Patterns::patterns();
        ensure_outbound_queries(&mut queries, patterns, Lang::TsJs);
        detect_calls_with_queries(
            &root.join("client.js"),
            source,
            Lang::TsJs,
            patterns,
            &queries,
            Some(root),
        )
    }

    #[test]
    fn env_default_const_resolves_to_its_literal_fallback() {
        // The nat-ui pattern: `const HTTP_PROXY_PATH = process.env.X || '/api'`.
        // The env branch is runtime-selected, but the `||` fallback is a
        // convention-based default the analyzer may report as the host.
        let found = calls(
            r#"const HTTP_PROXY_PATH = process.env.HTTP_PUBLIC_PATH || '/api';
               const UPDATE_DATA_STREAM = '/update-data-stream';
               const target = `${HTTP_PROXY_PATH}${UPDATE_DATA_STREAM}`;
               fetch(target);"#,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].url.host,
            HostPart::Expr("HTTP_PROXY_PATH".into())
        );
        assert_eq!(found[0].url.template.as_deref(), Some("/update-data-stream"));
    }

    #[test]
    fn env_default_with_dynamic_suffix_stays_dynamic() {
        let found = calls(
            r#"const HTTP_PROXY_PATH = process.env.HTTP_PUBLIC_PATH || '/api';
               fetch(`${HTTP_PROXY_PATH}${httpEndpointPath}`, { method: 'POST' });"#,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].url.host,
            HostPart::Expr("HTTP_PROXY_PATH".into())
        );
        assert_eq!(found[0].url.template, None);
    }

    #[test]
    fn imports_follow_declared_aliases_exports_and_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("constants")).unwrap();
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{"compilerOptions":{"paths":{"@/*":["./*"]}}}"#,
        )
        .unwrap();
        std::fs::write(root.join("constants/index.js"), "const PATH='/mcp/list'; const ROUTES={INIT:'/init'}; const HIDDEN='/private'; module.exports={PATH, ROUTES};").unwrap();
        std::fs::write(root.join("helper.js"), "import { PATH as MCP } from '@/constants'; export const getUrl = () => { const suffix = MCP; return `http://nat${suffix}`; };").unwrap();
        let found = workspace_calls(root, "import {getUrl} from './helper'; fetch(getUrl());");
        assert_eq!(found[0].url.host, HostPart::Literal("nat".into()));
        assert_eq!(found[0].url.template.as_deref(), Some("/mcp/list"));
        let found = workspace_calls(
            root,
            "const {ROUTES} = require('./constants'); fetch(new URL(ROUTES.INIT, 'http://nat/base/').toString());",
        );
        assert_eq!(found[0].url.template.as_deref(), Some("/init"));
        let found = workspace_calls(root, "import {HIDDEN} from './constants'; fetch(HIDDEN);");
        assert_eq!(found[0].url.template, None);
        std::fs::write(root.join("mutable.js"), "export let PATH = '/mutable';").unwrap();
        let found = workspace_calls(root, "import {PATH} from './mutable'; fetch(PATH);");
        assert_eq!(found[0].url.template, None);
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("outside.js"),
            "export const PATH='/escaped';",
        )
        .unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path().join("outside.js"), root.join("escape.js"))
                .unwrap();
            let found = workspace_calls(root, "import {PATH} from './escape'; fetch(PATH);");
            assert_eq!(found[0].url.template, None);
        }
    }

    #[test]
    fn commonjs_namespace_alias_destructuring_preserves_route() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("constants")).unwrap();
        std::fs::write(
            root.join("constants/index.js"),
            "const CA_RAG_INIT='/init'; const EXTENDED_ROUTES={CA_RAG_INIT}; module.exports={EXTENDED_ROUTES};",
        )
        .unwrap();
        let found = workspace_calls(
            root,
            "const constants = require('./constants'); const {EXTENDED_ROUTES} = constants; const ORIGIN=`${upstream.protocol}//${upstream.host}`; const target=new URL(EXTENDED_ROUTES.CA_RAG_INIT, ORIGIN).toString(); fetch(target, {method:'POST'});",
        );
        assert_eq!(found[0].url.host, HostPart::Expr("ORIGIN".into()));
        assert_eq!(found[0].url.template.as_deref(), Some("/init"));
    }

    #[test]
    fn defaults_import_cycles_and_escaping_modules_stay_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("routes.js"),
            "export const PATH = process.env.PATH_OVERRIDE || '/default';",
        )
        .unwrap();
        let found = workspace_calls(root, "import {PATH} from './routes'; fetch(PATH);");
        assert_eq!(found[0].url.template, None);
        std::fs::write(
            root.join("a.js"),
            "import {B} from './b'; export const A = B;",
        )
        .unwrap();
        std::fs::write(
            root.join("b.js"),
            "import {A} from './a'; export const B = A;",
        )
        .unwrap();
        let found = workspace_calls(root, "import {A} from './a'; fetch(A);");
        assert_eq!(found[0].url.template, None);
        std::fs::write(
            root.join("tsconfig.json"),
            r#"{"compilerOptions":{"paths":{"@/*":["./*"]}}}"#,
        )
        .unwrap();
        let found = workspace_calls(root, "import {PATH} from '@/../outside'; fetch(PATH);");
        assert_eq!(found[0].url.template, None);
    }

    #[test]
    fn ignored_imports_cannot_bypass_startup_input_fingerprints() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repo = git2::Repository::init(root).unwrap();
        std::fs::write(root.join(".gitignore"), "generated.js\n").unwrap();
        std::fs::write(root.join("generated.js"), "export const PATH='/generated';").unwrap();
        let found = workspace_calls(root, "import {PATH} from './generated'; fetch(PATH);");
        assert_eq!(found[0].url.template, None);
        // Checked-in generated code is a fingerprinted input even when ignored.
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("generated.js")).unwrap();
        index.write().unwrap();
        let found = workspace_calls(root, "import {PATH} from './generated'; fetch(PATH);");
        assert_eq!(found[0].url.template.as_deref(), Some("/generated"));
    }
}
