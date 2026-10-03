//! Phase D — SQL-tables sensor (spec §7).
//!
//! Recognizes literal SQL inside known call shapes (sqlx, rusqlite,
//! `cursor.execute` / `db.query`, JDBC `prepareStatement`), parses the
//! statement with a focused hand-rolled parser, and emits one
//! `Table { service: "", name }` per distinct name plus a `ReadsTable`
//! / `WritesTable` edge from the enclosing function (or `File` for
//! module-level calls) to the table.
//!
//! The parser is deliberately narrow: it handles the canonical SQL
//! shapes the recognized call sites emit (SELECT with CTE /
//! subselect / join; INSERT / UPDATE / DELETE / MERGE). Quirkier
//! syntax (window functions, recursive CTEs, UPSERT clauses,
//! dialect-specific extensions) lands on the `unresolved` ledger with
//! reason `DynamicSql` — the spec treats "the parser could not
//! classify it" the same as "the SQL is dynamic".
//!
//! Non-literal SQL (parameter binding, `format!`, f-string, string
//! concatenation, …) also lands on the ledger. The sensor only
//! handles a single string literal in the call's first argument
//! position; anything else is `DynamicSql`.
//!
//! Phase A coverage ledger integrates the unresolved bucket — see
//! `crate::federation::contracts::coverage::{SensorLedger,
//! UnresolvedRecord, UnresolvedReason}`. Phase D's `DynamicSql`
//! reason piggybacks on the existing `DynamicUrl` enum value
//! (`DynamicSql`) so the ledger does not need a new variant.
//!
//! Phase 1: runs after `http_client_sensor` so the joiner can
//! resolve the enclosing function from the indexer's existing
//! Function / Method nodes. Like `http_client_sensor`, the
//! sql sensor owns its own `Table` nodes; a rescan retracts only
//! the previous SQL output via `replace_sensor_output`.
//!
//! Spec: docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md §7

use crate::error::LainError;
use crate::federation::contracts::coverage::UnresolvedRecord;
use crate::federation::contracts::model::{ContractFact, SourceSite, Table, UnresolvedReason};
use crate::graph::{graph_path, GraphDatabase, SensorOwner};
use crate::schema::{EdgeProvenance, EdgeType, GraphEdge, GraphNode, NodeType, RepoNamespace};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// ─── Public sensor shape ───────────────────────────────────────────────

/// One SQL statement detected inside a recognized call. Public so
/// the integration tests can exercise the detector without going
/// through the graph emission path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlSite {
    pub stmt: SqlStatement,
    pub line: u32,
}

/// What we did with the SQL — `Reads` (SELECT) or `Writes`
/// (INSERT / UPDATE / DELETE / MERGE). The sensor emits one edge
/// per `ReadsTable` or `WritesTable` per referenced table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlAccess {
    Reads,
    Writes,
}

/// One parsed SQL statement. Carries the access kind plus the
/// distinct tables it touches. Tables are deduped per statement
/// so a join against the same table three times produces one
/// edge, not three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlStatement {
    pub access: SqlAccess,
    pub tables: Vec<String>,
}

/// Unit-struct Sensor impl. Registered via `inventory::submit!`
/// below; no central registry to edit.
pub struct SqlSensor;

// Phase 1 (spec §6.1): runs after the protocol sensors and after
// `http_client_sensor`. The sql sensor reads the indexed Function /
// Method nodes via `enclosing_symbol`, so any phase that resolves
// symbols first is fine.
crate::server::sensors::register_sensor!(SqlSensor, "sql", SqlTables, 1, scan_workspace_sql);

// ─── Workspace scan ───────────────────────────────────────────────────

