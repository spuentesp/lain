use super::django::{capture_index, string_literal, text};
use super::OrmAnalysis;
use crate::federation::contracts::model::SourceSite;
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::util::{language_for, parse_for_lang, Lang};
use std::path::Path;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

fn keyword_argument<'a>(arguments: Node<'a>, name: &str, src: &'a [u8]) -> Option<Node<'a>> {
    let mut cursor = arguments.walk();
    let found = arguments.named_children(&mut cursor).find_map(|argument| {
        if argument.kind() != "keyword_argument" {
            return None;
        }
        let key = argument.child_by_field_name("name")?;
        (text(key, src) == name)
            .then(|| argument.child_by_field_name("value"))
            .flatten()
    });
    found
}

fn positional_strings(arguments: Node, src: &[u8]) -> Vec<String> {
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|argument| argument.kind() != "keyword_argument")
        .filter_map(|argument| string_literal(argument, src))
        .collect()
}

fn nested_column_call(arguments: Node) -> Option<Node> {
    let mut cursor = arguments.walk();
    let found = arguments
        .named_children(&mut cursor)
        .find(|argument| argument.kind() == "call");
    found
}

fn column_name_from_call(call: Node, src: &[u8]) -> Option<String> {
    let arguments = call.child_by_field_name("arguments")?;
    if let Some(name) = keyword_argument(arguments, "name", src) {
        return string_literal(name, src);
    }
    positional_strings(arguments, src).into_iter().next()
}

fn attach_alembic(
    analysis: &mut OrmAnalysis,
    path: &str,
    operation: Node,
    arguments: Node,
    src: &[u8],
) {
    let Some(table) = positional_strings(arguments, src).into_iter().next() else {
        return;
    };
    let Some(column_call) = nested_column_call(arguments) else {
        return;
    };
    let Some(column) = column_name_from_call(column_call, src) else {
        return;
    };
    let site = SourceSite {
        path: path.to_string(),
        line: operation.start_position().row as u32 + 1,
    };
    for candidate in analysis
        .columns
        .iter_mut()
        .filter(|candidate| candidate.table == table && candidate.name == column)
    {
        candidate.migration = Some(site.clone());
    }
}

fn attach_django(
    analysis: &mut OrmAnalysis,
    path: &str,
    operation: Node,
    arguments: Node,
    src: &[u8],
) {
    let Some(model_name) = keyword_argument(arguments, "model_name", src)
        .and_then(|value| string_literal(value, src))
        .map(|value| value.to_lowercase())
    else {
        return;
    };
    let Some(attribute) =
        keyword_argument(arguments, "name", src).and_then(|value| string_literal(value, src))
    else {
        return;
    };
    let Some(field) = keyword_argument(arguments, "field", src) else {
        return;
    };
    let column = field
        .child_by_field_name("arguments")
        .and_then(|arguments| keyword_argument(arguments, "db_column", src))
        .and_then(|value| string_literal(value, src))
        .unwrap_or(attribute);
    let model_matches = analysis
        .models
        .iter()
        .any(|model| model.name.to_lowercase() == model_name || model.table == model_name);
    if !model_matches {
        return;
    }
    let table = analysis
        .models
        .iter()
        .find(|model| model.name.to_lowercase() == model_name || model.table == model_name)
        .map(|model| model.table.clone());
    let Some(table) = table else {
        return;
    };
    let site = SourceSite {
        path: path.to_string(),
        line: operation.start_position().row as u32 + 1,
    };
    for candidate in analysis
        .columns
        .iter_mut()
        .filter(|candidate| candidate.table == table && candidate.name == column)
    {
        candidate.migration = Some(site.clone());
    }
}

fn attach_file(
    analysis: &mut OrmAnalysis,
    root: &Path,
    file: &Path,
    content: &str,
    patterns: &Patterns,
    query_key: &str,
    django: bool,
) {
    let Ok(queries) = patterns.compiled_queries() else {
        return;
    };
    let Some((_, _, _, body)) = queries.iter().find(|(key, _, _, _)| *key == query_key) else {
        return;
    };
    let Ok(query) = Query::new(&language_for(Lang::Python), body) else {
        return;
    };
    let Some(operation_index) = capture_index(&query, "operation") else {
        return;
    };
    let Some(arguments_index) = capture_index(&query, "arguments") else {
        return;
    };
    let Some(tree) = parse_for_lang(Lang::Python, content) else {
        return;
    };
    let path = crate::graph::graph_path(root, file);
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
    while let Some(matched) = matches.next() {
        let operation = matched
            .captures
            .iter()
            .find(|capture| capture.index == operation_index);
        let arguments = matched
            .captures
            .iter()
            .find(|capture| capture.index == arguments_index);
        if let (Some(operation), Some(arguments)) = (operation, arguments) {
            if django {
                attach_django(
                    analysis,
                    &path,
                    operation.node,
                    arguments.node,
                    content.as_bytes(),
                );
            } else {
                attach_alembic(
                    analysis,
                    &path,
                    operation.node,
                    arguments.node,
                    content.as_bytes(),
                );
            }
        }
    }
}

pub(super) fn attach_python_migrations(
    root: &Path,
    analysis: &mut OrmAnalysis,
    patterns: &Patterns,
) {
    for (file, content, ()) in crate::server::sensors::util::scan_files(root, |path| {
        let normalized = crate::graph::graph_path(root, path);
        (normalized.contains("migration") || normalized.contains("alembic"))
            .then_some(())
            .filter(|_| path.extension().and_then(|extension| extension.to_str()) == Some("py"))
    }) {
        attach_file(
            analysis,
            root,
            &file,
            &content,
            patterns,
            "python/sqlalchemy-orm-migration.scm",
            false,
        );
        attach_file(
            analysis,
            root,
            &file,
            &content,
            patterns,
            "python/django-orm-migration.scm",
            true,
        );
    }
}
