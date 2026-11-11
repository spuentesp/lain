use crate::server::sensors::patterns::{DjangoOrmShapes, Patterns};
use crate::server::sensors::util::{language_for, parse_for_lang, Lang};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

#[derive(Debug, Clone)]
pub(crate) enum DbTable {
    Default,
    Literal(String),
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TableResolution {
    Table(String),
    Abstract,
    Unresolved(&'static str),
}

pub(crate) type TableRes = TableResolution;
pub(crate) type ModelMap = DjangoModelMap;

#[derive(Debug, Clone)]
pub(crate) struct DjangoField {
    pub attribute: String,
    pub column: Option<String>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct DjangoModel {
    pub name: String,
    pub path: String,
    pub line: u32,
    pub end_line: u32,
    pub bases: Vec<String>,
    pub abstract_: bool,
    pub proxy: bool,
    pub db_table: DbTable,
    pub app_label: Option<String>,
    pub app_label_dynamic: bool,
    pub fields: Vec<DjangoField>,
}

#[derive(Debug)]
pub(crate) struct DjangoModelMap {
    pub models: Vec<DjangoModel>,
    pub by_name: BTreeMap<String, Vec<usize>>,
    pub is_model: Vec<bool>,
}

pub(super) fn text(node: Node, src: &[u8]) -> String {
    node.utf8_text(src).unwrap_or("").to_string()
}

pub(super) fn string_literal(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if current.kind() == "interpolation" {
            return None;
        }
        if current.kind() == "string_content" {
            return Some(text(current, src));
        }
        let mut cursor = current.walk();
        pending.extend(current.named_children(&mut cursor));
    }
    None
}

pub(super) fn assignment(node: Node) -> Option<(Node, Node)> {
    let expression = (node.kind() == "expression_statement").then_some(node)?;
    let assignment = expression.named_child(0)?;
    (assignment.kind() == "assignment").then_some(assignment)?;
    Some((
        assignment.child_by_field_name("left")?,
        assignment.child_by_field_name("right")?,
    ))
}

fn base_names(node: Node, src: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|base| {
            matches!(base.kind(), "identifier" | "attribute" | "subscript")
                .then(|| text(base, src))
                .map(|name| {
                    name.split('[')
                        .next()
                        .unwrap_or("")
                        .rsplit('.')
                        .next()
                        .unwrap_or("")
                        .to_string()
                })
                .filter(|name| !name.is_empty())
        })
        .collect()
}

fn parse_meta(model: &mut DjangoModel, class_body: Node, src: &[u8]) {
    let mut cursor = class_body.walk();
    for statement in class_body.named_children(&mut cursor) {
        let Some((left, right)) = assignment(statement) else {
            continue;
        };
        if left.kind() != "identifier" {
            continue;
        }
        match text(left, src).as_str() {
            "abstract" => model.abstract_ = text(right, src) == "True",
            "proxy" => model.proxy = text(right, src) == "True",
            "db_table" => {
                model.db_table =
                    string_literal(right, src).map_or(DbTable::Dynamic, DbTable::Literal);
            }
            "app_label" => {
                model.app_label = string_literal(right, src);
                model.app_label_dynamic = model.app_label.is_none();
            }
            _ => {}
        }
    }
}

fn dynamic_column(call: Node, src: &[u8]) -> Option<Option<String>> {
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    for argument in arguments.named_children(&mut cursor) {
        if argument.kind() != "keyword_argument" {
            continue;
        }
        let name = argument.child_by_field_name("name")?;
        if text(name, src) != "db_column" {
            continue;
        }
        let value = argument.child_by_field_name("value")?;
        return Some(string_literal(value, src));
    }
    None
}

fn parse_field(statement: Node, src: &[u8], suffixes: &[String]) -> Option<DjangoField> {
    let (left, right) = assignment(statement)?;
    if left.kind() != "identifier" || right.kind() != "call" {
        return None;
    }
    let function = right.child_by_field_name("function")?;
    let function_name = text(function, src);
    let is_field = function_name.starts_with("models.")
        && suffixes
            .iter()
            .any(|suffix| function_name.ends_with(suffix));
    if !is_field {
        return None;
    }
    let attribute = text(left, src);
    let column = dynamic_column(right, src).unwrap_or_else(|| Some(attribute.clone()));
    Some(DjangoField {
        attribute,
        column,
        line: statement.start_position().row as u32 + 1,
    })
}

fn parse_class(
    path: &str,
    class: Node,
    name: Node,
    bases: Node,
    body: Node,
    src: &[u8],
    patterns: &Patterns,
) -> DjangoModel {
    let mut model = DjangoModel {
        name: text(name, src),
        path: path.to_string(),
        line: class.start_position().row as u32 + 1,
        end_line: class.end_position().row as u32 + 1,
        bases: base_names(bases, src),
        abstract_: false,
        proxy: false,
        db_table: DbTable::Default,
        app_label: None,
        app_label_dynamic: false,
        fields: Vec::new(),
    };
    let mut cursor = body.walk();
    for statement in body.named_children(&mut cursor) {
        if statement.kind() == "class_definition" {
            let nested_name = statement.child_by_field_name("name");
            if nested_name.is_some_and(|node| text(node, src) == "Meta") {
                if let Some(meta_body) = statement.child_by_field_name("body") {
                    parse_meta(&mut model, meta_body, src);
                }
            }
            continue;
        }
        if let Some(field) = parse_field(
            statement,
            src,
            &patterns.orm_model_shapes().django_field_suffixes,
        ) {
            model.fields.push(field);
        }
    }
    model
}

pub(super) fn capture_index(query: &Query, name: &str) -> Option<u32> {
    query
        .capture_names()
        .iter()
        .position(|capture| *capture == name)
        .map(|index| index as u32)
}

fn collect_ast_classes(
    root: &Path,
    shapes: &DjangoOrmShapes,
    patterns: &Patterns,
) -> Vec<DjangoModel> {
    let Ok(queries) = patterns.compiled_queries() else {
        return Vec::new();
    };
    let Some((_, _, _, body)) = queries
        .iter()
        .find(|(key, _, _, _)| *key == "python/django-orm-model.scm")
    else {
        return Vec::new();
    };
    let Ok(query) = Query::new(&language_for(Lang::Python), body) else {
        return Vec::new();
    };
    let Some(class_index) = capture_index(&query, "class") else {
        return Vec::new();
    };
    let Some(name_index) = capture_index(&query, "model") else {
        return Vec::new();
    };
    let Some(bases_index) = capture_index(&query, "bases") else {
        return Vec::new();
    };
    let Some(body_index) = capture_index(&query, "body") else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for (file, content, ()) in crate::server::sensors::util::scan_files(root, |path| {
        (path.extension().and_then(|extension| extension.to_str()) == Some("py")).then_some(())
    }) {
        let path = crate::graph::graph_path(root, &file);
        let Some(tree) = parse_for_lang(Lang::Python, &content) else {
            continue;
        };
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
        while let Some(matched) = matches.next() {
            let class = matched
                .captures
                .iter()
                .find(|capture| capture.index == class_index);
            let name = matched
                .captures
                .iter()
                .find(|capture| capture.index == name_index);
            let bases = matched
                .captures
                .iter()
                .find(|capture| capture.index == bases_index);
            let body = matched
                .captures
                .iter()
                .find(|capture| capture.index == body_index);
            if let (Some(class), Some(name), Some(bases), Some(body)) = (class, name, bases, body) {
                models.push(parse_class(
                    &path,
                    class.node,
                    name.node,
                    bases.node,
                    body.node,
                    content.as_bytes(),
                    patterns,
                ));
            }
        }
    }
    let _ = shapes;
    models
}

fn app_of_path(path: &str) -> String {
    let components: Vec<&str> = path.split('/').collect();
    let directories = &components[..components.len().saturating_sub(1)];
    if let Some(position) = directories
        .iter()
        .rposition(|component| *component == "models")
    {
        if position > 0 {
            return directories[position - 1].to_string();
        }
    }
    directories
        .last()
        .map(|name| name.to_string())
        .unwrap_or_default()
}

impl DjangoModelMap {
    pub(crate) fn load(root: &Path, shapes: &DjangoOrmShapes) -> Self {
        let patterns = Patterns::with_overrides(root).unwrap_or_else(|error| {
            tracing::warn!(
                "django model map: override load failed ({error}); using bundled patterns"
            );
            std::borrow::Cow::Borrowed(Patterns::patterns())
        });
        let classes = collect_ast_classes(root, shapes, &patterns);
        Self::build(classes, shapes)
    }