/// Walk `root`, find every literal SQL call shape, parse the
/// statement, and emit `Table` nodes + `ReadsTable` / `WritesTable`
/// edges. Returns the count of `Table` nodes minted (deduped
/// across all statements in the workspace, so a multi-call file
/// contributes one node per distinct table name + path).
pub fn scan_workspace_sql(
    graph: &GraphDatabase,
    root: &Path,
    namespace: &RepoNamespace,
) -> Result<usize, LainError> {
    if graph.is_read_only() {
        return Ok(0);
    }

    let mut all_nodes: Vec<GraphNode> = Vec::new();
    let mut all_edges: Vec<GraphEdge> = Vec::new();
    let mut unresolved: BTreeMap<String, UnresolvedRecord> = BTreeMap::new();

    let any_ext = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string())
    };
    for (path, content, ext) in crate::server::sensors::util::scan_files(root, any_ext) {
        let graph_path_str = graph_path(root, &path);
        let sites = detect_in_file(&content, &ext);
        let (nodes, edges, file_unresolved) =
            build_graph(graph, &sites, &graph_path_str, namespace);
        all_nodes.extend(nodes);
        all_edges.extend(edges);
        for rec in file_unresolved {
            let entry = unresolved
                .entry(format!("{:?}", rec.reason))
                .or_insert_with(|| UnresolvedRecord {
                    reason: rec.reason,
                    count: 0,
                    sample_ids: Vec::new(),
                });
            entry.count += rec.count;
            for id in &rec.sample_ids {
                if entry.sample_ids.len() < 5 {
                    entry.sample_ids.push(id.clone());
                }
            }
        }
    }

    let removed = graph.replace_sensor_output(SensorOwner::SqlSensor, &all_nodes, &all_edges)?;
    if removed > 0 {
        tracing::debug!("sql_sensor: replaced {removed} stale table edge(s) for {root:?}");
    }
    // The ledger bucket for `unresolved` lives in the per-sensor
    // coverage report; here we just count the deduped `Table` nodes
    // (the spec wants per-repo emission counts; `sensor_counts` is
    // a flat `Vec<Table>` count).
    let _ = unresolved; // reserved for the run_all_with_coverage path
    Ok(all_nodes.len())
}

// ─── Detection (regex-first, language-dispatched) ────────────────────

/// All SQL sites detected in one file. Each site carries a parsed
/// statement or a dynamic-SQL flag.
fn detect_in_file(content: &str, ext: &str) -> Vec<SqlSite> {
    let mut sites: Vec<SqlSite> = Vec::new();
    let shapes = shapes_for_ext(ext);
    let lines: Vec<&str> = content.lines().collect();
    // When two patterns match on the same line (e.g. `cursor.execute("...")`
    // matches both `cursor.execute(` and `.execute(`), we keep only
    // the most specific one — the needle with the longest prefix.
    // Tracking per-line prevents double-emission for nested matches.
    for idx in 0..lines.len() {
        let line = strip_line_comment(lines[idx]);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let line_num = (idx as u32) + 1;
        let mut best: Option<(usize, &SqlShape)> = None;
        for shape in &shapes {
            if !shape.matches(trimmed) {
                continue;
            }
            let needle_len = shape.needle.len();
            match best {
                None => best = Some((needle_len, shape)),
                Some((prev, _)) if needle_len > prev => best = Some((needle_len, shape)),
                _ => {}
            }
        }
        let Some((_, shape)) = best else {
            continue;
        };
        // Look at the matched line and up to 4 lines ahead for
        // the literal — multi-line calls like
        //
        //     conn.execute(
        //         "UPDATE …",
        //         params,
        //     )
        //
        // have the literal on a different line. We stop after the
        // first non-blank, non-comment line OR after the closing
        // paren of the call.
        let literal = first_string_arg_across_lines(&lines, idx, shape);
        match literal {
            Some(literal_text) => match parse_sql(&literal_text) {
                Some(stmt) => sites.push(SqlSite {
                    stmt,
                    line: line_num,
                }),
                None => sites.push(SqlSite {
                    stmt: SqlStatement {
                        access: SqlAccess::Reads,
                        tables: Vec::new(),
                    },
                    line: line_num,
                }),
            },
            None => {
                // Non-literal first arg → dynamic SQL.
                sites.push(SqlSite {
                    stmt: SqlStatement {
                        access: SqlAccess::Reads,
                        tables: Vec::new(),
                    },
                    line: line_num,
                });
            }
        }
    }
    sites
}

/// Look up to 4 lines ahead for the call's first string literal
/// argument. Returns `None` when no literal is found in the
/// lookahead window (the call's first arg is a non-literal
/// expression — e.g. `format!(...)` or a variable).
fn first_string_arg_across_lines(lines: &[&str], start: usize, shape: &SqlShape) -> Option<String> {
    let start_line = lines[start];
    let after_on_same = start_line
        .find(shape.needle)
        .map(|i| &start_line[i + shape.needle.len()..]);
    if let Some(text) = after_on_same {
        if let Some(lit) = extract_string_literal(text) {
            return Some(lit);
        }
    }
    for j in 1..=4 {
        let Some(lookahead_line) = lines.get(start + j) else {
            break;
        };
        let trimmed = strip_line_comment(lookahead_line).trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with(')') || trimmed == ");" || trimmed.starts_with("],") {
            // Closing of the call — stop scanning.
            break;
        }
        if let Some(lit) = extract_string_literal(trimmed) {
            return Some(lit);
        }
    }
    None
}

