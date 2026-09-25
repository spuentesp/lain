//! Context domain handlers - build LLM-optimized context

use crate::error::LainError;
use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::server::tools::utils::resolve_node;

pub fn get_context_for_prompt(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
    max_tokens: Option<usize>,
) -> Result<String, LainError> {
    let node = resolve_node(graph, overlay, symbol)?;
    let max_toks = max_tokens.unwrap_or(2000);

    let mut parts = Vec::new();

    // Node identity
    parts.push(format!("## {} ({:?})\n", node.name, node.node_type));
    parts.push(format!("Path: {}\n", node.path));

    // Signature
    if let Some(ref sig) = node.signature {
        parts.push(format!("Signature: `{}`\n", sig));
    }

    // Docstring
    if let Some(ref doc) = node.docstring {
        parts.push(format!("Documentation: {}\n", doc));
    }

    // Relationships (callers and callees)
    let callers = graph
        .get_edges_to(&node.id)?
        .into_iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::Calls)
        .filter_map(|e| graph.get_node(&e.source_id).ok().flatten())
        .map(|n| n.name)
        .collect::<Vec<_>>();

    let callees = graph
        .get_edges_from(&node.id)?
        .into_iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::Calls)
        .filter_map(|e| graph.get_node(&e.target_id).ok().flatten())
        .map(|n| n.name)
        .collect::<Vec<_>>();

    if !callers.is_empty() {
        parts.push(format!("Called by: {}\n", callers.join(", ")));
    }
    if !callees.is_empty() {
        parts.push(format!("Calls: {}\n", callees.join(", ")));
    }

    // Type context (for structs/enums)
    if matches!(
        node.node_type,
        crate::schema::NodeType::Struct | crate::schema::NodeType::Enum
    ) {
        let uses = graph
            .get_edges_from(&node.id)?
            .into_iter()
            .filter(|e| e.edge_type == crate::schema::EdgeType::Uses)
            .filter_map(|e| graph.get_node(&e.target_id).ok().flatten())
            .map(|n| format!("{} ({:?})", n.name, n.node_type))
            .collect::<Vec<_>>();
        if !uses.is_empty() {
            parts.push(format!("Uses types: {}\n", uses.join(", ")));
        }
    }

    // Co-change partners
    let partners = graph.get_co_change_partners(&node.path)?;
    if !partners.is_empty() {
        parts.push(format!(
            "Frequently co-changes with: {}\n",
            partners
                .iter()
                .take(3)
                .map(|(p, _)| p.clone())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    // Join and truncate
    let mut context = parts.join("\n");
    let token_count = context.split_whitespace().count() * 2; // rough estimate
    if token_count > max_toks {
        let words: Vec<&str> = context.split_whitespace().collect();
        let truncated = words
            .into_iter()
            .take(max_toks / 2)
            .collect::<Vec<_>>()
            .join(" ");
        context = format!("{}...\n[truncated - {} tokens]", truncated, token_count);
    }

    Ok(context)
}

/// Resolve a graph-relative path against the repo it belongs to.
///
/// Graph paths are workspace-relative, and `std::fs` resolves a relative
/// path against the *process* working directory. In single-workspace
/// mode those coincide often enough that nothing showed; in a multi-repo
/// federation they never do, and `get_code_snippet` on a symbol in one
/// repo happily returned the same-named file from wherever the server
/// happened to be launched — `src/lib.rs` from lain's own checkout
/// instead of the repo that was asked about. Wrong file, no error.
/// Resolve a caller-supplied path to a file inside the workspace.
///
/// Anything that resolves outside it is refused. This used to pass absolute
/// paths through and join `../..` unchecked, so `get_code_snippet` read any
/// file on the host — `/etc/passwd`, keys, other repositories — and
/// `lain server` serves that tool over HTTP.
pub(crate) fn resolve_against_workspace(
    workspace: &std::path::Path,
    path: &str,
) -> Result<String, LainError> {
    let p = std::path::Path::new(path);
    // Rooted without being absolute is a Windows case: `\Windows\win.ini`
    // or `/etc/passwd` has no drive, so `is_absolute()` is false and
    // `join` resolves it against the workspace's drive root — outside it.
    let rooted = p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Prefix(_)
            )
        });
    let candidate = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    };
    let outside = || {
        LainError::Other(format!(
            "path '{path}' is outside the repository; pass a path relative to {}",
            workspace.display()
        ))
    };
    let root = dunce::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    match dunce::canonicalize(&candidate) {
        Ok(real) if real.starts_with(&root) => Ok(real.to_string_lossy().into_owned()),
        Ok(_) => Err(outside()),
        // Missing file: refuse `..` escapes lexically, and let the read
        // report "not found" for anything else.
        Err(_) => {
            let escapes = p
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
            if rooted || escapes {
                Err(outside())
            } else {
                Ok(candidate.to_string_lossy().into_owned())
            }
        }
    }
}

