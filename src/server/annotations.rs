//! Per-repo annotation + handoff storage.
//!
//! Implementation note: storage is a per-repo SQLite table
//! (`<state_dir>/annotations/<repo>.sqlite`) keyed by `id`, indexed
//! on `(target_kind, target_id)`, `author`, and `status` — the
//! exact shape `docs/M4-step-8-plan.md` §4.2 prescribes. Every
//! mutation is one SQL statement inside a `parking_lot::Mutex` on
//! the connection; the lock is held for the duration of the
//! statement only (rusqlite::Connection is `!Sync`, so we serialize
//! via the mutex instead of wrapping in `Arc<Mutex<...>>`).
//!
//! Module surface is intentionally small — five write/read operations
//! match the five MCP tools, plus a registry that maps `RepoId` to
//! its per-repo store. Stale detection (§4.6 of the plan) lives at
//! the call site: the registry passes a `&dyn Fn(&AnnotationTarget)`
//! resolver into the store, so the sqlite layer never imports the
//! federation backend.

use crate::error::LainError;
use crate::federation::repo_id::RepoId;
use crate::server::path_util::posix_string;
use crate::server::presence::AgentId;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

/// Maximum annotation body size, in bytes. Validated at the
/// boundary; rows over the cap are rejected before the SQL write.
pub const MAX_BODY_BYTES: usize = 4096;

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Tags an `AnnotationTarget` with a stable, canonical id string so
/// `list_annotations` can index and filter on it. The conversion
/// rules:
/// - `Symbol` / `File` / `Repo` use their underlying string
///   verbatim (after path normalization for `File`, see
///   `canonical_target_id`).
/// - `Edge` uses `"<from> -> <to>"` so the same edge from two
///   different agents matches.
fn canonical_target_id(target: &AnnotationTarget) -> String {
    match target {
        AnnotationTarget::Symbol { symbol } => format!("symbol:{symbol}"),
        AnnotationTarget::File { file } => format!("file:{}", canonical_file(file)),
        AnnotationTarget::Repo { repo_id } => format!("repo:{repo_id}"),
        AnnotationTarget::Edge { from, to } => format!("edge:{from} -> {to}"),
    }
}

/// Path target strings come in via `TargetSpec::File { file }`. The
/// agent sends whatever `explain_symbol` returns, which is
/// `posix_string`-normalized (workspace-relative, forward
/// slashes). Re-normalizing here is the safety net for agents that
/// bypass `TargetSpec` and pass raw paths.
///
/// `..` segments and absolute paths are rejected: an annotation's
/// `target_id` lands in a sqlite row that other agents later
/// query by. A `target_id` of `../../etc/passwd` would be a
/// confusing row on Linux but a real path-traversal footgun on
/// Windows where backslashes (re-normalized to forward by
/// `posix_string`) and `..` segments combine to escape the
/// workspace root in any tool that later re-resolves the id.
///
/// Absolute-ness is checked by hand rather than via `Path::is_absolute()`:
/// that method is platform-relative, so a Unix-style `/etc/passwd`
/// target is (correctly) rejected when this runs on Linux but is
/// *not* absolute by Windows' path parser (no drive letter), which
/// would let it slip through on a Windows build. The server accepts
/// paths from any agent regardless of the host OS, so both a
/// leading `/` or `\` and a Windows drive-letter prefix (`C:`) are
/// rejected unconditionally.
fn canonical_file(p: &str) -> String {
    let is_drive_absolute = p.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && p.as_bytes().get(1) == Some(&b':');
    if p.contains("..") || p.starts_with('/') || p.starts_with('\\') || is_drive_absolute {
        return String::new();
    }
    posix_string(Path::new(p))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnnotationTarget {
    Symbol { symbol: String },
    Edge { from: String, to: String },
    File { file: String },
    Repo { repo_id: String },
}

impl AnnotationTarget {
    pub fn target_kind_str(&self) -> &'static str {
        match self {
            Self::Symbol { .. } => "symbol",
            Self::Edge { .. } => "edge",
            Self::File { .. } => "file",
            Self::Repo { .. } => "repo",
        }
    }
}