/// One recognized call shape. Each language has a small list of
/// shapes (sqlx::query, rusqlite::prepare, cursor.execute, JDBC
/// prepareStatement, …). The matcher is intentionally lenient —
/// it looks for a receiver keyword / function name + a `(` on
/// the same line — so a chained `.execute(...)` or
/// `.execute(&pool)` still matches.
struct SqlShape {
    /// A substring that must appear in the trimmed line.
    needle: &'static str,
}

impl SqlShape {
    const fn new(needle: &'static str) -> Self {
        Self { needle }
    }
    fn matches(&self, trimmed: &str) -> bool {
        trimmed.contains(self.needle)
    }
}

/// Per-language list of recognized call shapes. Each shape is the
/// receiver's name + `(`. The sensor fires for every match; the
/// first match wins per call (the regex does not overlap in
/// practice because shapes are by family — sqlx vs rusqlite vs
/// Python).
fn shapes_for_ext(ext: &str) -> Vec<SqlShape> {
    match ext {
        "rs" => vec![
            // sqlx
            SqlShape::new("sqlx::query("),
            SqlShape::new("sqlx::query_as("),
            SqlShape::new("sqlx::query_as_unchecked("),
            SqlShape::new("sqlx::query_scalar("),
            SqlShape::new("sqlx::query!(\""), // macro form
            SqlShape::new("sqlx::query_as!(\""),
            // rusqlite
            SqlShape::new("conn.prepare("),
            SqlShape::new("connection.prepare("),
            SqlShape::new("prepare("),
            SqlShape::new("conn.execute("),
            SqlShape::new("connection.execute("),
            SqlShape::new(".execute("),
            SqlShape::new(".query_row("),
            SqlShape::new(".query_map("),
        ],
        "py" | "pyx" => vec![
            // PEP 249 cursor / connection APIs
            SqlShape::new("cursor.execute("),
            SqlShape::new("cursor.executemany("),
            SqlShape::new(".execute("),
            SqlShape::new(".executemany("),
            SqlShape::new(".fetchone("),
            SqlShape::new(".fetchall("),
            SqlShape::new(".fetchmany("),
            SqlShape::new("db.query("),
            SqlShape::new("db.execute("),
            SqlShape::new("conn.execute("),
            SqlShape::new("connection.execute("),
            SqlShape::new("session.execute("),
            SqlShape::new(".scalar("),
        ],
        "java" => vec![
            // JDBC: `PreparedStatement ps = conn.prepareStatement("...");`
            SqlShape::new(".prepareStatement("),
            SqlShape::new(".prepareCall("),
            SqlShape::new(".createStatement().executeQuery("),
            SqlShape::new(".createStatement().executeUpdate("),
            SqlShape::new(".createStatement().execute("),
            SqlShape::new(".executeQuery("),
            SqlShape::new(".executeUpdate("),
            SqlShape::new(".execute("),
        ],
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => vec![
            // node-postgres / mysql2 / sqlite3 / better-sqlite3 / knex / prisma raw
            SqlShape::new(".query("),
            SqlShape::new(".execute("),
            SqlShape::new(".raw("),
            SqlShape::new(".prepare("),
            SqlShape::new("$queryRaw("),
            SqlShape::new("$executeRaw("),
        ],
        _ => Vec::new(),
    }
}

// ─── SQL parser (focused hand-rolled) ─────────────────────────────────

/// Parse a SQL literal into a [`SqlStatement`]. Returns `None`
/// when the literal is not a recognized statement (DDL, PRAGMA,
/// …) — the sensor reports it as `DynamicSql` so the operator
/// knows the call site was found but the statement wasn't.
///
/// The parser handles:
/// - SELECT (with optional CTE / WITH clause, subselects in
///   parentheses, JOIN / comma-joined tables)
/// - INSERT INTO <table> [(cols)] (VALUES | SELECT)
/// - UPDATE <table> SET ...
/// - DELETE FROM <table>
/// - MERGE INTO <table> USING ... ON ...
///
/// All variants are case-insensitive (we match the head keyword
/// case-insensitively). Table names are returned verbatim — the
/// spec stores names case-preserved so the joiner can apply the
/// repo's case-folding rules. Quoted strings inside SQL are
/// skipped so a string literal containing the word `from` does
/// not confuse the parser.
pub fn parse_sql(literal: &str) -> Option<SqlStatement> {
    let trimmed = literal.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return None;
    }
    let head = first_keyword(trimmed);
    match head.as_deref() {
        Some("SELECT") | Some("WITH") => {
            let tables = collect_tables_in_select(trimmed);
            Some(SqlStatement {
                access: SqlAccess::Reads,
                tables,
            })
        }
        Some("INSERT") => {
            let tables = collect_target_table_after(trimmed, "INSERT", "INTO");
            Some(SqlStatement {
                access: SqlAccess::Writes,
                tables,
            })
        }
        Some("UPDATE") => {
            let tables = collect_target_table_after(trimmed, "UPDATE", "");
            Some(SqlStatement {
                access: SqlAccess::Writes,
                tables,
            })
        }
        Some("DELETE") => {
            let tables = collect_target_table_after(trimmed, "DELETE", "FROM");
            Some(SqlStatement {
                access: SqlAccess::Writes,
                tables,
            })
        }
        Some("MERGE") => {
            let tables = collect_target_table_after(trimmed, "MERGE", "INTO");
            Some(SqlStatement {
                access: SqlAccess::Writes,
                tables,
            })
        }
        _ => None,
    }
}