pub fn get_code_snippet(
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    workspace: &std::path::Path,
    path: &str,
    line: Option<u32>,
    context_lines: Option<usize>,
) -> Result<String, LainError> {
    // `line` is 1-based, as shown in every tool's output; graph ranges are
    // 0-based. Mixing them showed the blank line above a symbol and cut
    // its last line off.
    let line_num = line.unwrap_or(1).max(1) as usize;
    let disk_path = resolve_against_workspace(workspace, path)?;
    // Around a symbol, context is added only when asked for.
    let around = context_lines.unwrap_or(0);
    let symbol_range = |ls: u32, le: u32| {
        let first = (ls as usize + 1).saturating_sub(around).max(1);
        (first, le as usize + 1 + around)
    };

    // Try overlay first
    if let Some(node) = overlay.get_node(path) {
        if let (Some(ls), Some(le)) = (node.line_start, node.line_end) {
            let (first, last) = symbol_range(ls, le);
            return read_file_range(&disk_path, first, last);
        }
    }

    // Fall back to graph, keyed by the repo-relative path: `./pkg/a.py`
    // and the absolute spelling missed the symbol lookup that `pkg/a.py`
    // got, and showed a different range.
    let key = crate::server::graph::graph_path(workspace, std::path::Path::new(&disk_path));
    if let Some(node) = graph.get_node_at_location(&key, line_num as u32 - 1) {
        if let (Some(ls), Some(le)) = (node.line_start, node.line_end) {
            let (first, last) = symbol_range(ls, le);
            return read_file_range(&disk_path, first, last);
        }
    }

    // Just read the file with context around the line
    let ctx = context_lines.unwrap_or(10);
    read_file_range(
        &disk_path,
        line_num.saturating_sub(ctx).max(1),
        line_num + ctx,
    )
}

/// Lines `first..=last` (1-based, clamped to the file) with their numbers.
fn read_file_range(path: &str, first: usize, last: usize) -> Result<String, LainError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| LainError::NotFound(format!("Path not found: {path} ({e})")))?;
    let lines: Vec<&str> = content.lines().collect();
    let first = first.max(1);
    let last = last.min(lines.len());
    if first > lines.len() {
        return Err(LainError::NotFound(format!(
            "line {first} is past the end of {path} ({} lines)",
            lines.len()
        )));
    }
    if first > last {
        return Err(LainError::NotFound(format!(
            "Invalid range: {first} to {last} ({path} has {} lines)",
            lines.len()
        )));
    }
    let snippet: Vec<String> = lines[first - 1..last]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:4}: {}", first + i, l))
        .collect();
    Ok(format!(
        "File: {}\nShowing lines {}-{}\n\n{}\n",
        path,
        first,
        last,
        snippet.join("\n")
    ))
}