/// Discriminator for what an annotation is *about*. Mirrors the
/// kinds `Agent UX roadmap` §M4 step 8 calls out: a free-form note,
/// a code review warning, an explicit todo, a "we should
/// investigate this" marker, and a "this is the fix for X" tag.
/// Stored as snake_case strings in SQLite so agents can grep the
/// log without parsing Rust enums.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    Note,
    Warning,
    Todo,
    Investigation,
    Fix,
}

impl AnnotationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Warning => "warning",
            Self::Todo => "todo",
            Self::Investigation => "investigation",
            Self::Fix => "fix",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "note" => Some(Self::Note),
            "warning" => Some(Self::Warning),
            "todo" => Some(Self::Todo),
            "investigation" => Some(Self::Investigation),
            "fix" => Some(Self::Fix),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationStatus {
    Open,
    Resolved,
    Stale,
}

impl AnnotationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Stale => "stale",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "resolved" => Some(Self::Resolved),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }
}

/// A single agent-side annotation. Lives in the per-repo SQLite
/// table. Serializes to the wire shape `list_annotations` returns
/// directly, with cross-references denormalized into a JSON array
/// under `refs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Annotation {
    pub id: String,
    pub target_kind: String,
    pub target_id: String,
    pub kind: String,
    pub body: String,
    pub author: AgentId,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub status: String,
    pub resolved_by: Option<AgentId>,
    pub resolved_at_unix_ms: Option<u64>,
    pub refs: Vec<AnnotationTarget>,
}

/// A trimmed-down view of [`Annotation`] suitable for embedding in
/// `explain_symbol` / `get_blast_radius` output. Truncates `body`
/// at 240 bytes so the rendered markdown stays legible; the agent
/// can call `list_annotations` with the id to fetch the full
/// record.
///
/// Note: this is a byte limit, not a character limit — see the
/// `from_full` impl for the rationale. UTF-8 boundary-safe, so we
/// never slice through a multi-byte sequence, but a 4-byte emoji
/// may still truncate after ~60 visible characters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotationSummary {
    pub id: String,
    pub target_kind: String,
    pub target_id: String,
    pub kind: String,
    pub status: String,
    pub body_excerpt: String,
    pub author: String,
    pub created_at_unix_ms: u64,
}

