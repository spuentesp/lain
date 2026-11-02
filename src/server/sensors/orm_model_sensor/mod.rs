pub(crate) mod django;
mod python;

use crate::error::LainError;
use crate::federation::contracts::coverage::UnresolvedRecord;
use crate::federation::contracts::model::{
    ContractFact, OrmColumnFact, OrmModelFact, OrmTableFact, SourceSite, UnresolvedReason,
};
use crate::graph::{graph_path, sensor_owner_of, GraphDatabase, SensorOwner};
use crate::schema::{
    EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace, StaticSource,
};
use crate::server::sensors::patterns::{OrmModelShapes, Patterns};
use regex::Regex;
use std::collections::BTreeMap;

fn pattern(shapes: &OrmModelShapes, key: &str, fallback: &str) -> Regex {
    let source = shapes
        .patterns
        .get(key)
        .map(String::as_str)
        .unwrap_or(fallback);
    Regex::new(source).unwrap_or_else(|e| panic!("invalid ORM pattern {key}: {e}"))
}
use std::path::Path;

#[derive(Debug, Clone)]
pub struct OrmModel {
    pub name: String,
    pub table: String,
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct OrmColumn {
    pub name: String,
    pub table: String,
    pub path: String,
    pub line: u32,
    pub migration: Option<SourceSite>,
}

#[derive(Debug, Clone)]
pub struct OrmRelation {
    pub edge_type: EdgeType,
    pub from: String,
    pub to: String,
    pub site: SourceSite,
    pub source: StaticSource,
}

#[derive(Debug, Default)]
pub struct OrmAnalysis {
    pub models: Vec<OrmModel>,
    pub columns: Vec<OrmColumn>,
    pub relations: Vec<OrmRelation>,
    pub unresolved: Vec<String>,
}

pub struct OrmModelSensor;
crate::server::sensors::register_sensor!(
    OrmModelSensor,
    "orm_models",
    SqlTables,
    1,
    scan_workspace_orm_models
);

fn line_of(text: &str, offset: usize) -> u32 {
    text[..offset].bytes().filter(|byte| *byte == b'\n').count() as u32 + 1
}

fn site(path: &str, line: u32) -> SourceSite {
    SourceSite {
        path: path.to_string(),
        line,
    }
}

fn must_materialize(graph: &GraphDatabase, id: &str) -> Result<bool, LainError> {
    Ok(match graph.get_node(id)? {
        Some(node) => sensor_owner_of(&node) == Some(SensorOwner::OrmModelSensor),
        None => true,
    })
}

fn table_name(model: &str) -> String {
    model.to_lowercase()
}

fn add_model(
    analysis: &mut OrmAnalysis,
    orm: &str,
    name: &str,
    table: &str,
    path: &str,
    line: u32,
    source: StaticSource,
) {
    let model = OrmModel {
        name: name.to_string(),
        table: table.to_string(),
        path: path.to_string(),
        line,
    };
    analysis.relations.push(OrmRelation {
        edge_type: EdgeType::MapsTo,
        from: name.to_string(),
        to: table.to_string(),
        site: site(path, line),
        source,
    });
    analysis.models.push(model);
    let _ = orm;
}

fn add_column(
    analysis: &mut OrmAnalysis,
    table: &str,
    name: &str,
    path: &str,
    line: u32,
    source: StaticSource,
) {
    analysis.columns.push(OrmColumn {
        name: name.to_string(),
        table: table.to_string(),
        path: path.to_string(),
        line,
        migration: None,
    });
    analysis.relations.push(OrmRelation {
        edge_type: EdgeType::HasColumn,
        from: table.to_string(),
        to: name.to_string(),
        site: site(path, line),
        source,
    });
}

fn analyze_sqlalchemy(
    analysis: &mut OrmAnalysis,
    path: &str,
    content: &str,
    shapes: &OrmModelShapes,
) {
    let class_re = pattern(
        shapes,
        "sqlalchemy_class",
        r#"(?m)^class\s+(\w+)\([^\)]*\):[ \t]*$"#,
    );
    let table_re = pattern(
        shapes,
        "sqlalchemy_table",
        r#"__tablename__\s*=\s*['\"]([^'\"]+)['\"]"#,
    );
    let field_re = pattern(
        shapes,
        "sqlalchemy_field",
        r#"(?m)^\s*(\w+)\s*=\s*Column\("#,
    );
    let column_re = pattern(shapes, "sqlalchemy_column", r#"['\"]([^'\"]+)['\"]"#);
    let classes: Vec<_> = class_re.captures_iter(content).collect();
    for (index, class) in classes.iter().enumerate() {
        let model = class.get(1).unwrap();
        let body_start = class.get(0).unwrap().end();
        let body_end = classes
            .get(index + 1)
            .and_then(|next| next.get(0))
            .map(|next| next.start())
            .unwrap_or(content.len());
        let body = &content[body_start..body_end];
        let Some(table) = table_re.captures(body).and_then(|m| m.get(1)) else {
            continue;
        };
        add_model(
            analysis,
            "sqlalchemy",
            model.as_str(),
            table.as_str(),
            path,
            line_of(content, model.start()),
            StaticSource::Regex,
        );
        for field in field_re.captures_iter(body) {
            let attribute = field.get(1).unwrap();
            let open = field.get(0).unwrap().end() - 1;
            let Some((args, _)) = paren_region(body, open) else {
                continue;
            };
            let literal = column_re.captures(args).and_then(|m| m.get(1));
            let first = args.split(',').next().unwrap_or("").trim();
            let dynamic = literal.is_none()
                && !first.is_empty()
                && (first.starts_with("name=")
                    || first.chars().next().is_some_and(char::is_lowercase));
            if dynamic {
                analysis
                    .unresolved
                    .push(format!("{path}: dynamic column on {}", model.as_str()));
                continue;
            }
            let name = literal.map(|m| m.as_str()).unwrap_or(attribute.as_str());
            add_column(
                analysis,
                table.as_str(),
                name,
                path,
                line_of(content, body_start + attribute.start()),
                StaticSource::Regex,
            );
        }
    }
}

fn analyze_prisma(analysis: &mut OrmAnalysis, path: &str, content: &str, shapes: &OrmModelShapes) {
    let model_re = pattern(shapes, "prisma_model", r#"model\s+(\w+)\s*\{([\s\S]*?)\}"#);
    let table_re = pattern(shapes, "prisma_table", r#"@@map\(\"([^\"]+)\"\)"#);
    let map_re = pattern(shapes, "prisma_column", r#"@map\(\"([^\"]+)\"\)"#);
    for model in model_re.captures_iter(content) {
        let name = model.get(1).unwrap();
        let body = model.get(2).unwrap().as_str();
        let table = table_re
            .captures(body)
            .and_then(|m| m.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| name.as_str().to_string());
        add_model(
            analysis,
            "prisma",
            name.as_str(),
            &table,
            path,
            line_of(content, name.start()),
            StaticSource::Regex,
        );
        for (offset, line) in body.lines().enumerate() {
            let trimmed = line.trim();
            let mut parts = trimmed.split_whitespace();
            let Some(field) = parts.next() else { continue };
            if trimmed.starts_with("//") || field.starts_with('@') || parts.next().is_none() {
                continue;
            }
            let column = map_re
                .captures(trimmed)
                .and_then(|m| m.get(1))
                .map(|m| m.as_str())
                .unwrap_or(field);
            add_column(
                analysis,
                &table,
                column,
                path,
                line_of(content, model.get(0).unwrap().start()) + offset as u32 + 1,
                StaticSource::Regex,
            );
        }
    }
}

fn analyze_drizzle(analysis: &mut OrmAnalysis, path: &str, content: &str, shapes: &OrmModelShapes) {
    let table_re = pattern(
        shapes,
        "drizzle_table",
        r#"(?:const|let|var)\s+(\w+)\s*=\s*(?:pgTable|mysqlTable|sqliteTable)\(\s*['\"]([^'\"]+)['\"]\s*,\s*\{([\s\S]*?)\}\s*\)"#,
    );
    let column_re = pattern(
        shapes,
        "drizzle_column",
        r#"(?m)^\s*(\w+)\s*:\s*\w+\(\s*['\"]([^'\"]+)['\"]"#,
    );
    for table in table_re.captures_iter(content) {
        let model = table.get(1).unwrap();
        let table_name = table.get(2).unwrap().as_str();
        let body = table.get(3).unwrap().as_str();
        add_model(
            analysis,
            "drizzle",
            model.as_str(),
            table_name,
            path,
            line_of(content, model.start()),
            StaticSource::Regex,
        );
        for column in column_re.captures_iter(body) {
            let _property = column.get(1).unwrap();
            let name = column.get(2).unwrap().as_str();
            add_column(
                analysis,
                table_name,
                name,
                path,
                line_of(
                    content,
                    table.get(3).unwrap().start() + column.get(0).unwrap().start(),
                ),
                StaticSource::Regex,
            );
        }
    }
}

fn analyze_typeorm(analysis: &mut OrmAnalysis, path: &str, content: &str, shapes: &OrmModelShapes) {
    let entity_re = pattern(
        shapes,
        "typeorm_entity",
        r#"@Entity\(([^\)]*)\)\s*export\s+class\s+(\w+)"#,
    );
    let name_re = pattern(shapes, "typeorm_name", r#"name\s*:\s*['\"]([^'\"]+)['\"]"#);
    let column_re = pattern(shapes, "typeorm_column", r#"@Column\("#);
    let property_re = pattern(shapes, "typeorm_property", r#"(?m)^\s*(\w+)"#);
    let entities: Vec<_> = entity_re.captures_iter(content).collect();
    for (index, entity) in entities.iter().enumerate() {
        let options = entity.get(1).unwrap().as_str();
        let model = entity.get(2).unwrap().as_str();
        let table = name_re
            .captures(options)
            .and_then(|m| m.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| table_name(model));
        add_model(
            analysis,
            "typeorm",
            model,
            &table,
            path,
            line_of(content, entity.get(0).unwrap().start()),
            StaticSource::Regex,
        );
        let body_start = entity.get(0).unwrap().end();
        let body_end = entities
            .get(index + 1)
            .and_then(|next| next.get(0))
            .map(|next| next.start())
            .unwrap_or(content.len());
        let body = &content[body_start..body_end];
        for column in column_re.captures_iter(body) {
            let open = column.get(0).unwrap().end() - 1;
            let Some((options, closing)) = paren_region(body, open) else {
                continue;
            };
            let Some(property_match) = property_re.captures(&body[closing + 1..]) else {
                continue;
            };
            let property = property_match.get(1).unwrap().as_str();
            let literal = name_re.captures(options).and_then(|m| m.get(1));
            if options.contains("name") && literal.is_none() {
                analysis
                    .unresolved
                    .push(format!("{path}: dynamic column on {model}"));
                continue;
            }
            let name = literal.map(|m| m.as_str()).unwrap_or(property);
            add_column(
                analysis,
                &table,
                name,
                path,
                line_of(content, body_start + column.get(0).unwrap().start()),
                StaticSource::Regex,
            );
        }
    }
}

fn braced_region(text: &str, open: usize) -> Option<(&str, usize)> {
    if text.as_bytes().get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in text[open..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            continue;
        }
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((&text[open + 1..open + offset], open + offset));
                }
            }
            _ => {}
        }
    }
    None
}

fn paren_region(text: &str, open: usize) -> Option<(&str, usize)> {
    if text.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in text[open..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((&text[open + 1..open + offset], open + offset));
                }
            }
            _ => {}
        }
    }
    None
}

fn following_braced_region(text: &str, closing: usize) -> Option<(&str, usize)> {
    let open = text[closing + 1..].find('{')? + closing + 1;
    braced_region(text, open)
}

fn analyze_sequelize_definition(
    analysis: &mut OrmAnalysis,
    path: &str,
    model: &str,
    body: &str,
    options: &str,
    definition_start: usize,
    content: &str,
    table_re: &Regex,
    column_re: &Regex,
    property_re: &Regex,
) {
    let literal_table = table_re.captures(options).and_then(|m| m.get(1));
    if options.contains("tableName") && literal_table.is_none() {
        analysis
            .unresolved
            .push(format!("{path}: dynamic table name on {model}"));
        return;
    }
    let table = literal_table
        .map(|name| name.as_str().to_string())
        .unwrap_or_else(|| format!("{model}s"));
    add_model(
        analysis,
        "sequelize",
        model,
        &table,
        path,
        line_of(content, definition_start),
        StaticSource::Regex,
    );
    for property in property_re.captures_iter(body) {
        let name = property.get(1).unwrap();
        let open = property.get(0).unwrap().end() - 1;
        let Some((property_options, _)) = braced_region(body, open) else {
            continue;
        };
        let literal = column_re.captures(property_options).and_then(|m| m.get(1));
        if property_options.contains("column") && literal.is_none() {
            analysis
                .unresolved
                .push(format!("{path}: dynamic column on {model}"));
            continue;
        }
        let column = literal.map(|m| m.as_str()).unwrap_or(name.as_str());
        add_column(
            analysis,
            &table,
            column,
            path,
            line_of(content, definition_start + name.start()),
            StaticSource::Regex,
        );
    }
}

fn analyze_sequelize(
    analysis: &mut OrmAnalysis,
    path: &str,
    content: &str,
    shapes: &OrmModelShapes,
) {
    let define_re = pattern(
        shapes,
        "sequelize_define",
        r#"sequelize\.define\(\s*['\"](\w+)['\"]\s*,\s*\{"#,
    );
    let init_re = pattern(shapes, "sequelize_init", r#"(?m)(\w+)\.init\(\s*\{"#);
    let table_re = pattern(
        shapes,
        "sequelize_table",
        r#"tableName\s*:\s*['\"]([^'\"]+)['\"]"#,
    );
    let column_re = pattern(
        shapes,
        "sequelize_column",
        r#"column\s*:\s*['\"]([^'\"]+)['\"]"#,
    );
    let property_re = pattern(shapes, "sequelize_property", r#"(?m)^\s*(\w+)\s*:\s*\{"#);
    for definition in define_re.captures_iter(content) {
        let model = definition.get(1).unwrap();
        let open = definition.get(0).unwrap().end() - 1;
        let Some((body, closing)) = braced_region(content, open) else {
            continue;
        };
        let Some((options, _)) = following_braced_region(content, closing) else {
            continue;
        };
        analyze_sequelize_definition(
            analysis,
            path,
            model.as_str(),
            body,
            options,
            definition.get(0).unwrap().start(),
            content,
            &table_re,
            &column_re,
            &property_re,
        );
    }
    for definition in init_re.captures_iter(content) {
        let model = definition.get(1).unwrap();
        let open = definition.get(0).unwrap().end() - 1;
        let Some((body, closing)) = braced_region(content, open) else {
            continue;
        };
        let Some((options, _)) = following_braced_region(content, closing) else {
            continue;
        };
        analyze_sequelize_definition(
            analysis,
            path,
            model.as_str(),
            body,
            options,
            definition.get(0).unwrap().start(),
            content,
            &table_re,
            &column_re,
            &property_re,
        );
    }
}

fn contains_identifier(content: &str, name: &str) -> bool {
    content.match_indices(name).any(|(offset, _)| {
        let before = content[..offset].chars().next_back();
        let after = content[offset + name.len()..].chars().next();
        !before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
            && !after.is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
    })
}

fn attach_migrations(root: &Path, analysis: &mut OrmAnalysis) {
    let model_tables: BTreeMap<String, String> = analysis
        .models
        .iter()
        .map(|model| (model.name.to_lowercase(), model.table.clone()))
        .collect();
    for (path, content, _) in crate::server::sensors::util::scan_files(root, |path| {
        let normalized = graph_path(root, path);
        let is_migration = normalized.contains("migration") || normalized.contains("alembic");
        (is_migration
            && matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("py" | "sql" | "ts" | "js")
            ))
        .then_some(())
    }) {
        let path = graph_path(root, &path);
        for column in &mut analysis.columns {
            let found_table = model_tables
                .values()
                .any(|table| table == &column.table && contains_identifier(&content, table));
            let sql_alter = found_table
                && (content.contains("ADD COLUMN") || content.contains("add_column"))
                && contains_identifier(&content, &column.name);
            let found_django_model = model_tables.iter().any(|(model, table)| {
                table == &column.table
                    && (contains_identifier(&content, model)
                        || contains_identifier(&content, table))
            });
            let django_add = content.contains("AddField")
                && contains_identifier(&content, &column.name)
                && found_django_model;
            if sql_alter || django_add {
                let offset = content.find(&column.name).unwrap_or(0);
                column.migration = Some(site(&path, line_of(&content, offset)));
            }
        }
    }
}

pub fn analyze_orm_models(root: &Path) -> OrmAnalysis {
    let patterns = Patterns::with_overrides(root).unwrap_or_else(|e| {
        tracing::warn!("orm_model_sensor: override load failed ({e}); using bundled patterns");
        std::borrow::Cow::Borrowed(Patterns::patterns())
    });
    let shapes = patterns.orm_model_shapes();
    let django_models = django::DjangoModelMap::load(root, patterns.django_orm_shapes());
    let mut analysis = OrmAnalysis::default();
    for (path, content, extension) in crate::server::sensors::util::scan_files(root, |path| {
        path.extension()
            .and_then(|e| e.to_str())
            .filter(|extension| matches!(*extension, "py" | "prisma" | "ts" | "js" | "tsx" | "jsx"))
            .map(|extension| extension.to_string())
    }) {
        let path = graph_path(root, &path);
        match extension.as_str() {
            "py" => {
                analyze_sqlalchemy(&mut analysis, &path, &content, shapes);
            }
            "prisma" => analyze_prisma(&mut analysis, &path, &content, shapes),
            "ts" | "tsx" => {
                analyze_drizzle(&mut analysis, &path, &content, shapes);
                analyze_typeorm(&mut analysis, &path, &content, shapes);
            }
            "js" | "jsx" => analyze_sequelize(&mut analysis, &path, &content, shapes),
            _ => {}
        }
        if content.contains("process.env") || content.contains("os.environ") {
            analysis
                .unresolved
                .push(format!("{path}: dynamic ORM definition"));
        }
    }
    python::add_django(&mut analysis, &django_models);
    attach_migrations(root, &mut analysis);
    analysis
        .models
        .sort_by(|a, b| (&a.name, &a.table, &a.path).cmp(&(&b.name, &b.table, &b.path)));
    analysis
        .models
        .dedup_by(|a, b| a.name == b.name && a.table == b.table && a.path == b.path);
    analysis
        .columns
        .sort_by(|a, b| (&a.table, &a.name, &a.path).cmp(&(&b.table, &b.name, &b.path)));
    analysis
        .columns
        .dedup_by(|a, b| a.table == b.table && a.name == b.name && a.path == b.path);
    analysis.relations.sort_by(|a, b| {
        (a.from.clone(), a.to.clone(), a.edge_type.to_string()).cmp(&(
            b.from.clone(),
            b.to.clone(),
            b.edge_type.to_string(),
        ))
    });
    analysis
        .relations
        .dedup_by(|a, b| a.from == b.from && a.to == b.to && a.edge_type == b.edge_type);
    analysis.unresolved.sort();
    analysis.unresolved.dedup();
    analysis
}

pub fn scan_workspace_orm_models(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    scan_workspace_orm_models_with_report(graph, root, namespace).map(|report| report.emitted)
}

pub fn scan_workspace_orm_models_with_report(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<crate::server::sensors::ScanReport, LainError> {
    if graph.is_read_only() {
        return Ok(crate::server::sensors::ScanReport::default());
    }
    let analysis = analyze_orm_models(root);
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut model_ids = BTreeMap::new();
    let mut table_ids = BTreeMap::new();
    let mut column_ids = BTreeMap::new();
    let existing_nodes = graph.get_all_nodes();
    for model in &analysis.models {
        let id = existing_nodes
            .iter()
            .find(|node| {
                node.node_type == NodeType::Class
                    && node.path == model.path
                    && node.name == model.name
            })
            .map(|node| node.id.clone())
            .unwrap_or_else(|| {
                GraphNode::generate_id(
                    &NodeType::Class,
                    &model.path,
                    &model.name,
                    Some(model.line),
                    namespace,
                )
            });
        model_ids.insert((model.path.clone(), model.name.clone()), id.clone());
        table_ids.insert(
            (model.path.clone(), model.table.clone()),
            GraphNode::generate_id(&NodeType::Table, &model.path, &model.table, None, namespace),
        );
        if must_materialize(graph, &id)? {
            let mut node = GraphNode::new_in(
                NodeType::Class,
                model.name.clone(),
                model.path.clone(),
                namespace,
            );
            node.id = id;
            node.line_start = Some(model.line);
            node.line_end = Some(model.line);
            node.contract = vec![ContractFact::OrmModel(OrmModelFact {
                orm: "declared".into(),
                model: model.name.clone(),
                table: model.table.clone(),
            })];
            nodes.push(node);
        }
        let table_id = table_ids[&(model.path.clone(), model.table.clone())].clone();
        if must_materialize(graph, &table_id)? {
            let mut node = GraphNode::new_in(
                NodeType::Table,
                model.table.clone(),
                model.path.clone(),
                namespace,
            );
            node.id = table_id;
            node.line_start = Some(model.line);
            node.contract = vec![ContractFact::OrmTable(OrmTableFact {
                orm: "declared".into(),
                model: model.name.clone(),
                table: model.table.clone(),
            })];
            nodes.push(node);
        }
    }
    for column in &analysis.columns {
        let id = GraphNode::generate_id(
            &NodeType::Column,
            &column.path,
            &column.name,
            Some(column.line),
            namespace,
        );
        column_ids.insert(
            (
                column.path.clone(),
                column.table.clone(),
                column.name.clone(),
            ),
            id.clone(),
        );
        if must_materialize(graph, &id)? {
            let mut node = GraphNode::new_in(
                NodeType::Column,
                column.name.clone(),
                column.path.clone(),
                namespace,
            );
            node.id = id;
            node.line_start = Some(column.line);
            node.line_end = column.migration.as_ref().map(|_| column.line);
            node.contract = vec![ContractFact::OrmColumn(OrmColumnFact {
                orm: "declared".into(),
                table: column.table.clone(),
                column: column.name.clone(),
                migration: column.migration.clone(),
            })];
            nodes.push(node);
        }
    }
    for relation in &analysis.relations {
        let (from, to) = match relation.edge_type {
            EdgeType::MapsTo => (
                model_ids
                    .get(&(relation.site.path.clone(), relation.from.clone()))
                    .cloned(),
                table_ids
                    .get(&(relation.site.path.clone(), relation.to.clone()))
                    .cloned(),
            ),
            EdgeType::HasColumn => (
                table_ids
                    .get(&(relation.site.path.clone(), relation.from.clone()))
                    .cloned(),
                column_ids
                    .get(&(
                        relation.site.path.clone(),
                        relation.from.clone(),
                        relation.to.clone(),
                    ))
                    .cloned(),
            ),
            _ => continue,
        };
        let (Some(from), Some(to)) = (from, to) else {
            continue;
        };
        let mut edge = GraphEdge::new(relation.edge_type.clone(), from, to);
        edge.site = Some(relation.site.clone());
        edge.provenance = Some(EdgeProvenance::Static {
            source: relation.source,
        });
        edges.push(edge);
    }
    let emitted = nodes.len();
    graph.replace_sensor_output(SensorOwner::OrmModelSensor, &nodes, &edges)?;
    let unresolved = if analysis.unresolved.is_empty() {
        Vec::new()
    } else {
        vec![UnresolvedRecord {
            reason: UnresolvedReason::OrmDynamicQuery,
            count: analysis.unresolved.len(),
            sample_ids: analysis.unresolved.iter().take(5).cloned().collect(),
        }]
    };
    Ok(crate::server::sensors::ScanReport {
        emitted,
        error: None,
        unresolved,
    })
}