pub fn get_call_sites(
    workspace: &std::path::Path,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
    symbol: &str,
) -> Result<String, LainError> {
    let (node, other_defs) =
        crate::server::tools::utils::resolve_node_ambiguous(graph, overlay, symbol)?;
    let amb = crate::server::tools::utils::ambiguity_note(&node, &other_defs);
    let freshness = graph.freshness(workspace, &node.path);
    let target_id = &node.id;

    // Find all callers (edges of type Calls pointing to this node)
    let callers = graph
        .get_edges_to(target_id)?
        .into_iter()
        .filter(|e| e.edge_type == crate::schema::EdgeType::Calls)
        .filter_map(|e| graph.get_node(&e.source_id).ok().flatten())
        .collect::<Vec<_>>();

    if callers.is_empty() {
        // A leaf really has no callers; a stale file only looks like one. These
        // read identically to a caller acting on the answer, so distinguish them.
        return Ok(match freshness.note(&node.path) {
            Some(note) => format!(
                "{amb}{note}\nNo call sites found for '{symbol}' in the graph — \
                 callers added since the last index would not appear."
            ),
            None => format!("{amb}No call sites found for '{symbol}' ({symbol} is a leaf)"),
        });
    }

    let mut result = amb.clone();
    if let Some(note) = freshness.note(&node.path) {
        result.push_str(&note);
        result.push('\n');
    }
    // Locate the actual call lines. `Calls` edges connect caller to
    // callee and carry no position, so this listed each calling
    // function's own definition range under the heading "Call sites" —
    // `build_core_memory at ...:19-360` is a 341-line span, not a call
    // site, and a function calling the target three times still counted
    // as one. Scanning the caller's body for the name gives the real
    // positions and the real count.
    let mut lines_by_caller: Vec<(&crate::schema::GraphNode, Vec<usize>)> = Vec::new();
    let mut total_sites = 0usize;
    for caller in &callers {
        let sites = call_lines_in(workspace, caller, &node.name);
        total_sites += sites.len();
        lines_by_caller.push((caller, sites));
    }

    let heading = if total_sites > 0 && total_sites != callers.len() {
        format!(
            "Call sites for '{}' ({} call(s) across {} function(s)):\n\n",
            symbol,
            total_sites,
            callers.len()
        )
    } else {
        format!(
            "Call sites for '{}' ({} found):\n\n",
            symbol,
            total_sites.max(callers.len())
        )
    };
    result.push_str(&heading);

    for (caller, sites) in lines_by_caller {
        if sites.is_empty() {
            // Nothing matched textually — the file may have moved on
            // since indexing. Fall back to the enclosing function and
            // say that is what this is, rather than passing a
            // definition range off as a call position.
            let loc = if let (Some(ls), Some(le)) = (caller.line_start, caller.line_end) {
                format!("{}:{}-{}", caller.path, ls + 1, le + 1)
            } else {
                caller.path.clone()
            };
            result.push_str(&format!(
                "- **{}** — enclosing function at {} (exact call line not found in the file on disk)\n",
                caller.name, loc
            ));
        } else {
            let joined = sites
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let label = if sites.len() == 1 { "line" } else { "lines" };
            result.push_str(&format!(
                "- **{}** ({}) calls it at {} {}\n",
                caller.name, caller.path, label, joined
            ));
        }
    }

    Ok(result)
}