impl AnnotationSummary {
    pub fn from_full(a: &Annotation) -> Self {
        // 240 *bytes*, not characters. A char limit would force
        // a full UTF-8 walk per annotation; the markdown excerpt
        // is bounded by line length anyway, and walking back to
        // a char boundary on truncation keeps the excerpt valid
        // UTF-8 even when a multi-byte character straddles the
        // 240-byte boundary.
        const EXCERPT_LIMIT: usize = 240;
        let excerpt = if a.body.len() <= EXCERPT_LIMIT {
            a.body.clone()
        } else {
            // Char boundary safe: walk back to a UTF-8 boundary so
            // we never slice through a multi-byte sequence.
            let mut idx = EXCERPT_LIMIT;
            while !a.body.is_char_boundary(idx) && idx > 0 {
                idx -= 1;
            }
            format!("{}…", &a.body[..idx])
        };
        Self {
            id: a.id.clone(),
            target_kind: a.target_kind.clone(),
            target_id: a.target_id.clone(),
            kind: a.kind.clone(),
            status: a.status.clone(),
            body_excerpt: excerpt,
            author: a.author.as_str().to_string(),
            created_at_unix_ms: a.created_at_unix_ms,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListFilter {
    pub target: Option<AnnotationTarget>,
    pub author: Option<AgentId>,
    pub kind: Option<AnnotationKind>,
    pub status: Option<AnnotationStatus>,
    pub limit: Option<u32>,
}

/// Filter plus the live-resolver hook the registry injects so
/// `list_with_staleness` can mark targets that no longer exist in
/// the graph as `Stale` without the sqlite layer needing to import
/// the federation backend.
pub struct ListQuery<'a> {
    pub filter: &'a ListFilter,
    /// Returns `true` if the target still exists in the live graph
    /// (or the workspace, for `repo` targets). Targets the resolver
    /// returns `false` for are emitted with `status = "stale"`.
    pub exists: &'a dyn Fn(&AnnotationTarget) -> bool,
}

/// Per-repo store. Owns one SQLite connection, guards every
/// operation with a `parking_lot::Mutex` (rusqlite::Connection is
/// `!Sync`). Cheap to clone behind an `Arc` because all fields are
/// behind the same Arc.
pub struct AnnotationStore {
    conn: parking_lot::Mutex<Connection>,
}

impl AnnotationStore {
    pub fn open(path: &Path) -> Result<Self, LainError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LainError::Io(e.to_string()))?;
        }
        let conn = Connection::open(path)
            .map_err(|e| LainError::Other(format!("annotation sqlite open: {e}")))?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| LainError::Other(format!("annotation sqlite migrate: {e}")))?;
        Ok(Self {
            conn: parking_lot::Mutex::new(conn),
        })
    }

    /// Validate body length and write the annotation. `INSERT OR
    /// REPLACE` so a re-add with the same id is idempotent (useful
    /// for test fixtures and for the auto-include path that runs on
    /// every `explain_symbol` call).
    pub fn add(&self, a: &Annotation) -> Result<(), LainError> {
        if a.body.is_empty() {
            return Err(LainError::Other("annotation body must not be empty".into()));
        }
        if a.body.len() > MAX_BODY_BYTES {
            return Err(LainError::Other(format!(
                "annotation body exceeds {MAX_BODY_BYTES} bytes"
            )));
        }
        let refs_json = serde_json::to_string(&a.refs).unwrap_or_else(|_| "[]".into());
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO annotations \
             (id, target_kind, target_id, kind, body, author, \
              created_at, updated_at, status, resolved_by, resolved_at, refs_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                a.id,
                a.target_kind,
                a.target_id,
                a.kind,
                a.body,
                a.author.as_str(),
                a.created_at_unix_ms as i64,
                a.updated_at_unix_ms as i64,
                a.status,
                a.resolved_by.as_ref().map(|a| a.as_str().to_string()),
                a.resolved_at_unix_ms.map(|v| v as i64),
                refs_json,
            ],
        )
        .map_err(|e| LainError::Other(format!("annotation sqlite insert: {e}")))?;
        Ok(())
    }

    /// Read with the live-resolver hook so targets that no longer
    /// exist in the graph are returned with `status = "stale"`.
    /// Limit applies *before* staleness re-classification, so the
    /// caller doesn't get a stale entry when an open one would have
    /// fit in the page.
    ///
    /// Status filter: the SQL `WHERE status = ?` is intentionally
    /// *only* applied for `Resolved`. For `Open`, we read every
    /// open row and let the live reclassification down-grade rows
    /// whose target no longer exists in the graph — applying the
    /// SQL filter for `Open` would silently drop rows that should
    /// become `stale` on this read. For `Stale`, we read every row
    /// (open + resolved + stale) and post-filter on the
    /// reclassified status, so a caller querying `status = stale`
    /// sees both DB-persisted and freshly-reclassified rows.
    pub fn list_with_staleness(&self, q: &ListQuery<'_>) -> Result<Vec<Annotation>, LainError> {
        let mut sql = String::from(
            "SELECT id, target_kind, target_id, kind, body, author, \
                    created_at, updated_at, status, resolved_by, resolved_at, refs_json \
             FROM annotations WHERE 1 = 1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(target) = &q.filter.target {
            sql.push_str(" AND target_kind = ? AND target_id = ?");
            args.push(Box::new(target.target_kind_str().to_string()));
            args.push(Box::new(canonical_target_id(target)));
        }
        if let Some(author) = &q.filter.author {
            sql.push_str(" AND author = ?");
            args.push(Box::new(author.as_str().to_string()));
        }
        if let Some(kind) = q.filter.kind {
            sql.push_str(" AND kind = ?");
            args.push(Box::new(kind.as_str().to_string()));
        }
        if let Some(status) = q.filter.status {
            // Only pre-filter on `Resolved` — see the doc comment
            // above for why Open/Stale must be post-filtered.
            if matches!(status, AnnotationStatus::Resolved) {
                sql.push_str(" AND status = ?");
                args.push(Box::new(status.as_str().to_string()));
            }
        }
        sql.push_str(" ORDER BY created_at DESC, id ASC");
        let limit = q.filter.limit.unwrap_or(100).min(1000);
        sql.push_str(" LIMIT ?");
        args.push(Box::new(limit as i64));

        let conn = self.conn.lock();
        let params_refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| LainError::Other(format!("annotation sqlite prepare: {e}")))?;
        let mut rows = stmt
            .query(params_refs.as_slice())
            .map_err(|e| LainError::Other(format!("annotation sqlite query: {e}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|e| LainError::Other(format!("annotation sqlite row: {e}")))?
        {
            let mut a = row_to_annotation(row)?;
            if a.status == AnnotationStatus::Open.as_str() {
                if let Some(target) = annotation_target_from_row(&a) {
                    if !(q.exists)(&target) {
                        a.status = AnnotationStatus::Stale.as_str().to_string();
                    }
                } else {
                    // Unknown target_kind on disk shouldn't happen,
                    // but treat as stale defensively so callers see
                    // it instead of acting on malformed data.
                    a.status = AnnotationStatus::Stale.as_str().to_string();
                }
            }
            // Post-filter: caller asked for `Open` or `Stale`,
            // reclassify the result here so freshly-downgraded
            // rows are returned alongside DB-persisted ones.
            if let Some(requested) = q.filter.status {
                let matches = match requested {
                    AnnotationStatus::Resolved => a.status == AnnotationStatus::Resolved.as_str(),
                    AnnotationStatus::Open => a.status == AnnotationStatus::Open.as_str(),
                    AnnotationStatus::Stale => a.status == AnnotationStatus::Stale.as_str(),
                };
                if !matches {
                    continue;
                }
            }
            out.push(a);
        }
        Ok(out)
    }

    pub fn resolve(&self, id: &str, by: &AgentId) -> Result<Annotation, LainError> {
        let conn = self.conn.lock();
        let now = unix_ms() as i64;
        let updated = conn
            .execute(
                "UPDATE annotations \
                 SET status = 'resolved', resolved_by = ?1, resolved_at = ?2, updated_at = ?2 \
                 WHERE id = ?3 AND status = 'open'",
                params![by.as_str(), now, id],
            )
            .map_err(|e| LainError::Other(format!("annotation sqlite update: {e}")))?;
        if updated == 0 {
            return Err(LainError::NotFound(format!(
                "annotation {id} not found or already resolved"
            )));
        }
        let row = conn
            .query_row(
                "SELECT id, target_kind, target_id, kind, body, author, \
                        created_at, updated_at, status, resolved_by, resolved_at, refs_json \
                 FROM annotations WHERE id = ?1",
                params![id],
                |row| Ok(row_to_annotation(row)),
            )
            .optional()
            .map_err(|e| LainError::Other(format!("annotation sqlite select: {e}")))?;
        match row {
            Some(Ok(a)) => Ok(a),
            Some(Err(e)) => Err(e),
            None => Err(LainError::NotFound(format!("annotation {id} not found"))),
        }
    }

    pub fn get(&self, id: &str) -> Result<Option<Annotation>, LainError> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT id, target_kind, target_id, kind, body, author, \
                        created_at, updated_at, status, resolved_by, resolved_at, refs_json \
                 FROM annotations WHERE id = ?1",
                params![id],
                |row| Ok(row_to_annotation(row)),
            )
            .optional()
            .map_err(|e| LainError::Other(format!("annotation sqlite select: {e}")))?;
        row.transpose().map_err(|e: LainError| e)
    }
}

