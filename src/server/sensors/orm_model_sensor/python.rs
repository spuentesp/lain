use super::django::{
    assignment, capture_index, string_literal, text, DjangoModelMap, TableResolution,
};
use super::{add_column, add_model, OrmAnalysis};
use crate::schema::StaticSource;
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::util::{language_for, parse_for_lang, Lang};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

pub(super) fn add_django(analysis: &mut OrmAnalysis, map: &DjangoModelMap) {
    for (index, model) in map.models.iter().enumerate() {
        if !map.is_model(index) {
            continue;
        }
        match map.table_of(index) {
            TableResolution::Table(table) => {
                add_model(
                    analysis,
                    "django",
                    &model.name,
                    &table,
                    &model.path,
                    model.line,
                    StaticSource::TreeSitter,
                );
                for field in &model.fields {
                    if let Some(column) = &field.column {
                        add_column(
                            analysis,
                            &table,
                            column,
                            &model.path,
                            field.line,
                            StaticSource::TreeSitter,
                        );
                    } else {
                        analysis.unresolved.push(format!(
                            "{}:{}: dynamic column {} on {}",
                            model.path, field.line, field.attribute, model.name
                        ));
                    }
                }
            }
            TableResolution::Abstract => {}
            TableResolution::Unresolved(why) => {
                analysis.unresolved.push(format!(
                    "{}:{}: {why} on {}",
                    model.path, model.line, model.name
                ));
            }
        }
    }
}

fn sqlalchemy_column_name(call: Node, attribute: &str, src: &[u8]) -> Option<String> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let mut first_positional = None;
    for argument in arguments.named_children(&mut cursor) {
        if argument.kind() == "keyword_argument" {
            let name = argument.child_by_field_name("name")?;
            if text(name, src) == "name" {
                return string_literal(argument.child_by_field_name("value")?, src);
            }
        } else if first_positional.is_none() {
            first_positional = Some(argument);
        }
    }
    match first_positional {
        Some(node) if node.kind() == "string" => string_literal(node, src),
        Some(node) if node.kind() == "call" => Some(attribute.to_string()),
        Some(_) => None,
        None => Some(attribute.to_string()),
    }
}

fn sqlalchemy_table(body: Node, src: &[u8]) -> Option<Option<String>> {
    let mut cursor = body.walk();
    for statement in body.named_children(&mut cursor) {
        let Some((left, right)) = assignment(statement) else {
            continue;
        };
        if text(left, src) == "__tablename__" {
            return Some(string_literal(right, src));
        }
    }
    None
}

fn analyze_sqlalchemy_class(
    analysis: &mut OrmAnalysis,
    path: &str,
    class: Node,
    model: Node,
    body: Node,
    src: &[u8],
    patterns: &Patterns,
) {
    let model_name = text(model, src);
    let Some(table) = sqlalchemy_table(body, src) else {
        return;
    };
    let Some(table) = table else {
        analysis
            .unresolved
            .push(format!("{path}: dynamic table name on {model_name}"));
        return;
    };
    add_model(
        analysis,
        "sqlalchemy",
        &model_name,
        &table,
        path,
        class.start_position().row as u32 + 1,
        StaticSource::TreeSitter,
    );
    let mut cursor = body.walk();
    for statement in body.named_children(&mut cursor) {
        let Some((left, right)) = assignment(statement) else {
            continue;
        };
        if left.kind() != "identifier" || right.kind() != "call" {
            continue;
        }
        let Some(function) = right.child_by_field_name("function") else {
            continue;
        };
        let function_name = text(function, src);
        if !patterns
            .orm_model_shapes()
            .sqlalchemy_column_calls
            .iter()
            .any(|call| function_name == *call || function_name.ends_with(&format!(".{call}")))
        {
            continue;
        }
        let attribute = text(left, src);
        if let Some(column) = sqlalchemy_column_name(right, &attribute, src) {
            add_column(
                analysis,
                &table,
                &column,
                path,
                statement.start_position().row as u32 + 1,
                StaticSource::TreeSitter,
            );
        } else {
            analysis
                .unresolved
                .push(format!("{path}: dynamic column on {model_name}"));
        }
    }
}

pub(super) fn analyze_sqlalchemy(
    analysis: &mut OrmAnalysis,
    path: &str,
    content: &str,
    patterns: &Patterns,
) {
    let Ok(queries) = patterns.compiled_queries() else {
        return;
    };
    let Some((_, _, _, body)) = queries
        .iter()
        .find(|(key, _, _, _)| *key == "python/sqlalchemy-orm-model.scm")
    else {
        return;
    };
    let Ok(query) = Query::new(&language_for(Lang::Python), body) else {
        return;
    };
    let Some(class_index) = capture_index(&query, "class") else {
        return;
    };
    let Some(model_index) = capture_index(&query, "model") else {
        return;
    };
    let Some(body_index) = capture_index(&query, "body") else {
        return;
    };
    let Some(tree) = parse_for_lang(Lang::Python, content) else {
        return;
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
    while let Some(matched) = matches.next() {
        let class = matched
            .captures
            .iter()
            .find(|capture| capture.index == class_index);
        let model = matched
            .captures
            .iter()
            .find(|capture| capture.index == model_index);
        let body = matched
            .captures
            .iter()
            .find(|capture| capture.index == body_index);
        if let (Some(class), Some(model), Some(body)) = (class, model, body) {
            analyze_sqlalchemy_class(
                analysis,
                path,
                class.node,
                model.node,
                body.node,
                content.as_bytes(),
                patterns,
            );
        }
    }
}