/// 1-based line numbers inside `caller`'s body where `callee` appears as
/// a whole word.
///
/// `line_start` is used only to bound the scan and is treated as
/// advisory: it has been observed one off from the definition line, so
/// the range is widened by one rather than trusted exactly.
fn call_lines_in(
    workspace: &std::path::Path,
    caller: &crate::schema::GraphNode,
    callee: &str,
) -> Vec<usize> {
    if callee.is_empty() {
        return Vec::new();
    }
    let path = workspace.join(&caller.path);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let (lo, hi) = match (caller.line_start, caller.line_end) {
        // Graph ranges are 0-based and inclusive; `lineno` below is 1-based.
        (Some(ls), Some(le)) => (ls as usize + 1, le as usize + 1),
        _ => (0usize, usize::MAX),
    };
    // Lines naming the callee as a word, and among them the ones shaped
    // like a call (`name(`, `name (`). The word alone also matched
    // docstrings and prose (`:param send:`), so a caller showed five
    // "call" lines for one call. Call-shaped lines win; bare mentions are
    // kept only when there is no call-shaped line at all (Ruby and Scala
    // call without parentheses).
    let mut out = Vec::new();
    let mut call_shaped = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let lineno = idx + 1;
        if lineno < lo || lineno > hi {
            continue;
        }
        let t = line.trim_start();
        if ["//", "#", "/*", "* ", "--"]
            .iter()
            .any(|c| t.starts_with(c))
            || t == "*"
        {
            continue;
        }
        let mut found = false;
        let mut shaped = false;
        for (i, _) in line.match_indices(callee) {
            let before = line[..i].chars().next_back();
            let rest = &line[i + callee.len()..];
            let after = rest.chars().next();
            let boundary = |c: Option<char>| !c.is_some_and(|c| c.is_alphanumeric() || c == '_');
            if boundary(before) && boundary(after) {
                found = true;
                let next = rest.trim_start().chars().next();
                // `name(`, `name<T>(` / `name::<T>(`, `name { … }` (Swift
                // trailing closure), `name!(` (Rust macro form).
                if matches!(next, Some('(' | '<' | '{' | '!')) || rest.starts_with("::<") {
                    shaped = true;
                }
            }
        }
        // An import names the callee without calling it.
        if ["import ", "from ", "use ", "#include", "using ", "require "]
            .iter()
            .any(|kw| t.starts_with(kw))
        {
            continue;
        }
        // Skip the definition itself: a definition keyword followed by the
        // callee's name. This used to skip every line *starting* with
        // `fn ` — a Rust-only rule that also dropped Python's
        // `fn = guess_filename(v) or k`, leaving the call "not found".
        let defines = ["fn", "def", "func", "fun", "function"]
            .iter()
            .any(|kw| line.contains(&format!("{kw} {callee}")));
        if found && !defines {
            out.push(lineno);
            if shaped {
                call_shaped.push(lineno);
            }
        }
    }
    if call_shaped.is_empty() {
        out
    } else {
        call_shaped
    }
}

#[cfg(test)]
mod call_lines_tests {
    use super::*;

    fn caller_in(path: &str, lines: (u32, u32)) -> crate::schema::GraphNode {
        let mut n = crate::schema::GraphNode::new(
            crate::schema::NodeType::Function,
            "caller".into(),
            path.into(),
        );
        n.line_start = Some(lines.0);
        n.line_end = Some(lines.1);
        n
    }

    /// `get_code_snippet` reads only inside the repository.
    #[test]
    fn paths_outside_the_workspace_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let ws = root.path().join("repo");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/a.py"), "x = 1\n").unwrap();
        std::fs::write(root.path().join("secret.txt"), "key\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.path().join("secret.txt"), ws.join("link.txt")).unwrap();

        assert!(resolve_against_workspace(&ws, "src/a.py").is_ok());
        assert!(resolve_against_workspace(&ws, &ws.join("src/a.py").to_string_lossy()).is_ok());
        for bad in [
            "../secret.txt",
            "src/../../secret.txt",
            "/etc/passwd",
            "../missing.txt",
        ] {
            assert!(
                resolve_against_workspace(&ws, bad).is_err(),
                "{bad} must be refused"
            );
        }
        assert!(
            resolve_against_workspace(&ws, &root.path().join("secret.txt").to_string_lossy())
                .is_err()
        );
        #[cfg(unix)]
        assert!(
            resolve_against_workspace(&ws, "link.txt").is_err(),
            "a symlink out of the repo"
        );
        // A missing in-repo path passes through so the read reports "not found".
        assert!(resolve_against_workspace(&ws, "src/nope.py").is_ok());
    }

