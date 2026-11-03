use super::django::{capture_index, text};
use super::{add_column, add_model, OrmAnalysis};
use crate::schema::StaticSource;
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::util::{language_for, parse_for_lang, to_snake_case, Lang};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

fn string_literal(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if current.kind() == "string_fragment" {
            return Some(text(current, src));
        }
        let mut cursor = current.walk();
        pending.extend(current.named_children(&mut cursor));
    }
    None
}

fn ancestors(node: Node) -> impl Iterator<Item = Node> {
    std::iter::successors(node.parent(), |parent| parent.parent())
}

fn object_name(arguments: Node, src: &[u8]) -> Option<Option<String>> {
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if argument.kind() == "string" {
            return Some(string_literal(argument, src));
        }
        if argument.kind() != "object" {
            continue;
        }
        let mut object_cursor = argument.walk();
        for pair in argument.named_children(&mut object_cursor) {
            if pair.kind() != "pair" {
                continue;
            }
            let key = pair.child_by_field_name("key")?;
            if text(key, src) != "name" {
                continue;
            }
            let value = pair.child_by_field_name("value")?;
            return Some(string_literal(value, src));
        }
    }
    None
}

fn class_declaration(decorator: Node) -> Option<Node> {
    let parent = decorator.parent()?;
    if parent.kind() == "class_declaration" {
        return Some(parent);
    }
    let mut cursor = parent.walk();
    let found = parent
        .named_children(&mut cursor)
        .find(|child| child.kind() == "class_declaration");
    found
}

fn decorator_arguments(decorator: Node) -> Option<Node> {
    let mut cursor = decorator.walk();
    let found = decorator
        .named_children(&mut cursor)
        .find(|child| child.kind() == "call_expression")?
        .child_by_field_name("arguments");
    found
}

fn analyze_typeorm_class(
    analysis: &mut OrmAnalysis,
    path: &str,
    entity: Node,
    class: Node,
    src: &[u8],
) {
    let Some(name) = class.child_by_field_name("name") else {
        return;
    };
    let model = text(name, src);
    let Some(arguments) = decorator_arguments(entity) else {
        return;
    };
    let table =
        object_name(arguments, src).map_or_else(|| Some(to_snake_case(&model)), |name| name);
    let Some(table) = table else {
        analysis
            .unresolved
            .push(format!("{path}: dynamic table name on {model}"));
        return;
    };
    add_model(
        analysis,
        "typeorm",
        &model,
        &table,
        path,
        class.start_position().row as u32 + 1,
        StaticSource::TreeSitter,
    );
    let mut pending = class
        .named_child(class.named_child_count().saturating_sub(1))
        .into_iter()
        .collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        if node.kind() == "decorator" {
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            pending.extend(children);
            let mut decorator_cursor = node.walk();
            for child in node.named_children(&mut decorator_cursor) {
                if child.kind() != "call_expression" {
                    continue;
                }
                let Some(function) = child.child_by_field_name("function") else {
                    continue;
                };
                if text(function, src) != "Column" {
                    continue;
                }
                let Some(arguments) = child.child_by_field_name("arguments") else {
                    continue;
                };
                let Some(property) = ancestors(node).find(|parent| {
                    matches!(
                        parent.kind(),
                        "public_field_definition" | "class_property" | "property_signature"
                    )
                }) else {
                    continue;
                };
                let Some(property_name) = property.child_by_field_name("name").or_else(|| {
                    let mut cursor = property.walk();
                    let found = property
                        .named_children(&mut cursor)
                        .find(|child| child.kind() == "property_identifier");
                    found
                }) else {
                    continue;
                };
                let attribute = text(property_name, src);
                let column = object_name(arguments, src)
                    .map_or_else(|| Some(attribute.clone()), |name| name);
                if let Some(column) = column {
                    add_column(
                        analysis,
                        &table,
                        &column,
                        path,
                        node.start_position().row as u32 + 1,
                        StaticSource::TreeSitter,
                    );
                } else {
                    analysis
                        .unresolved
                        .push(format!("{path}: dynamic column on {model}"));
                }
            }
        } else {
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            pending.extend(children);
        }
    }
}

