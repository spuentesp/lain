//! Shared helpers for protocol sensors.
//!
//! Sensors register via [`crate::server::sensors::Sensor`]; everything
//! here is imported, never re-implemented per sensor.

use crate::graph::GraphDatabase;
use crate::schema::GraphNode;
use ignore::DirEntry;
use std::path::Path;

/// Walk every file under `root`, honouring `.gitignore` and hidden-file
/// rules. Per-entry errors are flattened away — a malformed symlink or a
/// target that vanishes mid-walk must not abort ingestion.
///
/// Iteration order is filesystem-defined and matches the inline
/// `ignore::WalkBuilder` the sensors used previously (depth-first,
/// alphabetical), so any test that compares full output keeps passing.
///
/// Returns the [`DirEntry`] (not `&Path`) because the underlying
/// iterator owns each entry; callers typically do
/// `for entry in walk_workspace(root) { let path = entry.path(); … }`.
pub fn walk_workspace(root: &Path) -> impl Iterator<Item = DirEntry> {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .build()
        .flatten()
}

/// `CamelCase` / `mixedCase` → `snake_case`. Each non-initial uppercase
/// char introduces a `_` separator.
pub fn to_snake_case(name: &str) -> String {
    let mut result = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(c.to_ascii_lowercase());
    }
    result
}

/// `snake_case` → `camelCase`. The first non-`_` char stays as-is;
/// each subsequent `_`-delimited segment is title-cased.
pub fn to_camel_case(name: &str) -> String {
    let mut result = String::new();
    let mut capitalize = false;
    for c in name.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            result.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// Look up a handler node by `name`, trying exact, then `snake_case`,
/// then `camelCase`. Empty `name` returns `None`.
///
/// Unifying the priority to `exact → snake → camel` is a strict
/// superset of proto's old `exact → snake` (proto gains camelCase
/// matches) and graphql's old `exact → camel → snake`. In practice the
/// three orderings agree on every input that doesn't start with `_`,
/// because `to_camel_case(x)` of a string with no `_` returns `x`
/// unchanged — so `camel` and `snake` swap positions without changing
/// the resolved set. The only realistic divergence is when both a
/// snake-case and a camelCase form exist as separate nodes; the new
/// order picks snake. The audit's case-mismatch incidents all turned
/// on missing one of the forms, not on choosing between them.
pub fn find_handler_in_graph(graph: &GraphDatabase, name: &str) -> Option<GraphNode> {
    if name.is_empty() {
        return None;
    }
    graph
        .find_node_by_name(name)
        .or_else(|| graph.find_node_by_name(&to_snake_case(name)))
        .or_else(|| graph.find_node_by_name(&to_camel_case(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_handles_camel_and_pascal() {
        assert_eq!(to_snake_case("getUser"), "get_user");
        assert_eq!(to_snake_case("GetUser"), "get_user");
        assert_eq!(to_snake_case("URL"), "u_r_l");
        assert_eq!(to_snake_case(""), "");
    }

    #[test]
    fn camel_case_handles_snake_and_passthrough() {
        assert_eq!(to_camel_case("get_user"), "getUser");
        assert_eq!(to_camel_case("GetUser"), "GetUser");
        assert_eq!(to_camel_case(""), "");
    }

    #[test]
    fn walker_visits_a_written_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.proto"), "syntax = \"proto3\";\n").unwrap();
        let names: Vec<_> = walk_workspace(dir.path())
            .map(|e| e.path().to_path_buf())
            .filter_map(|p| p.file_name().map(|n| n.to_owned()))
            .collect();
        assert!(
            names.iter().any(|n| n == "a.proto"),
            "walker missed the file: {names:?}"
        );
    }
}
