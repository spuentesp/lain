//! Django ORM table sensor — the ORM counterpart of `sql_sensor`.
//!
//! `sql_sensor` only understands literal SQL, so a Django codebase
//! (Saleor) produced one `Table` node. This sensor maps Django model
//! classes to their tables and emits `ReadsTable` / `WritesTable` edges
//! from the functions that run a queryset or write through an instance.
//!
//! # Model map
//!
//! Every top-level `class X(<bases>)` in a `*.py` file is a candidate. It
//! is a model when a base is `Model` / `AbstractUser` / … or a class
//! already known to be a model in the same repo (fixpoint, by name).
//! Table name, in order: `Meta.db_table` literal; otherwise
//! `<app_label>_<classname>` lowercased, where the app label is
//! `Meta.app_label` or the directory holding `models.py` (the directory
//! above a `models/` package). `abstract = True` models have no table;
//! `proxy = True` models use their concrete parent's. A non-literal
//! `db_table`, or a proxy whose parent is not unique, has **no** table
//! and every access to it goes on the ledger — nothing is guessed.
//!
//! The app label is a path heuristic: a custom `AppConfig.label` is not
//! read, so such an app gets a wrong default name (set `db_table` or
//! `app_label` in `Meta` and it is exact).
//!
//! # Access sites
//!
//! - `Model.objects[.using(db)].<chain>`: the chain is parsed across
//!   lines. Any write method in it (`create`, `update`, `delete`, …) is
//!   a `WritesTable`; a read method (`filter`, `get`, `values`, …) is a
//!   `ReadsTable`. A chain with only custom manager methods
//!   (`.visible_to_user(...)`) still starts at that model's manager, so it
//!   is emitted as a **heuristic** `ReadsTable` (confidence 0.6): the table
//!   is certain, the access kind is not. A bare `Model.objects` assigned
//!   to a variable, or a lowercase receiver (`self.model.objects`), is
//!   unresolved.
//! - `get_object_or_404(Model, …)` / `get_list_or_404`: read.
//! - `self.save()` inside a model class, and `var.save()` /
//!   `var.delete()` where `var` is bound in the same function from
//!   `Model(...)`, `Model.objects.get/create/...` or `get_object_or_404`:
//!   write. An unresolved `.save()` in a file that touches the ORM goes
//!   on the ledger.
//!
//! Ambiguity (two models with one name, not settled by same-file or
//! import) is unresolved too. A `.objects` receiver whose class is not a
//! model in this repo (a library model) is ignored, not ledgered.
//!
//! # Emission
//!
//! Like `sql_sensor`: one `Table` node per table (at the model's file,
//! so raw SQL on the same table name converges on the contract key), a
//! synthetic `orm-read:<path>:<line>` node per site carrying the
//! `TableConsumer` fact, and the edges. The sensor has its own
//! `SensorOwner`, so a rescan of either SQL sensor never retracts the
//! other's output.
//!
//! Model bases and manager/queryset method lists live in
//! `src/server/sensors/patterns/frameworks.yaml` under `django_orm:` and
//! layer additively with per-repo `<root>/.lain/patterns/*.yaml` overrides.

use crate::error::LainError;
use crate::federation::contracts::coverage::UnresolvedRecord;
use crate::federation::contracts::model::{
    ContractFact, SourceSite, Table, TableConsumerFact, UnresolvedReason,
};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{
    EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace, StaticSource,
};
use crate::server::sensors::patterns::{DjangoOrmShapes, Patterns};
use crate::server::sensors::SensorEntry;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Confidence of a `ReadsTable` whose access kind is inferred from a
/// custom manager method.
const CUSTOM_MANAGER_CONFIDENCE: f32 = 0.6;
const CUSTOM_MANAGER_DETECTOR: &str = "django_orm_custom_manager";

// ─── Public analysis shape ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OrmAccess {
    Reads,
    Writes,
}

/// One resolved access to a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrmSite {
    /// Graph path of the file the access is in.
    pub path: String,
    /// 1-based line of the access.
    pub line: u32,
    pub table: String,
    pub access: OrmAccess,
    /// The table is certain but the access kind was inferred.
    pub heuristic: bool,
}

/// One place the sensor saw ORM use and could not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrmUnresolved {
    pub path: String,
    pub line: u32,
    pub why: &'static str,
}

/// Where a table is defined (the model's file and class line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDef {
    pub path: String,
    pub line: u32,
}

/// Pure result of scanning a workspace; no graph involved.
#[derive(Debug, Default)]
pub struct Analysis {
    /// Classes recognised as Django models (abstract included).
    pub models_total: usize,
    pub tables: BTreeMap<String, TableDef>,
    pub sites: Vec<OrmSite>,
    pub unresolved: Vec<OrmUnresolved>,
}

// ─── Sensor ────────────────────────────────────────────────────────────

pub struct DjangoOrmSensor;

impl crate::server::sensors::Sensor for DjangoOrmSensor {
    fn name(&self) -> &'static str {
        "django_orm"
    }
    /// Counted with the SQL tables: both mint `Table` nodes.
    fn count_field(&self) -> crate::server::sensors::SensorCountField {
        crate::server::sensors::SensorCountField::SqlTables
    }
    fn phase(&self) -> u8 {
        1
    }
    fn scan(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<usize, LainError> {
        scan_workspace_django_orm_with_report(graph, root, namespace).map(|r| r.emitted)
    }
    fn scan_with_report(
        &self,
        graph: &GraphDatabase,
        root: &Path,
        namespace: &RepoNamespace,
    ) -> Result<crate::server::sensors::ScanReport, LainError> {
        scan_workspace_django_orm_with_report(graph, root, namespace)
    }
}
inventory::submit!(SensorEntry(&DjangoOrmSensor));

/// Scan `root` and emit `Table` nodes, `orm-read:` consumer nodes and
/// `ReadsTable` / `WritesTable` edges. Returns the count of tables.
pub fn scan_workspace_django_orm(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    scan_workspace_django_orm_with_report(graph, root, namespace).map(|r| r.emitted)
}