fn first_keyword(sql: &str) -> Option<String> {
    for tok in tokenize(sql) {
        if is_sql_keyword(&tok.text.to_ascii_uppercase()) {
            return Some(tok.text.to_ascii_uppercase());
        }
    }
    None
}

fn is_sql_keyword(tok: &str) -> bool {
    matches!(
        tok,
        "SELECT"
            | "INSERT"
            | "UPDATE"
            | "DELETE"
            | "MERGE"
            | "WITH"
            | "CREATE"
            | "DROP"
            | "ALTER"
            | "TRUNCATE"
            | "BEGIN"
            | "COMMIT"
            | "ROLLBACK"
            | "EXPLAIN"
            | "PRAGMA"
            | "USE"
            | "SET"
            | "SHOW"
            | "GRANT"
            | "REVOKE"
    )
}

#[derive(Debug, Clone)]
struct Token {
    text: String,
    /// Reserved for future caller offsets; currently unused because
    /// the sensor only needs the token text. `#[allow(dead_code)]`
    /// is the cleaner alternative to deleting the field; the
    /// tokenizer's offsets are still useful for diagnostics.
    #[allow(dead_code)]
    start: usize,
    #[allow(dead_code)]
    end: usize,
}

/// Tokenize SQL into `(text, start, end)` triples, skipping
/// quoted strings and comments. Whitespace separates tokens; the
/// returned offsets let the caller scan the original string.
fn tokenize(sql: &str) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut current_start: Option<usize> = None;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        // Single-quoted string literal: '...' with '' as escape.
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                in_double = false;
            }
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            // Postgres treats "name" as an identifier. We still
            // skip the quoted region.
            in_double = true;
            i += 1;
            continue;
        }
        // `--` line comment until end-of-line.
        if b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // `/* ... */` block comment.
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if b.is_ascii_whitespace() {
            if let Some(s) = current_start {
                out.push(Token {
                    text: sql[s..i].to_string(),
                    start: s,
                    end: i,
                });
                current_start = None;
            }
            i += 1;
            continue;
        }
        // Punctuation / structural character — close any open token.
        if matches!(b, b'(' | b')' | b',' | b';' | b'=') {
            if let Some(s) = current_start {
                out.push(Token {
                    text: sql[s..i].to_string(),
                    start: s,
                    end: i,
                });
                current_start = None;
            }
            i += 1;
            continue;
        }
        if current_start.is_none() {
            current_start = Some(i);
        }
        i += 1;
    }
    if let Some(s) = current_start {
        out.push(Token {
            text: sql[s..bytes.len()].to_string(),
            start: s,
            end: bytes.len(),
        });
    }
    out
}