    fn build(classes: Vec<DjangoModel>, shapes: &DjangoOrmShapes) -> Self {
        let mut by_name = BTreeMap::new();
        for (index, model) in classes.iter().enumerate() {
            by_name
                .entry(model.name.clone())
                .or_insert_with(Vec::new)
                .push(index);
        }
        let mut is_model = vec![false; classes.len()];
        loop {
            let mut changed = false;
            for index in 0..classes.len() {
                if is_model[index] {
                    continue;
                }
                let hit = classes[index].bases.iter().any(|base| {
                    shapes.has_model_base(base)
                        || by_name.get(base).is_some_and(|parents| {
                            parents
                                .iter()
                                .any(|&parent| parent != index && is_model[parent])
                        })
                });
                if hit {
                    is_model[index] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Self {
            models: classes,
            by_name,
            is_model,
        }
    }

    pub(crate) fn app_of(&self, index: usize) -> String {
        let model = &self.models[index];
        model
            .app_label
            .clone()
            .unwrap_or_else(|| app_of_path(&model.path))
    }

    pub(crate) fn is_model(&self, index: usize) -> bool {
        self.is_model[index]
    }

    pub(crate) fn table_of(&self, index: usize) -> TableResolution {
        self.table_of_at_depth(index, 0)
    }

    fn table_of_at_depth(&self, index: usize, depth: u8) -> TableResolution {
        let model = &self.models[index];
        if model.abstract_ {
            return TableResolution::Abstract;
        }
        if model.proxy {
            if depth >= 4 {
                return TableResolution::Unresolved("proxy chain too deep");
            }
            let parents: BTreeSet<TableResolution> = model
                .bases
                .iter()
                .filter_map(|base| self.by_name.get(base))
                .flatten()
                .filter(|&&parent| {
                    parent != index && self.is_model[parent] && !self.models[parent].abstract_
                })
                .map(|&parent| self.table_of_at_depth(parent, depth + 1))
                .collect();
            return match parents.len() {
                1 => parents.into_iter().next().unwrap(),
                _ => TableResolution::Unresolved("proxy parent not unique"),
            };
        }
        match &model.db_table {
            DbTable::Literal(table) => TableResolution::Table(table.clone()),
            DbTable::Dynamic => TableResolution::Unresolved("non-literal db_table"),
            DbTable::Default if model.app_label_dynamic => {
                TableResolution::Unresolved("dynamic app_label")
            }
            DbTable::Default => TableResolution::Table(format!(
                "{}_{}",
                self.app_of(index).to_lowercase(),
                model.name.to_lowercase()
            )),
        }
    }
}