pub fn scan_workspace_django_orm_with_report(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<crate::server::sensors::ScanReport, LainError> {
    if graph.is_read_only() {
        return Ok(crate::server::sensors::ScanReport::default());
    }
    let analysis = analyze_workspace(root);
    let (nodes, edges) = build_graph(graph, &analysis, namespace);
    graph.replace_sensor_output(SensorOwner::DjangoOrmSensor, &nodes, &edges)?;

    let mut sample_ids: Vec<String> = Vec::new();
    for u in &analysis.unresolved {
        if sample_ids.len() < 5 {
            sample_ids.push(format!("{}:{}", u.path, u.line));
        }
    }
    let unresolved = if analysis.unresolved.is_empty() {
        Vec::new()
    } else {
        vec![UnresolvedRecord {
            reason: UnresolvedReason::OrmDynamicQuery,
            count: analysis.unresolved.len(),
            sample_ids,
        }]
    };
    Ok(crate::server::sensors::ScanReport {
        emitted: analysis.tables.len(),
        error: None,
        unresolved,
    })
}

// ─── Workspace analysis ────────────────────────────────────────────────

/// Two passes over the repo's `*.py` files: collect model classes, then
/// find access sites. Nothing is retained between passes but the (small)
/// class table.
pub fn analyze_workspace(root: &Path) -> Analysis {
    let patterns = Patterns::with_overrides(root).unwrap_or_else(|e| {
        tracing::warn!(
            "django_orm_sensor: {}: override load failed ({e}); using bundled patterns",
            root.display()
        );
        Cow::Borrowed(Patterns::patterns())
    });
    let shapes = patterns.django_orm_shapes();
    let py = |p: &Path| (p.extension().and_then(|e| e.to_str()) == Some("py")).then_some(());

    let mut classes: Vec<ClassDef> = Vec::new();
    for (path, content, ()) in crate::server::sensors::util::scan_files(root, py) {
        let gp = graph_path(root, &path);
        let masked = mask_python(&content);
        classes.extend(collect_classes(&gp, &content, &masked));
    }
    let models = ModelMap::build(classes, shapes);

    let mut out = Analysis {
        models_total: models.is_model.iter().filter(|m| **m).count(),
        ..Default::default()
    };
    for (i, c) in models.classes.iter().enumerate() {
        if !models.is_model[i] {
            continue;
        }
        if let TableRes::Table(t) = models.table_of(i, 0) {
            out.tables.entry(t).or_insert_with(|| TableDef {
                path: c.path.clone(),
                line: c.line,
            });
        }
    }
    for (path, content, ()) in crate::server::sensors::util::scan_files(root, py) {
        let gp = graph_path(root, &path);
        analyze_file(&models, shapes, &gp, &content, &mut out);
    }
    out.sites.sort_by(|a, b| {
        (&a.path, a.line, &a.table, a.access).cmp(&(&b.path, b.line, &b.table, b.access))
    });
    out.sites.dedup();
    out.unresolved
        .sort_by(|a, b| (&a.path, a.line, a.why).cmp(&(&b.path, b.line, b.why)));
    out.unresolved.dedup();
    out
}

// ─── Source masking ────────────────────────────────────────────────────

/// Copy of `src` with comments and string/docstring contents replaced by
/// spaces (delimiters and newlines kept), so byte offsets and line
/// numbers are identical to the original and nothing inside a string or
/// comment can match.
fn mask_python(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = b.to_vec();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c == b'#' {
            while i < n && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
        } else if c == b'\'' || c == b'"' {
            let triple = i + 2 < n && b[i + 1] == c && b[i + 2] == c;
            let open = if triple { 3 } else { 1 };
            let mut j = i + open;
            loop {
                if j >= n {
                    break;
                }
                if b[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if triple {
                    if j + 2 < n && b[j] == c && b[j + 1] == c && b[j + 2] == c {
                        break;
                    }
                } else if b[j] == c || b[j] == b'\n' {
                    break;
                }
                j += 1;
            }
            let end = j.min(n);
            for k in (i + open)..end {
                if out[k] != b'\n' {
                    out[k] = b' ';
                }
            }
            i = if triple {
                (end + 3).min(n)
            } else {
                (end + 1).min(n)
            };
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn line_starts(s: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

/// 1-based line of byte offset `off`.
fn line_of(starts: &[usize], off: usize) -> u32 {
    starts.partition_point(|&s| s <= off) as u32
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn ident_at(s: &str, from: usize) -> &str {
    let rest = &s[from..];
    let len = rest.bytes().take_while(|b| is_ident_byte(*b)).count();
    &rest[..len]
}

fn upper_first(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

// ─── Model map ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum DbTable {
    Default,
    Literal(String),
    Dynamic,
}

#[derive(Debug, Clone)]
struct ClassDef {
    name: String,
    path: String,
    /// 1-based line of the `class` statement.
    line: u32,
    /// 1-based inclusive last line of the class body.
    end_line: u32,
    bases: Vec<String>,
    abstract_: bool,
    proxy: bool,
    db_table: DbTable,
    app_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TableRes {
    Table(String),
    Abstract,
    Unresolved(&'static str),
}

struct ModelMap {
    classes: Vec<ClassDef>,
    by_name: BTreeMap<String, Vec<usize>>,
    is_model: Vec<bool>,
}

/// The app label Django would derive for a module path: the directory of
/// `models.py`, or the directory above a `models/` package.
fn app_of_path(path: &str) -> String {
    let comps: Vec<&str> = path.split('/').collect();
    let dirs = &comps[..comps.len().saturating_sub(1)];
    if let Some(pos) = dirs.iter().rposition(|c| *c == "models") {
        if pos > 0 {
            return dirs[pos - 1].to_string();
        }
    }
    dirs.last().map(|s| s.to_string()).unwrap_or_default()
}

impl ModelMap {
    fn build(classes: Vec<ClassDef>, shapes: &DjangoOrmShapes) -> Self {
        let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, c) in classes.iter().enumerate() {
            by_name.entry(c.name.clone()).or_default().push(i);
        }
        let mut is_model = vec![false; classes.len()];
        loop {
            let mut changed = false;
            for i in 0..classes.len() {
                if is_model[i] {
                    continue;
                }
                let hit = classes[i].bases.iter().any(|b| {
                    shapes.has_model_base(b)
                        || by_name
                            .get(b)
                            .is_some_and(|v| v.iter().any(|&j| j != i && is_model[j]))
                });
                if hit {
                    is_model[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Self {
            classes,
            by_name,
            is_model,
        }
    }

    fn app_of(&self, i: usize) -> String {
        let c = &self.classes[i];
        c.app_label.clone().unwrap_or_else(|| app_of_path(&c.path))
    }

    /// The table of model `i`. `depth` bounds proxy chains.
    fn table_of(&self, i: usize, depth: u8) -> TableRes {
        let c = &self.classes[i];
        if c.abstract_ {
            return TableRes::Abstract;
        }
        if c.proxy {
            if depth >= 4 {
                return TableRes::Unresolved("proxy chain too deep");
            }
            let parents: BTreeSet<TableRes> = c
                .bases
                .iter()
                .filter_map(|b| self.by_name.get(b))
                .flatten()
                .filter(|&&j| j != i && self.is_model[j] && !self.classes[j].abstract_)
                .map(|&j| self.table_of(j, depth + 1))
                .collect();
            return match parents.len() {
                1 => parents.into_iter().next().unwrap(),
                _ => TableRes::Unresolved("proxy parent not unique"),
            };
        }
        match &c.db_table {
            DbTable::Literal(t) => TableRes::Table(t.clone()),
            DbTable::Dynamic => TableRes::Unresolved("non-literal db_table"),
            DbTable::Default => TableRes::Table(format!(
                "{}_{}",
                self.app_of(i).to_lowercase(),
                c.name.to_lowercase()
            )),
        }
    }
}

impl PartialOrd for TableRes {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TableRes {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        format!("{self:?}").cmp(&format!("{other:?}"))
    }
}

/// Top-level classes of one file, with the `Meta` facts that decide the
/// table. `orig` and `masked` have identical offsets.
fn collect_classes(path: &str, orig: &str, masked: &str) -> Vec<ClassDef> {
    let ml: Vec<&str> = masked.lines().collect();
    let ol: Vec<&str> = orig.lines().collect();
    let mut out = Vec::new();
    let mut idx = 0;
    while idx < ml.len() {
        let line = ml[idx];
        let Some(rest) = line.strip_prefix("class ") else {
            idx += 1;
            continue;
        };
        let name = ident_at(rest, 0).to_string();
        if name.is_empty() {
            idx += 1;
            continue;
        }
        // Bases: from `(` to its match, possibly across lines.
        let mut bases_text = String::new();
        let mut body_from = idx + 1;
        let after = &rest[name.len()..];
        if after.trim_start().starts_with('(') {
            let mut depth = 0i32;
            let mut j = idx;
            let mut first = true;
            'outer: loop {
                let l = if first { after } else { ml[j] };
                for ch in l.chars() {
                    match ch {
                        '(' => {
                            depth += 1;
                            if depth == 1 {
                                continue;
                            }
                        }
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break 'outer;
                            }
                        }
                        _ => {}
                    }
                    if depth >= 1 {
                        bases_text.push(ch);
                    }
                }
                bases_text.push(' ');
                first = false;
                j += 1;
                if j >= ml.len() {
                    break;
                }
            }
            body_from = j + 1;
        }
        let bases: Vec<String> = split_top_level(&bases_text)
            .into_iter()
            .filter(|p| !p.contains('='))
            .map(|p| {
                let p = p.split('[').next().unwrap_or("").trim();
                p.rsplit('.').next().unwrap_or("").to_string()
            })
            .filter(|p| !p.is_empty())
            .collect();
        // Body: until the next non-blank line at indent 0.
        let mut end = body_from;
        while end < ml.len() && (ml[end].trim().is_empty() || indent_of(ml[end]) > 0) {
            end += 1;
        }
        let mut c = ClassDef {
            name,
            path: path.to_string(),
            line: (idx + 1) as u32,
            end_line: end as u32,
            bases,
            abstract_: false,
            proxy: false,
            db_table: DbTable::Default,
            app_label: None,
        };
        parse_meta(
            &ml[body_from.min(ml.len())..end.min(ml.len())],
            &ol,
            body_from,
            &mut c,
        );
        out.push(c);
        idx = end.max(idx + 1);
    }
    out
}

fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Read `abstract` / `proxy` / `db_table` / `app_label` from the class
/// body's `class Meta:`. `body` is masked; literals are read back from
/// the original line (same offsets).
fn parse_meta(body: &[&str], orig_lines: &[&str], body_from: usize, c: &mut ClassDef) {
    let mut i = 0;
    while i < body.len() {
        let t = body[i].trim();
        if t.starts_with("class Meta") {
            let m_indent = indent_of(body[i]);
            let mut j = i + 1;
            while j < body.len() && (body[j].trim().is_empty() || indent_of(body[j]) > m_indent) {
                let line = body[j];
                if let Some((k, v)) = line.split_once('=') {
                    let key = k.trim();
                    let val = v.trim();
                    match key {
                        "abstract" => c.abstract_ = val == "True",
                        "proxy" => c.proxy = val == "True",
                        "db_table" | "app_label" => {
                            let lit = literal_value(orig_lines.get(body_from + j), line);
                            if key == "db_table" {
                                c.db_table = lit.map_or(DbTable::Dynamic, DbTable::Literal);
                            } else if let Some(l) = lit {
                                c.app_label = Some(l);
                            }
                        }
                        _ => {}
                    }
                }
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
}

/// The string literal on the right of `=` in `masked`, read from `orig`
/// (same offsets). `None` when the right-hand side is not one plain
/// literal (a `%`, `+`, call, name… is dynamic).
fn literal_value(orig: Option<&&str>, masked: &str) -> Option<String> {
    let orig = orig?;
    let eq = masked.find('=')?;
    let rhs = &masked[eq + 1..];
    let start = eq + 1 + (rhs.len() - rhs.trim_start().len());
    let q = masked[start..].chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let end = start + 1 + masked[start + 1..].find(q)?;
    if !masked[end + 1..].trim().is_empty() {
        return None;
    }
    orig.get(start + 1..end).map(str::to_string)
}

// ─── Per-file analysis ─────────────────────────────────────────────────

#[derive(Default)]
struct Imports {
    /// local alias → dotted module (`product_models` → `saleor.product.models`).
    modules: BTreeMap<String, String>,
    /// local name → (dotted module, original name).
    names: BTreeMap<String, (String, String)>,
}

/// The app a dotted module belongs to, `None` for django itself.
fn app_of_module(m: &str) -> Option<String> {
    if m.starts_with("django") {
        return None;
    }
    let comps: Vec<&str> = m.split('.').filter(|c| !c.is_empty()).collect();
    if let Some(pos) = comps.iter().rposition(|c| *c == "models") {
        return (pos > 0).then(|| comps[pos - 1].to_string());
    }
    comps.last().map(|s| s.to_string())
}

fn parse_imports(masked: &str, file_path: &str) -> Imports {
    let mut imp = Imports::default();
    let lines: Vec<&str> = masked.lines().collect();
    let dir: Vec<&str> = {
        let c: Vec<&str> = file_path.split('/').collect();
        c[..c.len().saturating_sub(1)].to_vec()
    };
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if let Some(rest) = t.strip_prefix("from ") {
            let Some((module, names)) = rest.split_once(" import ") else {
                i += 1;
                continue;
            };
            let mut names_text = names.to_string();
            if names_text.contains('(') && !names_text.contains(')') {
                let mut j = i + 1;
                while j < lines.len() {
                    names_text.push(' ');
                    names_text.push_str(lines[j].trim());
                    if lines[j].contains(')') {
                        break;
                    }
                    j += 1;
                }
                i = j;
            }
            let module = module.trim();
            let dots = module.chars().take_while(|c| *c == '.').count();
            let resolved = if dots == 0 {
                module.to_string()
            } else {
                let up = dots - 1;
                if up > dir.len() {
                    i += 1;
                    continue;
                }
                let mut base: Vec<&str> = dir[..dir.len() - up].to_vec();
                let tail = module.trim_start_matches('.');
                if !tail.is_empty() {
                    base.extend(tail.split('.'));
                }
                base.join(".")
            };
            let cleaned = names_text.replace(['(', ')'], " ");
            for part in cleaned.split(',') {
                let mut it = part.split_whitespace();
                let Some(orig) = it.next() else { continue };
                let local = match (it.next(), it.next()) {
                    (Some("as"), Some(l)) => l,
                    _ => orig,
                };
                if orig == "*" {
                    continue;
                }
                if orig.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                    imp.modules
                        .insert(local.to_string(), format!("{resolved}.{orig}"));
                } else {
                    imp.names
                        .insert(local.to_string(), (resolved.clone(), orig.to_string()));
                }
            }
        } else if let Some(rest) = t.strip_prefix("import ") {
            for part in rest.split(',') {
                let mut it = part.split_whitespace();
                if let (Some(m), Some("as"), Some(l)) = (it.next(), it.next(), it.next()) {
                    imp.modules.insert(l.to_string(), m.to_string());
                }
            }
        }
        i += 1;
    }
    imp
}

enum Resolved {
    NotModel,
    One(usize),
    Ambiguous,
}

struct FileCtx<'a> {
    path: &'a str,
    imports: Imports,
}

impl ModelMap {
    fn resolve_class(&self, name: &str, qualifier: Option<&str>, ctx: &FileCtx) -> Resolved {
        let (orig_name, import_app) = match ctx.imports.names.get(name) {
            Some((m, o)) => (o.as_str(), app_of_module(m)),
            None => (name, None),
        };
        let cands: Vec<usize> = self
            .by_name
            .get(orig_name)
            .map(|v| v.iter().copied().filter(|&j| self.is_model[j]).collect())
            .unwrap_or_default();
        match cands.len() {
            0 => return Resolved::NotModel,
            1 => return Resolved::One(cands[0]),
            _ => {}
        }
        let same_file: Vec<usize> = cands
            .iter()
            .copied()
            .filter(|&j| self.classes[j].path == ctx.path)
            .collect();
        if same_file.len() == 1 {
            return Resolved::One(same_file[0]);
        }
        let hint = import_app.or_else(|| {
            qualifier
                .and_then(|q| ctx.imports.modules.get(q))
                .and_then(|m| app_of_module(m))
        });
        if let Some(app) = hint {
            let by_app: Vec<usize> = cands
                .iter()
                .copied()
                .filter(|&j| self.app_of(j) == app)
                .collect();
            if by_app.len() == 1 {
                return Resolved::One(by_app[0]);
            }
        }
        Resolved::Ambiguous
    }
}

/// Parse `.name(args)` links starting at `i` (just after `.objects` or a
/// previous link). Returns the method names and stops after a terminal
/// method. Whitespace and newlines before a `.` are skipped.
fn parse_chain(m: &str, mut i: usize, shapes: &DjangoOrmShapes) -> Vec<String> {
    let b = m.as_bytes();
    let n = b.len();
    let mut methods = Vec::new();
    loop {
        let mut j = i;
        while j < n && (b[j].is_ascii_whitespace() || b[j] == b'\\') {
            j += 1;
        }
        if j >= n || b[j] != b'.' {
            break;
        }
        j += 1;
        while j < n && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let name = ident_at(m, j);
        if name.is_empty() {
            break;
        }
        let mut k = j + name.len();
        while k < n && (b[k] == b' ' || b[k] == b'\t') {
            k += 1;
        }
        if k < n && b[k] == b'(' {
            let mut depth = 0i32;
            while k < n {
                match b[k] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            k += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            methods.push(name.to_string());
            i = k;
            if shapes.is_terminal_method(name) {
                break;
            }
        } else {
            i = j + name.len();
        }
    }
    methods
}

fn classify_chain(methods: &[String], shapes: &DjangoOrmShapes) -> Option<(Vec<OrmAccess>, bool)> {
    let writes = methods.iter().any(|m| shapes.is_write_method(m));
    let reads = methods.iter().any(|m| shapes.is_read_method(m));
    let both = methods.iter().any(|m| shapes.is_read_write_method(m));
    let custom = methods.iter().any(|m| {
        !shapes.is_write_method(m) && !shapes.is_read_method(m) && m != "using" && m != "db_manager"
    });
    if writes {
        let mut a = vec![OrmAccess::Writes];
        if both {
            a.push(OrmAccess::Reads);
        }
        return Some((a, false));
    }
    if reads {
        return Some((vec![OrmAccess::Reads], false));
    }
    custom.then_some((vec![OrmAccess::Reads], true))
}

/// Innermost enclosing `def` (1-based line, 0 for module level) of every
/// line, from indentation.
fn def_scopes(masked: &str) -> Vec<u32> {
    let mut scopes = Vec::new();
    let mut stack: Vec<(usize, u32)> = Vec::new();
    for (idx, line) in masked.lines().enumerate() {
        if line.trim().is_empty() {
            scopes.push(stack.last().map_or(0, |s| s.1));
            continue;
        }
        let ind = indent_of(line);
        while stack.last().is_some_and(|s| s.0 >= ind) {
            stack.pop();
        }
        scopes.push(stack.last().map_or(0, |s| s.1));
        let t = line.trim_start();
        if t.starts_with("def ") || t.starts_with("async def ") {
            stack.push((ind, (idx + 1) as u32));
        }
    }
    scopes
}

fn analyze_file(
    models: &ModelMap,
    shapes: &DjangoOrmShapes,
    path: &str,
    orig: &str,
    out: &mut Analysis,
) {
    let masked = mask_python(orig);
    let m = masked.as_str();
    if !m.contains(".objects")
        && !m.contains("get_object_or_404")
        && !m.contains("get_list_or_404")
        && !m.contains(".save(")
        && !m.contains(".delete(")
    {
        return;
    }
    let ctx = FileCtx {
        path,
        imports: parse_imports(m, path),
    };
    let starts = line_starts(m);
    let scopes = def_scopes(m);
    let scope_at = |line: u32| scopes.get(line as usize - 1).copied().unwrap_or(0);
    let mentions_orm = m.contains(".objects")
        || ctx
            .imports
            .modules
            .values()
            .chain(ctx.imports.names.values().map(|(mo, _)| mo))
            .any(|mo| mo.ends_with("models") || mo.contains(".models."));

    // Per-(scope, var) assignment history keyed by LHS byte offset (`Some(model)`
    // when bound to a model instance, `None` when reassigned to something else).
    let mut bound: BTreeMap<(u32, String), Vec<(usize, Option<usize>)>> = BTreeMap::new();
    let mut set_binding = |scope: u32, var: String, off: usize, model: Option<usize>| {
        let entries = bound.entry((scope, var)).or_default();
        if let Some(existing) = entries.iter_mut().find(|(o, _)| *o == off) {
            if model.is_some() {
                existing.1 = model;
            }
        } else {
            entries.push((off, model));
        }
    };

    let record =
        |out: &mut Analysis, line: u32, model: usize, access: Vec<OrmAccess>, heuristic: bool| {
            match models.table_of(model, 0) {
                TableRes::Table(t) => {
                    for a in access {
                        out.sites.push(OrmSite {
                            path: path.to_string(),
                            line,
                            table: t.clone(),
                            access: a,
                            heuristic,
                        });
                    }
                }
                TableRes::Abstract => out.unresolved.push(OrmUnresolved {
                    path: path.to_string(),
                    line,
                    why: "abstract model has no table",
                }),
                TableRes::Unresolved(why) => out.unresolved.push(OrmUnresolved {
                    path: path.to_string(),
                    line,
                    why,
                }),
            }
        };

    // Pass A: `<recv>.objects` chains.
    for (idx, _) in m.match_indices(".objects") {
        let after = idx + ".objects".len();
        if m.as_bytes().get(after).is_some_and(|b| is_ident_byte(*b)) {
            continue;
        }
        let line = line_of(&starts, idx);
        let mut s = idx;
        while s > 0 && (is_ident_byte(m.as_bytes()[s - 1]) || m.as_bytes()[s - 1] == b'.') {
            s -= 1;
        }
        let recv = &m[s..idx];
        let segs: Vec<&str> = recv.split('.').filter(|x| !x.is_empty()).collect();
        let Some(last) = segs.last().copied() else {
            out.unresolved.push(OrmUnresolved {
                path: path.to_string(),
                line,
                why: "computed receiver",
            });
            continue;
        };
        if !upper_first(last) {
            out.unresolved.push(OrmUnresolved {
                path: path.to_string(),
                line,
                why: "dynamic receiver",
            });
            continue;
        }
        let qualifier = (segs.len() >= 2).then(|| segs[segs.len() - 2]);
        let model = match models.resolve_class(last, qualifier, &ctx) {
            Resolved::NotModel => continue,
            Resolved::Ambiguous => {
                out.unresolved.push(OrmUnresolved {
                    path: path.to_string(),
                    line,
                    why: "ambiguous model name",
                });
                continue;
            }
            Resolved::One(j) => j,
        };
        let methods = parse_chain(m, after, shapes);
        let Some((access, heuristic)) = classify_chain(&methods, shapes) else {
            out.unresolved.push(OrmUnresolved {
                path: path.to_string(),
                line,
                why: "bare manager",
            });
            continue;
        };
        record(out, line, model, access, heuristic);
        if let Some((assign_off, var)) = assigned_var(m, &starts, s) {
            let bound_model = methods
                .last()
                .is_some_and(|l| shapes.is_instance_method(l))
                .then_some(model);
            set_binding(scope_at(line), var, assign_off, bound_model);
        }
    }

    // `get_object_or_404(Model, …)` / `get_list_or_404(Model, …)`.
    for needle in ["get_object_or_404(", "get_list_or_404("] {
        for (idx, _) in m.match_indices(needle) {
            if idx > 0 && is_ident_byte(m.as_bytes()[idx - 1]) {
                continue;
            }
            let from = idx + needle.len();
            let arg: String = m[from..]
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            let rest_after = m[from..].trim_start()[arg.len()..].trim_start();
            if arg.is_empty() || !(rest_after.starts_with(',') || rest_after.starts_with(')')) {
                continue; // a queryset expression: the `.objects` scan has it
            }
            let segs: Vec<&str> = arg.split('.').collect();
            let last = segs[segs.len() - 1];
            if !upper_first(last) {
                continue;
            }
            let qualifier = (segs.len() >= 2).then(|| segs[segs.len() - 2]);
            let line = line_of(&starts, idx);
            match models.resolve_class(last, qualifier, &ctx) {
                Resolved::NotModel => {}
                Resolved::Ambiguous => out.unresolved.push(OrmUnresolved {
                    path: path.to_string(),
                    line,
                    why: "ambiguous model name",
                }),
                Resolved::One(j) => {
                    record(out, line, j, vec![OrmAccess::Reads], false);
                    if let Some((assign_off, var)) = assigned_var(m, &starts, idx) {
                        let bound_model = (needle == "get_object_or_404(").then_some(j);
                        set_binding(scope_at(line), var, assign_off, bound_model);
                    }
                }
            }
        }
    }

    // Instance creation (`x = Model(...)`) and plain reassignments that
    // aren't already handled by `.objects` or `get_object_or_404`.
    for (idx, line_text) in m.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        let line_start = starts[idx];
        let scope = scope_at(line_no);
        let mut cursor = 0usize;
        for stmt in line_text.split(';') {
            let stmt_start = line_start + cursor;
            cursor += stmt.len() + 1;
            let trimmed = stmt.trim_start();
            let ws_len = stmt.len() - trimmed.len();
            let Some((target, rhs)) = trimmed.split_once('=') else {
                continue;
            };
            if target.ends_with([
                '!', '<', '>', '=', ':', '+', '-', '*', '/', '%', '&', '|', '^',
            ]) || rhs.starts_with('=')
            {
                continue;
            }
            let target = target.trim();
            if target.is_empty() || !target.bytes().all(is_ident_byte) {
                continue;
            }
            let lhs_off = stmt_start + ws_len;
            let rhs = rhs.trim_start();
            let rhs = rhs.strip_prefix("await ").unwrap_or(rhs);
            // Skip `.objects` and `get_*_or_404` RHS — those passes record
            // the binding themselves at the exact `lhs_off`.
            if rhs.contains(".objects")
                || rhs.starts_with("get_object_or_404(")
                || rhs.starts_with("get_list_or_404(")
            {
                continue;
            }
            let head: String = rhs
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            let bound_model = if !head.is_empty() && rhs[head.len()..].starts_with('(') {
                let segs: Vec<&str> = head.split('.').collect();
                let last = segs[segs.len() - 1];
                if upper_first(last) {
                    let qualifier = (segs.len() >= 2).then(|| segs[segs.len() - 2]);
                    match models.resolve_class(last, qualifier, &ctx) {
                        Resolved::One(j) => Some(j),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            };
            set_binding(scope, target.to_string(), lhs_off, bound_model);
        }
    }

    for entries in bound.values_mut() {
        entries.sort_by_key(|(off, _)| *off);
    }

    // Pass B: `.save()` / `.delete()` on a resolved instance.
    let class_spans: Vec<(u32, u32, usize)> = models
        .classes
        .iter()
        .enumerate()
        .filter(|(i, c)| models.is_model[*i] && c.path == path)
        .map(|(i, c)| (c.line, c.end_line, i))
        .collect();
    for (needle, is_save) in [(".save(", true), (".delete(", false)] {
        for (idx, _) in m.match_indices(needle) {
            let line = line_of(&starts, idx);
            let mut s = idx;
            while s > 0 && is_ident_byte(m.as_bytes()[s - 1]) {
                s -= 1;
            }
            let recv = &m[s..idx];
            let dotted_before = s > 0 && m.as_bytes()[s - 1] == b'.';
            if recv.is_empty() || dotted_before {
                continue;
            }
            if recv == "self" {
                if let Some(&(_, _, i)) =
                    class_spans.iter().find(|(a, b, _)| line > *a && line <= *b)
                {
                    record(out, line, i, vec![OrmAccess::Writes], false);
                }
                continue;
            }
            let resolved_model =
                bound
                    .get(&(scope_at(line), recv.to_string()))
                    .and_then(|entries| {
                        entries
                            .iter()
                            .rev()
                            .find(|(off, _)| *off < s)
                            .and_then(|(_, model)| *model)
                    });
            if let Some(j) = resolved_model {
                record(out, line, j, vec![OrmAccess::Writes], false);
            } else if is_save && mentions_orm {
                out.unresolved.push(OrmUnresolved {
                    path: path.to_string(),
                    line,
                    why: "unresolved save() receiver",
                });
            }
        }
    }
}

/// The `(lhs_offset, variable_name)` a statement starting at/near `recv_start`
/// assigns to: `x = <recv>…`, `x, created = <recv>…`, or the previous line
/// ending in `x =` / `x = (`.
fn assigned_var(m: &str, starts: &[usize], recv_start: usize) -> Option<(usize, String)> {
    let line_no = line_of(starts, recv_start) as usize;
    let ls = starts[line_no - 1];
    // Respect `;` statement boundaries on the same line.
    let seg_rel_start = m[ls..recv_start].rfind(';').map_or(0, |p| p + 1);
    let seg_start = ls + seg_rel_start;
    let raw_before = &m[seg_start..recv_start];
    let trimmed_start = raw_before.trim_start();
    let leading_ws = raw_before.len() - trimmed_start.len();
    let before = trimmed_start.trim_end();
    let before = before.strip_suffix("await").map_or(before, str::trim);
    let (assign_off, candidate) = if before.is_empty() || before == "(" {
        let prev_line = (1..line_no)
            .rev()
            .find(|&n| !m[starts[n - 1]..starts[n]].trim().is_empty())?;
        let raw_prev = &m[starts[prev_line - 1]..starts[prev_line]];
        let prev_trimmed = raw_prev.trim_start();
        let prev_ws = raw_prev.len() - prev_trimmed.len();
        (
            starts[prev_line - 1] + prev_ws,
            prev_trimmed.trim_end().to_string(),
        )
    } else {
        (seg_start + leading_ws, before.to_string())
    };
    let lhs = candidate
        .trim_end_matches('(')
        .trim_end()
        .strip_suffix('=')?
        .trim();
    let first = lhs.split(',').next()?.trim();
    (!first.is_empty() && first.bytes().all(is_ident_byte)).then(|| (assign_off, first.to_string()))
}

// ─── Graph emission ────────────────────────────────────────────────────

fn build_graph(
    graph: &GraphDatabase,
    analysis: &Analysis,
    namespace: &RepoNamespace,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut table_ids: BTreeMap<&str, String> = BTreeMap::new();

    for (name, def) in &analysis.tables {
        let id = GraphNode::generate_id(&NodeType::Table, &def.path, name, None, namespace);
        let mut node =
            GraphNode::new_in(NodeType::Table, name.clone(), def.path.clone(), namespace);
        node.id = id.clone();
        node.line_start = Some(def.line);
        node.signature = Some(crate::server::sensors::util::DJANGO_TABLE_MARK.to_string());
        node.contract = vec![ContractFact::Table(Table {
            service: String::new(),
            name: name.clone(),
        })];
        table_ids.insert(name.as_str(), id);
        nodes.push(node);
    }

    // One reader node per (file, line), listing every table it touches.
    let mut readers: BTreeMap<(&str, u32), BTreeSet<String>> = BTreeMap::new();
    for s in &analysis.sites {
        readers
            .entry((s.path.as_str(), s.line))
            .or_default()
            .insert(s.table.clone());
        let Some(table_id) = table_ids.get(s.table.as_str()) else {
            continue;
        };
        let Some(source_id) =
            crate::server::sensors::sql_sensor::enclosing_or_file(graph, &s.path, s.line)
        else {
            continue;
        };
        let kind = match s.access {
            OrmAccess::Reads => EdgeType::ReadsTable,
            OrmAccess::Writes => EdgeType::WritesTable,
        };
        let mut edge = GraphEdge::new(kind, source_id, table_id.clone());
        edge.provenance = Some(if s.heuristic {
            EdgeProvenance::Heuristic {
                detector: CUSTOM_MANAGER_DETECTOR.to_string(),
                confidence: CUSTOM_MANAGER_CONFIDENCE,
            }
        } else {
            EdgeProvenance::Static {
                source: StaticSource::Regex,
            }
        });
        edge.site = Some(SourceSite {
            path: s.path.clone(),
            line: s.line,
        });
        edges.push(edge);
    }
    for ((path, line), tables) in readers {
        let mut reader = crate::server::sensors::util::synthetic_site_node(
            format!(
                "{}{path}:{line}",
                crate::server::sensors::util::ORM_READ_PREFIX
            ),
            path,
            line,
            namespace,
        );
        reader.contract = vec![ContractFact::TableConsumer(TableConsumerFact {
            tables: tables.into_iter().collect(),
        })];
        nodes.push(reader);
    }
    (nodes, edges)
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn analyze(files: &[(&str, &str)]) -> Analysis {
        let tmp = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            write(tmp.path(), rel, body);
        }
        analyze_workspace(tmp.path())
    }

    const PRODUCT_MODELS: &str = "\
from django.db import models

class SeoModel(models.Model):
    class Meta:
        abstract = True

class Product(SeoModel):
    name = models.CharField()
";

    fn site(a: &Analysis, line: u32) -> Vec<(String, OrmAccess, bool)> {
        a.sites
            .iter()
            .filter(|s| s.line == line)
            .map(|s| (s.table.clone(), s.access, s.heuristic))
            .collect()
    }

    // ── model map ────────────────────────────────────────────────────

    #[test]
    fn default_table_is_app_label_plus_lowercased_class() {
        let a = analyze(&[("shop/product/models.py", PRODUCT_MODELS)]);
        assert!(a.tables.contains_key("product_product"), "{:?}", a.tables);
        assert_eq!(a.tables["product_product"].path, "shop/product/models.py");
        assert_eq!(a.tables["product_product"].line, 7);
    }

    #[test]
    fn abstract_models_are_models_without_a_table() {
        let a = analyze(&[("shop/product/models.py", PRODUCT_MODELS)]);
        assert_eq!(a.models_total, 2, "SeoModel + Product");
        assert_eq!(a.tables.len(), 1, "abstract SeoModel has no table");
    }

    #[test]
    fn meta_db_table_literal_and_app_label_override_the_default() {
        let a = analyze(&[(
            "x/models.py",
            "from django.db import models\n\
             class A(models.Model):\n    class Meta:\n        db_table = \"custom_a\"\n\
             class B(models.Model):\n    class Meta:\n        app_label = \"billing\"\n",
        )]);
        assert!(a.tables.contains_key("custom_a"));
        assert!(a.tables.contains_key("billing_b"), "{:?}", a.tables);
    }

    #[test]
    fn models_package_uses_the_directory_above_it() {
        let a = analyze(&[(
            "shop/order/models/base.py",
            "from django.db import models\nclass Line(models.Model):\n    pass\n",
        )]);
        assert!(a.tables.contains_key("order_line"), "{:?}", a.tables);
    }

    #[test]
    fn inheritance_resolves_across_files_by_name() {
        let a = analyze(&[
            ("core/models.py", "from django.db import models\nclass Base(models.Model):\n    class Meta:\n        abstract = True\n"),
            ("shop/models.py", "from core.models import Base\nclass Thing(Base):\n    pass\n"),
            ("shop/other.py", "class NotAModel(object):\n    pass\n"),
        ]);
        assert!(a.tables.contains_key("shop_thing"), "{:?}", a.tables);
        assert_eq!(a.tables.len(), 1);
    }

    #[test]
    fn non_literal_db_table_has_no_table_and_accesses_are_ledgered() {
        let a = analyze(&[
            (
                "x/models.py",
                "from django.db import models\nNAME = 'n'\nclass A(models.Model):\n    class Meta:\n        db_table = NAME\n",
            ),
            ("x/use.py", "from x.models import A\ndef f():\n    return A.objects.get(pk=1)\n"),
        ]);
        assert!(a.tables.is_empty());
        assert!(a.sites.is_empty());
        assert_eq!(a.unresolved.len(), 1);
        assert_eq!(a.unresolved[0].why, "non-literal db_table");
    }

    #[test]
    fn proxy_model_uses_its_parents_table() {
        let a = analyze(&[(
            "x/models.py",
            "from django.db import models\nclass A(models.Model):\n    pass\nclass P(A):\n    class Meta:\n        proxy = True\n",
        )]);
        assert_eq!(a.tables.keys().collect::<Vec<_>>(), vec!["x_a"]);
    }

    // ── access detection ─────────────────────────────────────────────

    fn with_models(use_py: &str) -> Analysis {
        analyze(&[
            ("shop/product/models.py", PRODUCT_MODELS),
            ("shop/product/use.py", use_py),
        ])
    }

    #[test]
    fn manager_reads_and_writes_are_classified() {
        let a = with_models(
            "from shop.product.models import Product\n\
             def f():\n\
             \x20   a = Product.objects.filter(id=1)\n\
             \x20   Product.objects.create(name='x')\n\
             \x20   Product.objects.filter(id=1).update(name='y')\n\
             \x20   Product.objects.filter(id=1).delete()\n",
        );
        assert_eq!(
            site(&a, 3),
            vec![("product_product".into(), OrmAccess::Reads, false)]
        );
        assert_eq!(
            site(&a, 4),
            vec![("product_product".into(), OrmAccess::Writes, false)]
        );
        assert_eq!(
            site(&a, 5),
            vec![("product_product".into(), OrmAccess::Writes, false)]
        );
        assert_eq!(
            site(&a, 6),
            vec![("product_product".into(), OrmAccess::Writes, false)]
        );
    }

    #[test]
    fn get_or_create_both_reads_and_writes() {
        let a = with_models(
            "from shop.product.models import Product\ndef f():\n    Product.objects.get_or_create(name='x')\n",
        );
        let s = site(&a, 3);
        assert!(s.contains(&("product_product".into(), OrmAccess::Reads, false)));
        assert!(s.contains(&("product_product".into(), OrmAccess::Writes, false)));
    }

    #[test]
    fn saleor_style_using_chain_and_qualified_receiver() {
        let a = with_models(
            "from shop.product import models\n\
             def f(db):\n\
             \x20   qs = models.Product.objects.using(db).filter(\n\
             \x20       id=1,\n\
             \x20   )\n\
             \x20   return qs\n",
        );
        assert_eq!(
            site(&a, 3),
            vec![("product_product".into(), OrmAccess::Reads, false)]
        );
    }

    #[test]
    fn chain_continues_on_following_lines() {
        let a = with_models(
            "from shop.product.models import Product\n\
             def f():\n\
             \x20   return (\n\
             \x20       Product.objects\n\
             \x20       .using('r')\n\
             \x20       .exclude(id=1)\n\
             \x20   )\n",
        );
        assert_eq!(
            site(&a, 4),
            vec![("product_product".into(), OrmAccess::Reads, false)]
        );
    }

    #[test]
    fn custom_manager_method_is_a_heuristic_read() {
        let a = with_models(
            "from shop.product.models import Product\ndef f(u):\n    return Product.objects.visible_to_user(u)\n",
        );
        assert_eq!(
            site(&a, 3),
            vec![("product_product".into(), OrmAccess::Reads, true)]
        );
    }

    #[test]
    fn bare_manager_and_dynamic_receivers_are_ledgered() {
        let a = with_models(
            "from shop.product.models import Product\n\
             def f(self, cls):\n\
             \x20   m = Product.objects\n\
             \x20   self.model.objects.all()\n\
             \x20   cls.objects.all()\n",
        );
        assert!(a.sites.is_empty());
        let whys: Vec<_> = a.unresolved.iter().map(|u| (u.line, u.why)).collect();
        assert_eq!(
            whys,
            vec![
                (3, "bare manager"),
                (4, "dynamic receiver"),
                (5, "dynamic receiver")
            ]
        );
    }

    #[test]
    fn non_model_objects_receivers_are_ignored() {
        let a = with_models("from lib import Widget\ndef f():\n    Widget.objects.all()\n");
        assert!(a.sites.is_empty());
        assert!(a.unresolved.is_empty());
    }

    #[test]
    fn strings_and_comments_never_match() {
        let a = with_models(
            "from shop.product.models import Product\n\
             # Product.objects.delete()\n\
             DOC = \"Product.objects.create()\"\n\
             def f():\n\
             \x20   '''Product.objects.filter()'''\n",
        );
        assert!(a.sites.is_empty() && a.unresolved.is_empty(), "{a:?}");
    }

    #[test]
    fn get_object_or_404_reads_the_model() {
        let a = with_models(
            "from shop.product.models import Product\n\
             from django.shortcuts import get_object_or_404\n\
             def f():\n\
             \x20   return get_object_or_404(Product, pk=1)\n",
        );
        assert_eq!(
            site(&a, 4),
            vec![("product_product".into(), OrmAccess::Reads, false)]
        );
    }

    #[test]
    fn instance_save_resolved_when_bound_in_the_same_function() {
        let a = with_models(
            "from shop.product.models import Product\n\
             def made():\n\
             \x20   p = Product(name='x')\n\
             \x20   p.save()\n\
             def fetched():\n\
             \x20   p = Product.objects.get(pk=1)\n\
             \x20   p.name = 'y'\n\
             \x20   p.save()\n\
             def other(p):\n\
             \x20   p.save()\n",
        );
        assert_eq!(
            site(&a, 4),
            vec![("product_product".into(), OrmAccess::Writes, false)]
        );
        assert_eq!(
            site(&a, 8),
            vec![("product_product".into(), OrmAccess::Writes, false)]
        );
        assert!(site(&a, 10).is_empty(), "unbound receiver is not an edge");
        assert!(a
            .unresolved
            .iter()
            .any(|u| u.line == 10 && u.why == "unresolved save() receiver"));
    }

    #[test]
    fn self_save_inside_a_model_class_is_a_write() {
        let a = analyze(&[(
            "shop/product/models.py",
            "from django.db import models\n\
             class Product(models.Model):\n\
             \x20   def publish(self):\n\
             \x20       self.save()\n",
        )]);
        assert_eq!(
            site(&a, 4),
            vec![("product_product".into(), OrmAccess::Writes, false)]
        );
    }

    #[test]
    fn same_named_models_are_settled_by_import_or_ledgered() {
        let files = [
            (
                "a/models.py",
                "from django.db import models\nclass Item(models.Model):\n    pass\n",
            ),
            (
                "b/models.py",
                "from django.db import models\nclass Item(models.Model):\n    pass\n",
            ),
        ];
        let mut ok = files.to_vec();
        ok.push((
            "c/use.py",
            "from b.models import Item\ndef f():\n    Item.objects.all()\n",
        ));
        let a = analyze(&ok);
        assert_eq!(
            site(&a, 3),
            vec![("b_item".into(), OrmAccess::Reads, false)]
        );

        let mut amb = files.to_vec();
        amb.push(("c/use.py", "def f(Item):\n    Item.objects.all()\n"));
        let a = analyze(&amb);
        assert!(a.sites.is_empty());
        assert_eq!(a.unresolved[0].why, "ambiguous model name");
    }

    // ── emission ─────────────────────────────────────────────────────

    fn scan(tmp: &tempfile::TempDir) -> (GraphDatabase, crate::server::sensors::ScanReport) {
        let g = GraphDatabase::new(&tmp.path().join("db.bin")).unwrap();
        let ns = RepoNamespace::for_test();
        let r = scan_workspace_django_orm_with_report(&g, tmp.path(), &ns).unwrap();
        (g, r)
    }

    fn fn_node(name: &str, path: &str, start0: u32, end0: u32) -> GraphNode {
        // 0-based rows, like the indexer.
        let ns = RepoNamespace::for_test();
        let mut n = GraphNode::new_in(NodeType::Function, name.into(), path.into(), &ns);
        n.line_start = Some(start0);
        n.line_end = Some(end0);
        n.id = GraphNode::generate_id(&NodeType::Function, path, name, Some(start0), &ns);
        n
    }

    #[test]
    fn emits_table_reader_and_edge_once_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "shop/product/models.py", PRODUCT_MODELS);
        write(
            tmp.path(),
            "shop/product/use.py",
            "from shop.product.models import Product\ndef resolve_product(pk):\n    return Product.objects.get(pk=pk)\n",
        );
        let g = GraphDatabase::new(&tmp.path().join("db.bin")).unwrap();
        let f = fn_node("resolve_product", "shop/product/use.py", 1, 2);
        g.insert_nodes_batch(std::slice::from_ref(&f)).unwrap();
        let ns = RepoNamespace::for_test();
        let r1 = scan_workspace_django_orm_with_report(&g, tmp.path(), &ns).unwrap();
        let r2 = scan_workspace_django_orm_with_report(&g, tmp.path(), &ns).unwrap();
        assert_eq!(r1.emitted, 1);
        assert_eq!(r2.emitted, 1);

        let tables: Vec<_> = g.get_nodes_by_type(NodeType::Table).unwrap();
        assert_eq!(tables.len(), 1, "rescan must not duplicate the table");
        assert_eq!(tables[0].name, "product_product");
        assert_eq!(sensor_owner(&tables[0]), Some(SensorOwner::DjangoOrmSensor));
        let edges: Vec<_> = g
            .get_edges_from(&f.id)
            .unwrap()
            .into_iter()
            .filter(|e| e.edge_type == EdgeType::ReadsTable)
            .collect();
        assert_eq!(edges.len(), 1, "exactly one resolver→table edge");
        assert_eq!(edges[0].target_id, tables[0].id);
        let readers: Vec<_> = g
            .get_all_nodes()
            .into_iter()
            .filter(|n| {
                n.name
                    .starts_with(crate::server::sensors::util::ORM_READ_PREFIX)
            })
            .collect();
        assert_eq!(readers.len(), 1);
        assert_eq!(readers[0].line_end, None);
    }

    fn sensor_owner(n: &GraphNode) -> Option<SensorOwner> {
        crate::graph::sensor_owner_of(n)
    }

    #[test]
    fn deleting_the_access_retracts_its_edge_and_reader() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "shop/product/models.py", PRODUCT_MODELS);
        write(
            tmp.path(),
            "shop/product/use.py",
            "from shop.product.models import Product\ndef f():\n    return Product.objects.all()\n",
        );
        let g = GraphDatabase::new(&tmp.path().join("db.bin")).unwrap();
        let f = fn_node("f", "shop/product/use.py", 1, 2);
        g.insert_nodes_batch(std::slice::from_ref(&f)).unwrap();
        let ns = RepoNamespace::for_test();
        scan_workspace_django_orm_with_report(&g, tmp.path(), &ns).unwrap();
        write(
            tmp.path(),
            "shop/product/use.py",
            "from shop.product.models import Product\ndef f():\n    return 1\n",
        );
        scan_workspace_django_orm_with_report(&g, tmp.path(), &ns).unwrap();
        assert!(g
            .get_edges_from(&f.id)
            .unwrap()
            .iter()
            .all(|e| e.edge_type != EdgeType::ReadsTable));
        assert!(!g.get_all_nodes().iter().any(|n| n
            .name
            .starts_with(crate::server::sensors::util::ORM_READ_PREFIX)));
    }

    #[test]
    fn heuristic_reads_carry_heuristic_provenance() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "shop/product/models.py", PRODUCT_MODELS);
        write(
            tmp.path(),
            "shop/product/use.py",
            "from shop.product.models import Product\ndef f(u):\n    return Product.objects.visible_to_user(u)\n",
        );
        let g = GraphDatabase::new(&tmp.path().join("db.bin")).unwrap();
        let f = fn_node("f", "shop/product/use.py", 1, 2);
        g.insert_nodes_batch(std::slice::from_ref(&f)).unwrap();
        scan_workspace_django_orm_with_report(&g, tmp.path(), &RepoNamespace::for_test()).unwrap();
        let e = g
            .get_edges_from(&f.id)
            .unwrap()
            .into_iter()
            .find(|e| e.edge_type == EdgeType::ReadsTable)
            .expect("edge");
        assert!(matches!(
            e.provenance,
            Some(EdgeProvenance::Heuristic { confidence, .. }) if confidence < 1.0
        ));
    }

    #[test]
    fn unresolved_sites_land_on_the_ledger_not_in_the_graph() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "shop/product/models.py", PRODUCT_MODELS);
        write(
            tmp.path(),
            "shop/product/use.py",
            "from shop.product.models import Product\ndef f():\n    m = Product.objects\n",
        );
        let (g, r) = scan(&tmp);
        assert_eq!(r.unresolved.len(), 1);
        assert_eq!(r.unresolved[0].reason, UnresolvedReason::OrmDynamicQuery);
        assert_eq!(r.unresolved[0].count, 1);
        assert!(g
            .get_all_nodes()
            .iter()
            .all(|n| !n.name.starts_with("orm-read:")));
    }

    #[test]
    fn reassigned_instance_variable_resolves_per_access_site() {
        let a = analyze(&[
            (
                "shop/models.py",
                "from django.db import models\n\
                 class Product(models.Model):\n    pass\n\
                 class Order(models.Model):\n    pass\n",
            ),
            (
                "shop/use.py",
                "from shop.models import Product, Order\n\
                 def update_both():\n\
                 \x20   item = Product.objects.get(pk=1)\n\
                 \x20   item.save()\n\
                 \x20   item = Order.objects.get(pk=2)\n\
                 \x20   item.save()\n\
                 def cleared_after_non_model_reassignment():\n\
                 \x20   item = Product.objects.get(pk=1)\n\
                 \x20   item = build_non_model()\n\
                 \x20   item.save()\n",
            ),
        ]);
        // Line 4 (`item.save()`) must resolve to `shop_product`, NOT `shop_order`.
        assert_eq!(
            site(&a, 4),
            vec![("shop_product".into(), OrmAccess::Writes, false)]
        );
        // Line 6 (`item.save()`) after reassignment to `Order` resolves to `shop_order`.
        assert_eq!(
            site(&a, 6),
            vec![("shop_order".into(), OrmAccess::Writes, false)]
        );
        // Line 10 (`item.save()`) after reassignment to non-model is not attributed to `shop_product`.
        assert!(site(&a, 10).is_empty(), "{:?}", site(&a, 10));
        assert!(
            a.unresolved
                .iter()
                .any(|u| u.line == 10 && u.why == "unresolved save() receiver"),
            "{:?}",
            a.unresolved
        );
    }

    #[test]
    fn pattern_overrides_extend_model_bases_and_methods() {
        let a = analyze(&[
            (
                ".lain/patterns/django.yaml",
                "django_orm:\n  model_bases:\n    - CustomAuditBase\n  write_methods:\n    - archive_all\n  terminal_methods:\n    - archive_all\n",
            ),
            (
                "shop/models.py",
                "from core.db import CustomAuditBase\n\
                 class Invoice(CustomAuditBase):\n    pass\n",
            ),
            (
                "shop/use.py",
                "from shop.models import Invoice\n\
                 def run():\n\
                 \x20   Invoice.objects.filter(paid=True).archive_all()\n",
            ),
        ]);
        assert!(a.tables.contains_key("shop_invoice"), "{:?}", a.tables);
        assert_eq!(
            site(&a, 3),
            vec![("shop_invoice".into(), OrmAccess::Writes, false)]
        );
    }

    /// Real-data sanity check, opt-in:
    /// `LAIN_ORM_SANITY_ROOT=/path/to/django/repo cargo test --lib
    /// orm_sanity -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn orm_sanity_on_a_real_repo() {
        let Ok(root) = std::env::var("LAIN_ORM_SANITY_ROOT") else {
            return;
        };
        let a = analyze_workspace(Path::new(&root));
        let mut by_why: BTreeMap<&str, usize> = BTreeMap::new();
        for u in &a.unresolved {
            *by_why.entry(u.why).or_default() += 1;
        }
        let reads = a
            .sites
            .iter()
            .filter(|s| s.access == OrmAccess::Reads)
            .count();
        let writes = a.sites.len() - reads;
        let heur = a.sites.iter().filter(|s| s.heuristic).count();
        let files: BTreeSet<&str> = a.sites.iter().map(|s| s.path.as_str()).collect();
        println!(
            "models_total={} tables={} sites={} (reads={} writes={} heuristic={}) files_with_sites={} unresolved={} {:?}",
            a.models_total, a.tables.len(), a.sites.len(), reads, writes, heur, files.len(),
            a.unresolved.len(), by_why
        );
        let mut by_table: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &a.sites {
            *by_table.entry(s.table.as_str()).or_default() += 1;
        }
        let mut top: Vec<_> = by_table.into_iter().collect();
        top.sort_by(|x, y| y.1.cmp(&x.1));
        println!("top tables: {:?}", &top[..top.len().min(8)]);
        let is_test =
            |p: &str| p.contains("tests/") || p.contains("/test_") || p.contains("conftest");
        let prod_sites = a.sites.iter().filter(|s| !is_test(&s.path)).count();
        let prod_heur = a
            .sites
            .iter()
            .filter(|s| !is_test(&s.path) && s.heuristic)
            .count();
        let mut prod_why: BTreeMap<&str, usize> = BTreeMap::new();
        for u in a.unresolved.iter().filter(|u| !is_test(&u.path)) {
            *prod_why.entry(u.why).or_default() += 1;
        }
        println!("non-test: sites={prod_sites} (heuristic={prod_heur}) unresolved={prod_why:?}");
        if let Ok(focus) = std::env::var("LAIN_ORM_SANITY_FILE") {
            for s in a.sites.iter().filter(|s| s.path == focus) {
                println!(
                    "  {}:{} {:?} {} heuristic={}",
                    s.path, s.line, s.access, s.table, s.heuristic
                );
            }
            for u in a.unresolved.iter().filter(|u| u.path == focus) {
                println!("  {}:{} UNRESOLVED {}", u.path, u.line, u.why);
            }
        }
    }
}