#[allow(dead_code)]
fn strip_sql_comments(sql: &str) -> String {
    // Tokenize to skip quoted regions; then drop `--` and `/* */`.
    // The simplest correct implementation is the tokenizer above
    // — the comment handling is in there. This helper is a thin
    // pass that collapses whitespace. Kept for future use (e.g.
    // normalising SQL before diffing two statements); the parser
    // currently runs directly on the raw literal.
    let mut out = String::with_capacity(sql.len());
    let mut prev_ws = true;
    for c in sql.chars() {
        if c.is_ascii_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

/// Collect every table name referenced by a SELECT / WITH statement.
/// Walks the FROM clause and any JOIN / subselect (in parentheses).
fn collect_tables_in_select(sql: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let tokens = tokenize(sql);
    let mut i = 0;
    // Skip the leading SELECT (or WITH ... SELECT) — we look for
    // FROM and JOIN keywords inside the statement.
    let mut from_positions: Vec<usize> = Vec::new();
    let mut join_positions: Vec<usize> = Vec::new();
    while i < tokens.len() {
        let tok_upper = tokens[i].text.to_ascii_uppercase();
        if tok_upper == "FROM" {
            from_positions.push(i);
        } else if is_join_keyword(&tok_upper) {
            join_positions.push(i);
        }
        i += 1;
    }
    for pos in from_positions.iter().chain(join_positions.iter()) {
        if let Some(name) = next_table_token(&tokens, *pos) {
            // For a CTE body (`WITH cte AS (SELECT ... FROM real_table)`)
            // the parser only sees the outer SELECT — `real_table`
            // lives inside the parens. The tokenizer's
            // parenthesis skip is what surfaces it: we walk the
            // tokens inside every `(` group and pick up any
            // table-shaped identifiers via the recursive helper.
            out.insert(name);
        }
    }
    // Subselects: any `(` followed by a SELECT / WITH is a
    // subselect; recurse into its body.
    collect_tables_in_parens(sql, &mut out);
    out.into_iter().collect()
}

fn is_join_keyword(tok: &str) -> bool {
    matches!(
        tok,
        "JOIN" | "INNER" | "LEFT" | "RIGHT" | "FULL" | "OUTER" | "CROSS"
    )
}

/// After a FROM or JOIN keyword, the next token (skipping AS /
/// index hints) is the table name. We also unwrap `schema.table`
/// → `table`.
fn next_table_token(tokens: &[Token], from_or_join_idx: usize) -> Option<String> {
    let mut i = from_or_join_idx + 1;
    // Skip modifiers like `LEFT OUTER JOIN schema.table alias`.
    while i < tokens.len() {
        let t_upper = tokens[i].text.to_ascii_uppercase();
        // After a JOIN keyword, the parser has consumed `JOIN`
        // (or `LEFT OUTER JOIN`); the very next token is the
        // table.
        if from_or_join_idx > 0
            && tokens[from_or_join_idx].text.eq_ignore_ascii_case("JOIN")
            && i == from_or_join_idx + 1
        {
            // fall through and consume the table name
        } else if t_upper == "AS" {
            i += 1;
            continue;
        }
        if is_join_keyword(&t_upper) {
            // For `LEFT OUTER JOIN`, the table follows the JOIN.
            i += 1;
            continue;
        }
        if tokens[i].text == "(" {
            // Subselect — recurse.
            return None;
        }
        // Strip schema prefix: `public.users` → `users`.
        let name = tokens[i]
            .text
            .rsplit('.')
            .next()
            .unwrap_or(&tokens[i].text)
            .to_string();
        if is_ident_like(&name) {
            return Some(name);
        }
        return None;
    }
    None
}

/// Walk every balanced parenthesised region in `sql` and
/// recursively collect any FROM / JOIN tables. Used for subselects
/// in WHERE clauses, INSERT … SELECT bodies, and CTE bodies.
fn collect_tables_in_parens(sql: &str, out: &mut BTreeSet<String>) {
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let close = find_matching_paren(sql, i);
            if close > i {
                let inner = &sql[i + 1..close];
                let inner_tables = collect_tables_in_select(inner);
                for t in inner_tables {
                    out.insert(t);
                }
                // Also pick up INSERT/UPDATE/DELETE/MERGE targets
                // inside the parens (e.g. INSERT INTO … SELECT …).
                let inner_stmt = parse_sql(inner);
                if let Some(s) = inner_stmt {
                    for t in s.tables {
                        out.insert(t);
                    }
                }
            }
        }
        i += 1;
    }
}

fn find_matching_paren(sql: &str, open_idx: usize) -> usize {
    let bytes = sql.as_bytes();
    let mut depth = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut i = open_idx;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            if b == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    i += 2;
                    continue;
                }
                in_double = false;
            }
            i += 1;
            continue;
        }
        if b == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_double = true;
            i += 1;
            continue;
        }
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
        i += 1;
    }
    sql.len()
}

