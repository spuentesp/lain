pub(crate) mod django;
mod javascript;
mod migrations;
mod prisma;
mod python;
mod typescript;

use crate::error::LainError;
use crate::federation::contracts::coverage::UnresolvedRecord;
use crate::federation::contracts::model::{
    ContractFact, OrmColumnFact, OrmModelFact, OrmTableFact, SourceSite, UnresolvedReason,
};
use crate::graph::{graph_path, sensor_owner_of, GraphDatabase, SensorOwner};
use crate::schema::{
    EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace, StaticSource,
};
use crate::server::sensors::patterns::Patterns;
use std::collections::BTreeMap;
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

pub fn analyze_orm_models(root: &Path) -> OrmAnalysis {
    let patterns = Patterns::with_overrides(root).unwrap_or_else(|e| {
        tracing::warn!("orm_model_sensor: override load failed ({e}); using bundled patterns");
        std::borrow::Cow::Borrowed(Patterns::patterns())
    });
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
                python::analyze_sqlalchemy(&mut analysis, &path, &content, &patterns);
            }
            "prisma" => prisma::analyze(&mut analysis, &path, &content),
            "ts" | "tsx" => {
                let lang = if extension == "tsx" {
                    crate::server::sensors::util::Lang::Tsx
                } else {
                    crate::server::sensors::util::Lang::Ts
                };
                typescript::analyze_drizzle(&mut analysis, &path, &content, &patterns, lang);
                typescript::analyze_typeorm(&mut analysis, &path, &content, &patterns, lang);
            }
            "js" | "jsx" => {
                javascript::analyze_sequelize(&mut analysis, &path, &content, &patterns)
            }
            _ => {}
        }
        if content.contains("process.env") || content.contains("os.environ") {
            analysis
                .unresolved
                .push(format!("{path}: dynamic ORM definition"));
        }
    }
    python::add_django(&mut analysis, &django_models);
    migrations::attach_python_migrations(root, &mut analysis, &patterns);
    migrations::attach_sql_migrations(root, &mut analysis);
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
        (
            a.site.path.clone(),
            a.from.clone(),
            a.to.clone(),
            a.edge_type.to_string(),
        )
            .cmp(&(
                b.site.path.clone(),
                b.from.clone(),
                b.to.clone(),
                b.edge_type.to_string(),
            ))
    });
    analysis.relations.dedup_by(|a, b| {
        a.site.path == b.site.path && a.from == b.from && a.to == b.to && a.edge_type == b.edge_type
    });
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