fn analyze_typeorm_file(
    analysis: &mut OrmAnalysis,
    path: &str,
    content: &str,
    patterns: &Patterns,
    lang: Lang,
) {
    let bucket = match lang {
        Lang::Ts => "ts",
        Lang::Tsx => "tsx",
        _ => return,
    };
    let key = format!("{bucket}/typeorm-orm-model.scm");
    let Ok(queries) = patterns.compiled_queries() else {
        return;
    };
    let Some((_, _, _, body)) = queries
        .iter()
        .find(|(candidate, _, _, _)| *candidate == key)
    else {
        return;
    };
    let Ok(query) = Query::new(&language_for(lang), body) else {
        return;
    };
    let Some(decorator_index) = capture_index(&query, "orm_decorator") else {
        return;
    };
    let Some(name_index) = capture_index(&query, "decorator") else {
        return;
    };
    let Some(tree) = parse_for_lang(lang, content) else {
        return;
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
    while let Some(matched) = matches.next() {
        let decorator = matched
            .captures
            .iter()
            .find(|capture| capture.index == decorator_index);
        let name = matched
            .captures
            .iter()
            .find(|capture| capture.index == name_index);
        if let (Some(decorator), Some(name)) = (decorator, name) {
            if text(name.node, content.as_bytes()) != "Entity" {
                continue;
            }
            if let Some(class) = class_declaration(decorator.node) {
                analyze_typeorm_class(analysis, path, decorator.node, class, content.as_bytes());
            }
        }
    }
}

fn variable_model(call: Node, src: &[u8]) -> Option<String> {
    let declarator = ancestors(call).find(|node| node.kind() == "variable_declarator")?;
    declarator
        .child_by_field_name("name")
        .map(|name| text(name, src))
}

fn analyze_drizzle_table(
    analysis: &mut OrmAnalysis,
    path: &str,
    call: Node,
    table_name: Node,
    columns: Node,
    src: &[u8],
) {
    let Some(table) = string_literal(table_name, src) else {
        return;
    };
    let Some(model) = variable_model(call, src) else {
        return;
    };
    add_model(
        analysis,
        "drizzle",
        &model,
        &table,
        path,
        call.start_position().row as u32 + 1,
        StaticSource::TreeSitter,
    );
    let mut cursor = columns.walk();
    for pair in columns.named_children(&mut cursor) {
        if pair.kind() != "pair" {
            continue;
        }
        let Some(value) = pair.child_by_field_name("value") else {
            continue;
        };
        let Some(arguments) = value.child_by_field_name("arguments") else {
            continue;
        };
        let mut arguments_cursor = arguments.walk();
        let column = arguments
            .named_children(&mut arguments_cursor)
            .find_map(|argument| string_literal(argument, src));
        if let Some(column) = column {
            add_column(
                analysis,
                &table,
                &column,
                path,
                pair.start_position().row as u32 + 1,
                StaticSource::TreeSitter,
            );
        } else {
            analysis
                .unresolved
                .push(format!("{path}: dynamic column on {model}"));
        }
    }
}

fn analyze_drizzle_file(
    analysis: &mut OrmAnalysis,
    path: &str,
    content: &str,
    patterns: &Patterns,
    lang: Lang,
) {
    let bucket = match lang {
        Lang::Ts => "ts",
        Lang::Tsx => "tsx",
        _ => return,
    };
    let key = format!("{bucket}/drizzle-orm-model.scm");
    let Ok(queries) = patterns.compiled_queries() else {
        return;
    };
    let Some((_, _, _, body)) = queries
        .iter()
        .find(|(candidate, _, _, _)| *candidate == key)
    else {
        return;
    };
    let Ok(query) = Query::new(&language_for(lang), body) else {
        return;
    };
    let Some(table_index) = capture_index(&query, "table") else {
        return;
    };
    let Some(name_index) = capture_index(&query, "table_name") else {
        return;
    };
    let Some(columns_index) = capture_index(&query, "columns") else {
        return;
    };
    let Some(tree) = parse_for_lang(lang, content) else {
        return;
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
    while let Some(matched) = matches.next() {
        let table = matched
            .captures
            .iter()
            .find(|capture| capture.index == table_index);
        let name = matched
            .captures
            .iter()
            .find(|capture| capture.index == name_index);
        let columns = matched
            .captures
            .iter()
            .find(|capture| capture.index == columns_index);
        if let (Some(table), Some(name), Some(columns)) = (table, name, columns) {
            analyze_drizzle_table(
                analysis,
                path,
                table.node,
                name.node,
                columns.node,
                content.as_bytes(),
            );
        }
    }
}

pub(super) fn analyze_typeorm(
    analysis: &mut OrmAnalysis,
    path: &str,
    content: &str,
    patterns: &Patterns,
    lang: Lang,
) {
    analyze_typeorm_file(analysis, path, content, patterns, lang);
}

pub(super) fn analyze_drizzle(
    analysis: &mut OrmAnalysis,
    path: &str,
    content: &str,
    patterns: &Patterns,
    lang: Lang,
) {
    analyze_drizzle_file(analysis, path, content, patterns, lang);
}