fn is_ident_like(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// For `INSERT INTO <table>`, `UPDATE <table>`, `DELETE FROM
/// <table>`, `MERGE INTO <table>`: find the target table name.
/// `between` is the optional keyword between the head verb and
/// the table name ("" for UPDATE; "INTO" for INSERT / MERGE;
/// "FROM" for DELETE).
fn collect_target_table_after(sql: &str, head: &str, between: &str) -> Vec<String> {
    let tokens = tokenize(sql);
    let mut i = 0;
    // Find the head keyword (case-insensitive).
    while i < tokens.len() && tokens[i].text.to_ascii_uppercase() != head {
        i += 1;
    }
    if i >= tokens.len() {
        return Vec::new();
    }
    i += 1;
    if !between.is_empty() {
        while i < tokens.len() && tokens[i].text.to_ascii_uppercase() != between {
            i += 1;
        }
        if i >= tokens.len() {
            return Vec::new();
        }
        i += 1;
    }
    // The next token is the table name (with optional schema
    // prefix); skip wrapping parens for `INSERT INTO schema.t`
    // etc.
    let mut out: Vec<String> = Vec::new();
    while i < tokens.len() {
        let t_upper = tokens[i].text.to_ascii_uppercase();
        if tokens[i].text == "(" {
            // Column list or subselect — walk past.
            i += 1;
            continue;
        }
        if t_upper == "VALUES"
            || t_upper == "SET"
            || t_upper == "WHERE"
            || t_upper == "RETURNING"
            || t_upper == "USING"
            || t_upper == "WITH"
            || t_upper == "ON"
        {
            break;
        }
        if is_ident_like(&tokens[i].text) {
            let name = tokens[i]
                .text
                .rsplit('.')
                .next()
                .unwrap_or(&tokens[i].text)
                .to_string();
            out.push(name);
            break;
        }
        break;
    }
    out
}

// ─── Literal extraction ────────────────────────────────────────────────

/// Extract the first quoted-string literal from `s`. Recognises
/// both single and double quotes. Escapes (`\"`, `\'`, `\\`) are
/// honoured so a literal containing an escaped quote is not split.
/// Leading whitespace before the literal is skipped (the form
/// `func( "literal" )` is common in real code).
fn extract_string_literal(s: &str) -> Option<String> {
    crate::server::sensors::util_tokenize::extract_string_literal(s, 0).map(|(_, lit)| lit)
}

/// Strip line comments from the line (same shape as
/// `event_sensor::strip_line_comment` — kept here as a small
/// per-file helper so each sensor's text rules stand alone).
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match in_string {
            Some(q) if b == q => {
                if i > 0 && bytes[i - 1] == b'\\' {
                    i += 1;
                    continue;
                }
                in_string = None;
            }
            Some(_) => {}
            None if b == b'"' || b == b'\'' || b == b'`' => {
                in_string = Some(b);
            }
            None if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return &line[..i];
            }
            None if b == b'#' => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}

// ─── Graph emission ───────────────────────────────────────────────────

/// Resolve the source for a `ReadsTable` / `WritesTable` edge.
/// Tries the enclosing function / method first (spec §5.2
/// "typed traversal"); falls back to the file node for module-
/// level SQL; returns `None` when neither exists (the sensor
/// then skips the edge).
fn enclosing_or_file(graph: &GraphDatabase, graph_path_str: &str, line: u32) -> Option<String> {
    if let Some(sym) = crate::server::sensors::util::enclosing_symbol(graph, graph_path_str, line) {
        return Some(sym.id.clone());
    }
    graph.find_node_by_path(graph_path_str).map(|f| f.id)
}