    /// A symbol's snippet is exactly its lines: not the one above, not
    /// missing the last.
    #[test]
    fn snippet_shows_exactly_the_symbol() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join("u.py"),
            "import os\n\ndef f(x):\n    y = x\n    return y\n\nz = 1\n",
        )
        .unwrap();
        let graph = GraphDatabase::new(&ws.path().join(".lain")).unwrap();
        let ns = crate::schema::RepoNamespace::for_test();
        // 0-based lines 2..=4: `def f` through `return y`.
        graph
            .upsert_node(
                crate::schema::GraphNode::new(
                    crate::schema::NodeType::Function,
                    "f".into(),
                    "u.py".into(),
                )
                .with_location_in(2, 4, &ns),
            )
            .unwrap();
        let overlay = VolatileOverlay::new();
        let out = get_code_snippet(&graph, &overlay, ws.path(), "u.py", Some(3), None).unwrap();
        assert!(out.contains("Showing lines 3-5"), "{out}");
        // Other spellings of the same file find the same symbol.
        let abs = ws.path().join("u.py").to_string_lossy().to_string();
        for spelling in ["./u.py", abs.as_str()] {
            let other =
                get_code_snippet(&graph, &overlay, ws.path(), spelling, Some(3), None).unwrap();
            assert!(other.contains("Showing lines 3-5"), "{spelling}: {other}");
        }
        assert!(
            out.contains("   3: def f(x):") && out.contains("   5:     return y"),
            "{out}"
        );
        assert!(
            !out.contains("import os") && !out.contains("z = 1"),
            "{out}"
        );
        // Explicit context widens it.
        let out = get_code_snippet(&graph, &overlay, ws.path(), "u.py", Some(3), Some(1)).unwrap();
        assert!(out.contains("Showing lines 2-6"), "{out}");
    }

    /// A Python variable named `fn` is not a Rust definition.
    #[test]
    fn a_line_starting_with_fn_can_be_a_call() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join("models.py"),
            "def _encode_files(files):\n    fn = guess_filename(v) or k\n    return fn\n",
        )
        .unwrap();
        let lines = call_lines_in(ws.path(), &caller_in("models.py", (1, 3)), "guess_filename");
        assert_eq!(lines, vec![2]);
        std::fs::write(
            ws.path().join("m.py"),
            "from pkg.b import guess_filename\n\ndef f(v):\n    return guess_filename(v)\n",
        )
        .unwrap();
        let lines = call_lines_in(ws.path(), &caller_in("m.py", (0, 3)), "guess_filename");
        assert_eq!(lines, vec![4], "the import line is not a call site");
    }

    /// The definition line itself is still skipped, in any language.
    #[test]
    fn definition_lines_are_not_call_sites() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join("a.py"),
            "def helper(x):\n    return helper(x - 1)\n",
        )
        .unwrap();
        std::fs::write(ws.path().join("a.rs"), "fn helper() {\n    helper();\n}\n").unwrap();
        assert_eq!(
            call_lines_in(ws.path(), &caller_in("a.py", (1, 2)), "helper"),
            vec![2]
        );
        assert_eq!(
            call_lines_in(ws.path(), &caller_in("a.rs", (1, 3)), "helper"),
            vec![2]
        );
    }

    /// Docstring and comment mentions are not call sites; a paren-less
    /// call still counts when it is the only mention.
    #[test]
    fn prose_mentions_are_not_call_sites() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join("s.py"),
            "def request(self):
    \"\"\"Build and send it.\n    :param send: whether to send\n    \"\"\"\n    # send happens below\n    return self.send(prep)\n",
        )
        .unwrap();
        assert_eq!(
            call_lines_in(ws.path(), &caller_in("s.py", (0, 5)), "send"),
            vec![6]
        );
        std::fs::write(
            ws.path().join("r.rb"),
            "def go
  deliver :now
end
",
        )
        .unwrap();
        assert_eq!(
            call_lines_in(ws.path(), &caller_in("r.rb", (0, 2)), "deliver"),
            vec![2]
        );
    }
}