/// Reconstruct a [`AnnotationTarget`] from a row's `target_kind`
/// + `target_id` columns. Used by the live-staleness pass on read.
///
/// Returns `None` if the row has an unrecognized `target_kind` (the
/// caller treats this as stale).
fn annotation_target_from_row(a: &Annotation) -> Option<AnnotationTarget> {
    let id = &a.target_id;
    let stripped = id.strip_prefix(&format!("{}:", a.target_kind))?;
    match a.target_kind.as_str() {
        "symbol" => Some(AnnotationTarget::Symbol {
            symbol: stripped.to_string(),
        }),
        "file" => Some(AnnotationTarget::File {
            file: stripped.to_string(),
        }),
        "repo" => Some(AnnotationTarget::Repo {
            repo_id: stripped.to_string(),
        }),
        "edge" => {
            // Stored as "edge:<from> -> <to>".
            let payload = stripped.strip_prefix("edge:").unwrap_or(stripped);
            let (from, to) = payload.split_once(" -> ")?;
            Some(AnnotationTarget::Edge {
                from: from.to_string(),
                to: to.to_string(),
            })
        }
        _ => None,
    }
}

fn row_to_annotation(row: &rusqlite::Row<'_>) -> Result<Annotation, LainError> {
    let id: String = row
        .get(0)
        .map_err(|e| LainError::Other(format!("annotation sqlite col id: {e}")))?;
    let target_kind: String = row
        .get(1)
        .map_err(|e| LainError::Other(format!("annotation sqlite col kind: {e}")))?;
    let target_id: String = row
        .get(2)
        .map_err(|e| LainError::Other(format!("annotation sqlite col tid: {e}")))?;
    let kind: String = row
        .get(3)
        .map_err(|e| LainError::Other(format!("annotation sqlite col kind2: {e}")))?;
    let body: String = row
        .get(4)
        .map_err(|e| LainError::Other(format!("annotation sqlite col body: {e}")))?;
    let author: String = row
        .get(5)
        .map_err(|e| LainError::Other(format!("annotation sqlite col author: {e}")))?;
    let created_at: i64 = row
        .get(6)
        .map_err(|e| LainError::Other(format!("annotation sqlite col created_at: {e}")))?;
    let updated_at: i64 = row
        .get(7)
        .map_err(|e| LainError::Other(format!("annotation sqlite col updated_at: {e}")))?;
    let status: String = row
        .get(8)
        .map_err(|e| LainError::Other(format!("annotation sqlite col status: {e}")))?;
    let resolved_by: Option<String> = row
        .get(9)
        .map_err(|e| LainError::Other(format!("annotation sqlite col resolved_by: {e}")))?;
    let resolved_at: Option<i64> = row
        .get(10)
        .map_err(|e| LainError::Other(format!("annotation sqlite col resolved_at: {e}")))?;
    let refs_json: String = row
        .get(11)
        .map_err(|e| LainError::Other(format!("annotation sqlite col refs_json: {e}")))?;
    let refs: Vec<AnnotationTarget> = serde_json::from_str(&refs_json)
        .map_err(|e| LainError::Other(format!("refs parse: {e}")))?;
    Ok(Annotation {
        id,
        target_kind,
        target_id,
        kind,
        body,
        author: AgentId(author),
        created_at_unix_ms: created_at.max(0) as u64,
        updated_at_unix_ms: updated_at.max(0) as u64,
        status,
        resolved_by: resolved_by.map(AgentId),
        resolved_at_unix_ms: resolved_at.map(|v| v.max(0) as u64),
        refs,
    })
}