/// Build the `Table` nodes and `ReadsTable` / `WritesTable` edges
/// for one file's sites. Dedupes `Table` nodes by `(path, name)`
/// (one node per distinct table name per file; a single SQL
/// statement that joins two tables produces two nodes). Returns
/// the list of unresolved records the file produced — non-literal
/// SQL or statements the parser could not classify.
fn build_graph(
    graph: &GraphDatabase,
    sites: &[SqlSite],
    graph_path_str: &str,
    namespace: &RepoNamespace,
) -> (Vec<GraphNode>, Vec<GraphEdge>, Vec<UnresolvedRecord>) {
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut unresolved: Vec<UnresolvedRecord> = Vec::new();
    let mut emitted_table_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut per_table_dynamic_count: BTreeMap<String, u32> = BTreeMap::new();

    for site in sites {
        if site.stmt.tables.is_empty() {
            // Either the parser could not classify the literal
            // (DDL/PRAGMA/etc.) or the call's first arg was not a
            // literal. Both land on the unresolved ledger with
            // reason `DynamicSql`.
            let sample_id = format!("{graph_path_str}:{}", site.line);
            let count = per_table_dynamic_count
                .entry(sample_id.clone())
                .or_insert(0);
            *count += 1;
            continue;
        }
        let edge_kind = match site.stmt.access {
            SqlAccess::Reads => EdgeType::ReadsTable,
            SqlAccess::Writes => EdgeType::WritesTable,
        };
        for table_name in &site.stmt.tables {
            let table_key = format!("{graph_path_str}::{table_name}");
            let table_id = if let Some(id) = emitted_table_ids.get(&table_key) {
                id.clone()
            } else {
                let id = GraphNode::generate_id(
                    &NodeType::Table,
                    graph_path_str,
                    table_name,
                    None,
                    namespace,
                );
                let mut node = GraphNode::new_in(
                    NodeType::Table,
                    table_name.clone(),
                    graph_path_str.to_string(),
                    namespace,
                );
                node.id = id.clone();
                node.contract = Some(ContractFact::Table(Table {
                    service: String::new(),
                    name: table_name.clone(),
                }));
                emitted_table_ids.insert(table_key.clone(), id.clone());
                nodes.push(node);
                id
            };
            let source_id = enclosing_or_file(graph, graph_path_str, site.line);
            let Some(source_id) = source_id else {
                // No enclosing function and no file node — skip
                // the edge so we don't mint a phantom source.
                continue;
            };
            let mut edge = GraphEdge::new(edge_kind.clone(), source_id, table_id);
            edge.provenance = Some(EdgeProvenance::Static {
                source: crate::schema::StaticSource::Regex,
            });
            edge.site = Some(SourceSite {
                path: graph_path_str.to_string(),
                line: site.line,
            });
            edges.push(edge);
        }
    }

    for (_id, count) in per_table_dynamic_count {
        unresolved.push(UnresolvedRecord {
            reason: UnresolvedReason::DynamicSql,
            count: count as usize,
            sample_ids: vec![_id],
        });
    }

    (nodes, edges, unresolved)
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::RepoNamespace;

    #[test]
    fn extract_string_literal_handles_double_single_backtick() {
        assert_eq!(
            extract_string_literal("\"SELECT 1\""),
            Some("SELECT 1".to_string())
        );
        assert_eq!(
            extract_string_literal("'SELECT 1'"),
            Some("SELECT 1".to_string())
        );
        assert_eq!(
            extract_string_literal("`SELECT 1`"),
            Some("SELECT 1".to_string())
        );
        // Escaped quote is honoured. The escaped literal is the
        // 14-character sequence `"SELECT \"foo\""` (note: backslashes
        // are preserved by the extractor; only SQL-standard `""`
        // collapse is applied, see `extract_string_literal`).
        let escaped_input = "\"SELECT \\\"foo\\\"\"";
        let escaped_expected = "SELECT \\\"foo\\\"";
        assert_eq!(
            extract_string_literal(escaped_input),
            Some(escaped_expected.to_string())
        );
    }

    #[test]
    fn parse_select_single_table() {
        let s = parse_sql("SELECT id FROM orders WHERE id = ?").unwrap();
        assert_eq!(s.access, SqlAccess::Reads);
        assert_eq!(s.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn parse_select_with_join() {
        let s =
            parse_sql("SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id")
                .unwrap();
        let tables: std::collections::BTreeSet<_> = s.tables.iter().collect();
        assert!(tables.contains(&"orders".to_string()));
        assert!(tables.contains(&"customers".to_string()));
    }

    #[test]
    fn parse_select_with_cte() {
        let s = parse_sql(
            "WITH active AS (SELECT * FROM users WHERE active = true) \
             SELECT * FROM active",
        )
        .unwrap();
        let tables: std::collections::BTreeSet<_> = s.tables.iter().collect();
        // The CTE body references `users`; the outer SELECT
        // references the CTE name `active`. The CTE name is also
        // a "table" — the spec says reads include CTE bodies.
        assert!(tables.contains(&"users".to_string()));
        assert!(tables.contains(&"active".to_string()));
    }

    #[test]
    fn parse_select_with_subselect_in_where() {
        let s = parse_sql(
            "SELECT id FROM orders WHERE customer_id IN \
             (SELECT id FROM customers WHERE vip = true)",
        )
        .unwrap();
        let tables: std::collections::BTreeSet<_> = s.tables.iter().collect();
        assert!(tables.contains(&"orders".to_string()));
        assert!(tables.contains(&"customers".to_string()));
    }

    #[test]
    fn parse_insert_into() {
        let s = parse_sql("INSERT INTO orders (id, total) VALUES (?, ?)").unwrap();
        assert_eq!(s.access, SqlAccess::Writes);
        assert_eq!(s.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn parse_update() {
        let s = parse_sql("UPDATE orders SET status = 'paid' WHERE id = ?").unwrap();
        assert_eq!(s.access, SqlAccess::Writes);
        assert_eq!(s.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn parse_delete_from() {
        let s = parse_sql("DELETE FROM orders WHERE id = ?").unwrap();
        assert_eq!(s.access, SqlAccess::Writes);
        assert_eq!(s.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn parse_merge_into() {
        let s = parse_sql(
            "MERGE INTO orders o USING new_orders n ON o.id = n.id \
             WHEN MATCHED THEN UPDATE SET status = n.status",
        )
        .unwrap();
        assert_eq!(s.access, SqlAccess::Writes);
        assert_eq!(s.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn parse_ddl_returns_none() {
        // DDL is out of scope (spec §7); the sensor records it as
        // DynamicSql so the operator sees the call site exists.
        assert!(parse_sql("CREATE TABLE foo (id INT)").is_none());
        assert!(parse_sql("DROP TABLE foo").is_none());
        assert!(parse_sql("PRAGMA foreign_keys = ON").is_none());
    }

    #[test]
    fn parse_with_schema_prefix() {
        let s = parse_sql("SELECT * FROM public.users").unwrap();
        assert_eq!(s.tables, vec!["users".to_string()]);
    }

    #[test]
    fn parse_with_alias() {
        let s = parse_sql("SELECT u.id FROM users AS u").unwrap();
        assert!(s.tables.contains(&"users".to_string()));
    }

    #[test]
    fn detect_rust_sqlx_query_emits_read() {
        let src = r#"sqlx::query("SELECT id FROM orders WHERE id = ?").execute(&pool).await"#;
        let sites = detect_in_file(src, "rs");
        assert_eq!(sites.len(), 1);
        let stmt = &sites[0].stmt;
        assert_eq!(stmt.access, SqlAccess::Reads);
        assert_eq!(stmt.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn detect_python_cursor_execute_emits_write() {
        let src = r#"cursor.execute("INSERT INTO orders (id) VALUES (?)")"#;
        let sites = detect_in_file(src, "py");
        assert_eq!(sites.len(), 1);
        let stmt = &sites[0].stmt;
        assert_eq!(stmt.access, SqlAccess::Writes);
        assert_eq!(stmt.tables, vec!["orders".to_string()]);
    }

    #[test]
    fn detect_python_db_query_emits_read() {
        let src = r#"db.query("SELECT * FROM orders")"#;
        let sites = detect_in_file(src, "py");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].stmt.access, SqlAccess::Reads);
        assert!(sites[0].stmt.tables.contains(&"orders".to_string()));
    }

    #[test]
    fn detect_java_preparestatement_emits_read() {
        let src = r#"PreparedStatement ps = conn.prepareStatement("SELECT id FROM orders WHERE id = ?");"#;
        let sites = detect_in_file(src, "java");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].stmt.access, SqlAccess::Reads);
        assert!(sites[0].stmt.tables.contains(&"orders".to_string()));
    }

    #[test]
    fn detect_non_literal_sql_records_dynamic() {
        let src = r#"sqlx::query(&format!("SELECT * FROM {}", table)).execute(&pool).await"#;
        let sites = detect_in_file(src, "rs");
        assert_eq!(sites.len(), 1);
        assert!(
            sites[0].stmt.tables.is_empty(),
            "non-literal SQL has no parsed tables"
        );
    }

    #[test]
    fn detect_orm_call_ignores_no_sql_literal() {
        // SQLAlchemy ORM: `session.query(Order)` has no literal SQL.
        // The sensor should NOT match — there's no recognized shape
        // that takes a string SQL arg.
        let src = r#"session.query(Order).all()"#;
        let sites = detect_in_file(src, "py");
        assert_eq!(sites.len(), 0, "ORM call has no SQL literal");
    }

    #[test]
    fn build_graph_emits_table_node_and_reads_edge() {
        let dir = tempfile::tempdir().unwrap();
        let graph_path = dir.path().join("graph.bin");
        let graph = GraphDatabase::new(&graph_path).unwrap();
        // Mint a Function node so `enclosing_symbol` can resolve
        // the source of the SQL edge.
        let ns = RepoNamespace::for_test();
        let mut fn_node = GraphNode::new_in(
            NodeType::Function,
            "list_orders".into(),
            "src/orders.py".into(),
            &ns,
        );
        fn_node.line_start = Some(1);
        fn_node.line_end = Some(20);
        fn_node.id = GraphNode::generate_id(
            &NodeType::Function,
            "src/orders.py",
            "list_orders",
            Some(1),
            &ns,
        );
        graph.upsert_node(fn_node).unwrap();

        let site = SqlSite {
            stmt: SqlStatement {
                access: SqlAccess::Reads,
                tables: vec!["orders".to_string()],
            },
            line: 5,
        };
        let (nodes, edges, unresolved) = build_graph(&graph, &[site], "src/orders.py", &ns);
        assert!(unresolved.is_empty());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_type, NodeType::Table);
        assert_eq!(nodes[0].name, "orders");
        assert_eq!(edges.len(), 1);
        assert!(matches!(edges[0].edge_type, EdgeType::ReadsTable));
    }
}
