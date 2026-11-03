use super::django::{capture_index, text};
use super::typescript::string_literal;
use super::{add_column, add_model, OrmAnalysis};
use crate::schema::StaticSource;
use crate::server::sensors::patterns::Patterns;
use crate::server::sensors::util::{language_for, parse_for_lang, Lang};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

fn argument_at(arguments: Node, index: usize) -> Option<Node> {
    let mut cursor = arguments.walk();
    let found = arguments.named_children(&mut cursor).nth(index);
    found
}

fn object_option(object: Node, name: &str, src: &[u8]) -> Option<Option<String>> {
    if object.kind() != "object" {
        return None;
    }
    let mut cursor = object.walk();
    for pair in object.named_children(&mut cursor) {
        if pair.kind() != "pair" {
            continue;
        }
        let key = pair.child_by_field_name("key")?;
        if text(key, src) != name {
            continue;
        }
        let value = pair.child_by_field_name("value")?;
        return Some(string_literal(value, src));
    }
    None
}

fn analyze_definition(
    analysis: &mut OrmAnalysis,
    path: &str,
    call: Node,
    model: &str,
    attributes: Node,
    options: Node,
    src: &[u8],
) {
    let Some(table) = object_option(options, "tableName", src) else {
        analysis
            .unresolved
            .push(format!("{path}: implicit table name on {model}"));
        return;
    };
    let Some(table) = table else {
        analysis
            .unresolved
            .push(format!("{path}: dynamic table name on {model}"));
        return;
    };
    add_model(
        analysis,
        "sequelize",
        model,
        &table,
        path,
        call.start_position().row as u32 + 1,
        StaticSource::TreeSitter,
    );
    let mut cursor = attributes.walk();
    for pair in attributes.named_children(&mut cursor) {
        if pair.kind() != "pair" {
            continue;
        }
        let Some(key) = pair.child_by_field_name("key") else {
            continue;
        };
        let attribute = text(key, src);
        let Some(options) = pair.child_by_field_name("value") else {
            continue;
        };
        let column = object_option(options, "column", src)
            .map_or_else(|| Some(attribute.clone()), |name| name);
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

pub(super) fn analyze_sequelize(
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
        .find(|(key, _, _, _)| *key == "tsjs/sequelize-orm-model.scm")
    else {
        return;
    };
    let Ok(query) = Query::new(&language_for(Lang::TsJs), body) else {
        return;
    };
    let Some(call_index) = capture_index(&query, "definition") else {
        return;
    };
    let Some(callee_index) = capture_index(&query, "callee") else {
        return;
    };
    let Some(arguments_index) = capture_index(&query, "arguments") else {
        return;
    };
    let Some(tree) = parse_for_lang(Lang::TsJs, content) else {
        return;
    };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
    while let Some(matched) = matches.next() {
        let call = matched
            .captures
            .iter()
            .find(|capture| capture.index == call_index);
        let callee = matched
            .captures
            .iter()
            .find(|capture| capture.index == callee_index);
        let arguments = matched
            .captures
            .iter()
            .find(|capture| capture.index == arguments_index);
        let (Some(call), Some(callee), Some(arguments)) = (call, callee, arguments) else {
            continue;
        };
        let src = content.as_bytes();
        let callee_text = text(callee.node, src);
        if callee_text == "sequelize.define" {
            let Some(model_node) = argument_at(arguments.node, 0) else {
                continue;
            };
            let Some(model) = string_literal(model_node, src) else {
                continue;
            };
            let (Some(attributes), Some(options)) = (
                argument_at(arguments.node, 1),
                argument_at(arguments.node, 2),
            ) else {
                continue;
            };
            analyze_definition(analysis, path, call.node, &model, attributes, options, src);
            continue;
        }
        let Some(property) = callee.node.child_by_field_name("property") else {
            continue;
        };
        if text(property, src) != "init" {
            continue;
        }
        let Some(receiver) = callee.node.child_by_field_name("object") else {
            continue;
        };
        if receiver.kind() != "identifier" {
            continue;
        }
        let model = text(receiver, src);
        let (Some(attributes), Some(options)) = (
            argument_at(arguments.node, 0),
            argument_at(arguments.node, 1),
        ) else {
            continue;
        };
        analyze_definition(analysis, path, call.node, &model, attributes, options, src);
    }
}