/// SQLite DDL applied on every `AnnotationStore::open`. Idempotent
/// — `CREATE TABLE IF NOT EXISTS` makes a second open a no-op.
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS annotations (
    id          TEXT PRIMARY KEY,
    target_kind TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    kind        TEXT NOT NULL,
    body        TEXT NOT NULL,
    author      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'open',
    resolved_by TEXT,
    resolved_at INTEGER,
    refs_json   TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS annotations_target ON annotations(target_kind, target_id);
CREATE INDEX IF NOT EXISTS annotations_author ON annotations(author);
CREATE INDEX IF NOT EXISTS annotations_status ON annotations(status);
"#;

/// Federation-wide registry. One store per `RepoId`; opens new
/// stores on demand under `<state_dir>/annotations/<repo>.sqlite`.
/// Wrapped in `parking_lot::RwLock` so concurrent reads don't
/// serialize on the registry lock.
pub struct AnnotationRegistry {
    state_dir: PathBuf,
    stores: parking_lot::RwLock<HashMap<RepoId, Arc<AnnotationStore>>>,
}

impl AnnotationRegistry {
    pub fn open(state_dir: &Path) -> Result<Self, LainError> {
        let dir = state_dir.join("annotations");
        std::fs::create_dir_all(&dir).map_err(|e| LainError::Io(e.to_string()))?;
        Ok(Self {
            state_dir: state_dir.to_path_buf(),
            stores: parking_lot::RwLock::new(HashMap::new()),
        })
    }

    /// Best-effort constructor for the `LainServer` field.
    ///
    /// `open` requires a writable state dir; an I/O failure (a
    /// full disk, a permission-denied path, the audit-integration
    /// test's blocker-file scenario) must NOT panic the server.
    /// Returns a frozen empty registry that every per-repo
    /// `store_for` call translates into a "storage unavailable"
    /// error on the MCP tool path. This mirrors the audit log's
    /// "best-effort, never block an edit" invariant.
    pub fn open_best_effort(state_dir: &Path) -> Arc<Self> {
        match Self::open(state_dir) {
            Ok(reg) => Arc::new(reg),
            Err(e) => {
                eprintln!(
                    "annotation registry open failed at {state_dir:?}: {e} — \
                     annotation tools will return storage errors until the \
                     state directory is writable"
                );
                Arc::new(Self {
                    state_dir: state_dir.to_path_buf(),
                    stores: parking_lot::RwLock::new(HashMap::new()),
                })
            }
        }
    }

    pub fn store_for(&self, repo: &RepoId) -> Result<Arc<AnnotationStore>, LainError> {
        if let Some(existing) = self.stores.read().get(repo).cloned() {
            return Ok(existing);
        }
        let mut stores = self.stores.write();
        if let Some(existing) = stores.get(repo).cloned() {
            return Ok(existing);
        }
        let path = self
            .state_dir
            .join("annotations")
            .join(format!("{}.sqlite", repo.as_str()));
        let store = Arc::new(AnnotationStore::open(&path)?);
        stores.insert(repo.clone(), store.clone());
        Ok(store)
    }

    /// Cross-repo listing. `exists` is the live-resolver hook the
    /// caller passes in so the registry doesn't need to know about
    /// the federation backend.
    pub fn list_all(
        &self,
        repos: &[RepoId],
        filter: &ListFilter,
        exists: &dyn Fn(&AnnotationTarget) -> bool,
    ) -> Result<Vec<Annotation>, LainError> {
        let mut out = Vec::new();
        for repo in repos {
            let store = self.store_for(repo)?;
            let q = ListQuery { filter, exists };
            let mut from_repo = store.list_with_staleness(&q)?;
            out.append(&mut from_repo);
        }
        // Stable order: newest first across all repos.
        out.sort_by_key(|a| std::cmp::Reverse(a.created_at_unix_ms));
        if let Some(limit) = filter.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }
}

/// Inputs the `add_annotation` MCP tool accepts, after deserializing
/// the JSON-RPC params. The dispatcher maps each field onto
/// [`Annotation`] (minting id + timestamps server-side).
#[derive(Debug, Clone)]
pub struct AddAnnotationInputs {
    pub target: AnnotationTarget,
    pub kind: AnnotationKind,
    pub body: String,
    pub author: AgentId,
    pub refs: Vec<AnnotationTarget>,
}

impl AddAnnotationInputs {
    pub fn into_annotation(self) -> Annotation {
        let now = unix_ms();
        let target_id = canonical_target_id(&self.target);
        let target_kind = self.target.target_kind_str().to_string();
        Annotation {
            id: new_id(),
            target_kind,
            target_id,
            kind: self.kind.as_str().to_string(),
            body: self.body,
            author: self.author,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            status: AnnotationStatus::Open.as_str().to_string(),
            resolved_by: None,
            resolved_at_unix_ms: None,
            refs: self.refs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn open_store() -> (tempfile::TempDir, AnnotationStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = AnnotationStore::open(&tmp.path().join("ann.sqlite")).unwrap();
        (tmp, store)
    }

    #[test]
    fn round_trip_open_close_reopen() {
        // Open, write, drop, reopen — the row must still be there.
        let (tmp, store) = open_store();
        let a = AddAnnotationInputs {
            target: AnnotationTarget::Symbol {
                symbol: "fn a".into(),
            },
            kind: AnnotationKind::Note,
            body: "hi".into(),
            author: AgentId("alice".into()),
            refs: vec![],
        }
        .into_annotation();
        store.add(&a).unwrap();

        let store2 = AnnotationStore::open(&tmp.path().join("ann.sqlite")).unwrap();
        let q = ListQuery {
            filter: &ListFilter::default(),
            exists: &|_| true,
        };
        let rows = store2.list_with_staleness(&q).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, a.id);
    }

    #[test]
    fn reject_empty_and_oversize_body() {
        let (_tmp, store) = open_store();
        let mut a = AddAnnotationInputs {
            target: AnnotationTarget::Symbol {
                symbol: "fn a".into(),
            },
            kind: AnnotationKind::Note,
            body: "ok".into(),
            author: AgentId("alice".into()),
            refs: vec![],
        }
        .into_annotation();
        a.body = String::new();
        assert!(matches!(store.add(&a), Err(LainError::Other(_))));
        a.body = "x".repeat(MAX_BODY_BYTES + 1);
        assert!(matches!(store.add(&a), Err(LainError::Other(_))));
    }

    #[test]
    fn list_filter_status_post_filtered_for_open_and_stale() {
        // Copilot-review fix: pre-fix bug applied the SQL
        // `WHERE status = ?` for every status value, which meant
        // a caller filtering for `Open` never saw freshly-
        // reclassified `Stale` rows, and a caller filtering for
        // `Stale` saw zero rows because live reclassification
        // happens AFTER the SQL filter. Both must now return
        // post-filtered results.
        let (_tmp, store) = open_store();
        let make = |target: AnnotationTarget, kind: AnnotationKind, body: &str, author: &str| {
            AddAnnotationInputs {
                target,
                kind,
                body: body.into(),
                author: AgentId(author.into()),
                refs: vec![],
            }
            .into_annotation()
        };
        // One row whose target exists, one whose doesn't.
        store
            .add(&make(
                AnnotationTarget::Symbol {
                    symbol: "exists".into(),
                },
                AnnotationKind::Note,
                "live",
                "alice",
            ))
            .unwrap();
        store
            .add(&make(
                AnnotationTarget::Symbol {
                    symbol: "ghost".into(),
                },
                AnnotationKind::Note,
                "orphan",
                "alice",
            ))
            .unwrap();

        // Resolver returns true (everything exists): both rows
        // are Open. Filtering for `Open` returns both.
        let q_open = ListQuery {
            filter: &ListFilter {
                status: Some(AnnotationStatus::Open),
                limit: Some(10),
                ..Default::default()
            },
            exists: &|_| true,
        };
        let rows = store.list_with_staleness(&q_open).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.status == "open"));

        // Resolver returns false (everything is stale): both
        // rows reclassified. Filtering for `Stale` returns both,
        // filtering for `Open` returns zero.
        let q_false = ListQuery {
            filter: &ListFilter {
                status: Some(AnnotationStatus::Stale),
                limit: Some(10),
                ..Default::default()
            },
            exists: &|_| false,
        };
        let rows = store.list_with_staleness(&q_false).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.status == "stale"));

        let q_open_false = ListQuery {
            filter: &ListFilter {
                status: Some(AnnotationStatus::Open),
                limit: Some(10),
                ..Default::default()
            },
            exists: &|_| false,
        };
        let rows = store.list_with_staleness(&q_open_false).unwrap();
        assert_eq!(rows.len(), 0, "Open filter must drop freshly-stale rows");
    }

    #[test]
    fn list_filter_by_target_kind_and_author() {
        let (_tmp, store) = open_store();
        let make = |target: AnnotationTarget, kind: AnnotationKind, body: &str, author: &str| {
            AddAnnotationInputs {
                target,
                kind,
                body: body.into(),
                author: AgentId(author.into()),
                refs: vec![],
            }
            .into_annotation()
        };
        store
            .add(&make(
                AnnotationTarget::Symbol {
                    symbol: "fn a".into(),
                },
                AnnotationKind::Note,
                "first",
                "alice",
            ))
            .unwrap();
        store
            .add(&make(
                AnnotationTarget::Symbol {
                    symbol: "fn b".into(),
                },
                AnnotationKind::Warning,
                "second",
                "bob",
            ))
            .unwrap();
        store
            .add(&make(
                AnnotationTarget::File {
                    file: "src/lib.rs".into(),
                },
                AnnotationKind::Todo,
                "third",
                "alice",
            ))
            .unwrap();

        let f_alice = ListFilter {
            author: Some(AgentId("alice".into())),
            ..Default::default()
        };
        let q = ListQuery {
            filter: &f_alice,
            exists: &|_| true,
        };
        let rows = store.list_with_staleness(&q).unwrap();
        assert_eq!(rows.len(), 2);
        let authors: HashSet<&str> = rows.iter().map(|r| r.author.as_str()).collect();
        assert!(authors.contains("alice"));
        assert!(!authors.contains("bob"));

        let f_symbol = ListFilter {
            target: Some(AnnotationTarget::Symbol {
                symbol: "fn a".into(),
            }),
            ..Default::default()
        };
        let q = ListQuery {
            filter: &f_symbol,
            exists: &|_| true,
        };
        let rows = store.list_with_staleness(&q).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].body, "first");
    }

    #[test]
    fn canonical_file_rejects_traversal_and_absolute_paths() {
        // Path-traversal protection: a `..` segment or absolute
        // path on `File` targets is rejected up front so a row
        // can't escape the workspace root in tools that later
        // re-resolve the stored id. The `..` check is what
        // matters on every platform; the `is_absolute()` check
        // catches Unix absolute paths directly and (on Windows)
        // drive-letter absolute paths via the platform's path
        // parser.
        assert_eq!(canonical_file("../../etc/passwd"), "");
        assert_eq!(canonical_file("src/../lib.rs"), "");
        assert_eq!(canonical_file("/etc/passwd"), "");
        // Legitimate workspace-relative paths are preserved.
        assert_eq!(canonical_file("src/lib.rs"), "src/lib.rs");
        assert_eq!(canonical_file("src/sub/mod.rs"), "src/sub/mod.rs");
    }

    #[test]
    fn staleness_marks_targets_that_no_longer_exist() {
        let (_tmp, store) = open_store();
        let a = AddAnnotationInputs {
            target: AnnotationTarget::Symbol {
                symbol: "fn will_disappear".into(),
            },
            kind: AnnotationKind::Note,
            body: "stale me".into(),
            author: AgentId("alice".into()),
            refs: vec![],
        }
        .into_annotation();
        store.add(&a).unwrap();

        // Resolver returns true: row stays open.
        let q_true = ListQuery {
            filter: &ListFilter::default(),
            exists: &|_| true,
        };
        let rows = store.list_with_staleness(&q_true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "open");

        // Resolver returns false: row is marked stale.
        let q_false = ListQuery {
            filter: &ListFilter::default(),
            exists: &|_| false,
        };
        let rows = store.list_with_staleness(&q_false).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "stale");

        // Resolution still works on stale rows.
        let rows = store.list_with_staleness(&q_true).unwrap();
        let r = &store
            .resolve(&rows[0].id, &AgentId("alice".into()))
            .unwrap();
        assert_eq!(r.status, "resolved");
        assert_eq!(r.resolved_by.as_ref().unwrap().as_str(), "alice");
    }

    #[test]
    fn excerpt_truncates_at_a_utf8_boundary() {
        // 4-byte emoji at the boundary; the truncation must not slice
        // through it.
        let body = format!("{}😀", "x".repeat(239));
        let a = AddAnnotationInputs {
            target: AnnotationTarget::Symbol {
                symbol: "fn e".into(),
            },
            kind: AnnotationKind::Note,
            body: body.clone(),
            author: AgentId("alice".into()),
            refs: vec![],
        }
        .into_annotation();
        let s = AnnotationSummary::from_full(&a);
        assert!(s.body_excerpt.ends_with('…'));
        assert!(s.body_excerpt.len() <= 240 + "…".len());
        // Reconstructed prefix must equal the first 239 chars of the body.
        let prefix = &body[..239];
        assert!(s.body_excerpt.starts_with(prefix));
    }
}
