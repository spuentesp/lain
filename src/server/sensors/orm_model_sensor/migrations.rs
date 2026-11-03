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

#[derive(Debug, Clone)]
struct SqlToken {
    text: String,
    quoted: bool,
}

fn sql_tokens(statement: &str) -> Vec<SqlToken> {
    let bytes = statement.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() || matches!(bytes[index], b',' | b'(' | b')' | b';') {
            index += 1;
            continue;
        }
        if bytes[index] == b'-' && bytes.get(index + 1) == Some(&b'-') {
            break;
        }
        if matches!(bytes[index], b'\'' | b'"' | b'`' | b'[') {
            let closing = match bytes[index] {
                b'\'' => b'\'',
                b'"' => b'"',
                b'`' => b'`',
                _ => b']',
            };
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != closing {
                end += 1;
            }
            if end < bytes.len() {
                tokens.push(SqlToken {
                    text: statement[start..end].to_string(),
                    quoted: true,
                });
                index = end + 1;
            } else {
                index = start;
            }
            continue;
        }
        let start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'.' | b'$'))
        {
            index += 1;
        }
        if start < index {
            tokens.push(SqlToken {
                text: statement[start..index].to_string(),
                quoted: false,
            });
        } else {
            index += 1;
        }
    }
    tokens
}

fn token_eq(token: &SqlToken, expected: &str) -> bool {
    if token.quoted {
        token.text == expected
    } else {
        token.text.eq_ignore_ascii_case(expected)
    }
}

fn keyword(token: &SqlToken, expected: &str) -> bool {
    !token.quoted && token.text.eq_ignore_ascii_case(expected)
}

fn alter_identity(statement: &str) -> Option<(SqlToken, SqlToken)> {
    let tokens = sql_tokens(statement);
    if tokens.len() < 6 || !keyword(&tokens[0], "ALTER") || !keyword(&tokens[1], "TABLE") {
        return None;
    }
    let table = tokens[2].clone();
    if !keyword(&tokens[3], "ADD") || !keyword(&tokens[4], "COLUMN") {
        return None;
    }
    let (column, _) = if tokens.len() >= 9
        && keyword(&tokens[5], "IF")
        && keyword(&tokens[6], "NOT")
        && keyword(&tokens[7], "EXISTS")
    {
        (tokens[8].clone(), 8)
    } else {
        (tokens[5].clone(), 5)
    };
    Some((table, column))
}

fn quoted_statements(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut statements = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !matches!(bytes[index], b'\'' | b'"' | b'`') {
            index += 1;
            continue;
        }
        let quote = bytes[index];
        let start = index + 1;
        let mut end = start;
        let mut escaped = false;
        while end < bytes.len() {
            if escaped {
                escaped = false;
            } else if bytes[end] == b'\\' {
                escaped = true;
            } else if bytes[end] == quote {
                break;
            }
            end += 1;
        }
        if end < bytes.len() {
            statements.push(line[start..end].to_string());
            index = end;
        } else {
            index = start;
        }
        index += 1;
    }
    statements
}

fn attach_sql_statement(analysis: &mut OrmAnalysis, path: &str, line: u32, statement: &str) {
    let Some((table, column)) = alter_identity(statement) else {
        return;
    };
    let site = SourceSite {
        path: path.to_string(),
        line,
    };
    for candidate in analysis.columns.iter_mut().filter(|candidate| {
        token_eq(&table, &candidate.table) && token_eq(&column, &candidate.name)
    }) {
        candidate.migration = Some(site.clone());
    }
}

pub(super) fn attach_sql_migrations(root: &Path, analysis: &mut OrmAnalysis) {
    for (file, content, _) in crate::server::sensors::util::scan_files(root, |path| {
        let normalized = crate::graph::graph_path(root, path);
        (normalized.contains("migration") || normalized.contains("alembic"))
            .then_some(())
            .filter(|_| {
                matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("sql" | "ts" | "js")
                )
            })
    }) {
        let path = crate::graph::graph_path(root, &file);
        for (offset, line) in content.lines().enumerate() {
            let line_number = offset as u32 + 1;
            attach_sql_statement(analysis, &path, line_number, line);
            for statement in quoted_statements(line) {
                attach_sql_statement(analysis, &path, line_number, &statement);
            }
        }
    }
}
