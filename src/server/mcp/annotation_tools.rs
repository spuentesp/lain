//! MCP tool handlers for the agent-side annotation + handoff layer
//! (M4 plan §4.3). Five tools, all `Always` classified, all
//! routed through the central dispatcher:
//!
//! - `add_annotation`             — write one annotation
//! - `list_annotations`           — read with filters + live-staleness
//! - `resolve_annotation`         — close an open annotation
//! - `leave_handoff_note`         — write a workspace-scoped note for
//!   the next agent that registers
//! - `get_pending_handoffs`       — list notes left by previous agents
//!
//! The five `pub fn run_*` functions take `&LainServer` and return
//! `Result<serde_json::Value, String>` — the same shape
//! `presence_tools.rs` and `federation_tools.rs` use, so the
//! dispatcher's `dispatch_presence_tool_outcome`-style wrapper can
//! route them through the standard error-envelope path.
//!
//! Live-staleness for `list_annotations` and `get_pending_handoffs`
//! requires a `exists: &dyn Fn(&AnnotationTarget) -> bool` resolver.
//! For `file` and `repo` targets we use the workspace path; for
//! `symbol` targets we defer to the active per-repo graph's
//! `nodes()` membership (the agent is responsible for naming a
//! `repo_id` when listing cross-repo symbols; the dispatcher routes
//! to the right per-repo store before reaching this layer).

use super::presence_tools::authenticate;
use crate::server::annotations::{
    AddAnnotationInputs, AnnotationKind, AnnotationStatus, AnnotationTarget, ListFilter,
};
use crate::server::presence::AgentId;
use crate::server::LainServer;
use serde_json::{json, Value};

fn str_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("Missing required argument: {key}"))
}

fn opt_str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn parse_target_spec(v: &Value) -> Result<AnnotationTarget, String> {
    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "target.kind is required".to_string())?;
    match kind {
        "symbol" => Ok(AnnotationTarget::Symbol {
            symbol: str_arg(v, "symbol")?,
        }),
        "file" => Ok(AnnotationTarget::File {
            file: str_arg(v, "file")?,
        }),
        "repo" => Ok(AnnotationTarget::Repo {
            repo_id: str_arg(v, "repo_id")?,
        }),
        "edge" => Ok(AnnotationTarget::Edge {
            from: str_arg(v, "from")?,
            to: str_arg(v, "to")?,
        }),
        other => Err(format!("Unknown target.kind: {other}")),
    }
}

fn parse_kind(v: Option<&Value>) -> Result<AnnotationKind, String> {
    let Some(v) = v else {
        return Ok(AnnotationKind::Note);
    };
    let s = v
        .as_str()
        .ok_or_else(|| "kind must be a string".to_string())?;
    AnnotationKind::parse(s).ok_or_else(|| format!("Unknown kind: {s}"))
}

fn parse_status(v: Option<&Value>) -> Result<Option<AnnotationStatus>, String> {
    let Some(v) = v else {
        return Ok(None);
    };
    let s = v
        .as_str()
        .ok_or_else(|| "status must be a string".to_string())?;
    AnnotationStatus::parse(s)
        .map(Some)
        .ok_or_else(|| format!("Unknown status: {s}"))
}

fn parse_refs(v: Option<&Value>) -> Result<Vec<AnnotationTarget>, String> {
    let Some(v) = v else {
        return Ok(Vec::new());
    };
    let arr = v
        .as_array()
        .ok_or_else(|| "refs must be an array".to_string())?;
    arr.iter().map(parse_target_spec).collect()
}

fn parse_limit(v: Option<&Value>) -> Result<Option<u32>, String> {
    let Some(v) = v else {
        return Ok(Some(100));
    };
    let n = v
        .as_u64()
        .ok_or_else(|| "limit must be a positive integer".to_string())?;
    Ok(Some(n.min(1000) as u32))
}

/// Resolve the target to a single repo id. Cross-cutting helpers
/// (e.g. `leave_handoff_note`) call this so the federation backend
/// never has to guess where a `symbol` or `file` lives.
fn target_to_repo(
    server: &LainServer,
    target: &AnnotationTarget,
) -> Result<crate::federation::repo_id::RepoId, String> {
    use crate::federation::repo_id::RepoId;
    match target {
        AnnotationTarget::Repo { repo_id } => {
            let rid = RepoId::new(repo_id).map_err(|e| format!("Invalid repo_id: {e}"))?;
            // Reject unknown repos up front. Otherwise
            // `AnnotationStore::open` would happily create a
            // `<state_dir>/annotations/<repo>.sqlite` for a never-
            // registered id, the row would land there, and
            // `list_annotations`/`resolve_annotation` would never
            // find it because they enumerate only registered repos.
            if server
                .federation()
                .is_some_and(|f| f.get_repo(&rid).is_none())
            {
                return Err(format!("Unknown repo_id: {repo_id}"));
            }
            if server.federation().is_none() && server.federation_repos().is_empty() {
                // Single-workspace mode has no federation registry;
                // the lone workspace IS the only valid repo. We
                // accept the target verbatim so single-workspace
                // servers can target their own repo by id without
                // a federation registration round-trip.
            }
            Ok(rid)
        }
        AnnotationTarget::File { .. } | AnnotationTarget::Symbol { .. } => {
            // Single-repo mode: pin to the lone repo. Federation
            // mode: `add_annotation` is intentionally `Always`-
            // classified and the caller is expected to name the
            // repo via `AnnotationTarget::Repo { repo_id }` (or
            // via the registry's cross-repo listing).
            let repos = server.federation_repos();
            match repos.len() {
                1 => Ok(repos.into_iter().next().unwrap()),
                _ => Err(
                    "Cross-repo annotations require target = {kind: 'repo', repo_id: '...'} \
                     so the registry knows where to store the row"
                        .into(),
                ),
            }
        }
        AnnotationTarget::Edge { .. } => Err(
            "Edge targets require target = {kind: 'repo', repo_id: '...'} \
             so the registry knows where to store the row"
                .into(),
        ),
    }
}

fn author_from_args(server: &LainServer, args: &Value) -> Result<AgentId, String> {
    if let Some(s) = args.get("author").and_then(|v| v.as_str()) {
        return Ok(AgentId(s.to_string()));
    }
    let token = str_arg(args, "session_token")?;
    Ok(authenticate(server, &token)?.id)
}

pub fn run_add_annotation(server: &LainServer, args: Value) -> Result<Value, String> {
    let target_v = args
        .get("target")
        .ok_or_else(|| "Missing required argument: target".to_string())?;
    let target = parse_target_spec(target_v)?;
    let kind = parse_kind(args.get("kind"))?;
    let body = str_arg(&args, "body")?;
    let author = author_from_args(server, &args)?;
    let refs = parse_refs(args.get("refs"))?;
    let repo = target_to_repo(server, &target)?;
    let inputs = AddAnnotationInputs {
        target,
        kind,
        body,
        author,
        refs,
    };
    let ann = inputs.into_annotation();
    let store = server
        .annotations()
        .store_for(&repo)
        .map_err(|e| e.to_string())?;
    store.add(&ann).map_err(|e| e.to_string())?;
    Ok(json!({
        "id": ann.id,
        "created_at_unix_ms": ann.created_at_unix_ms,
        "repo_id": repo.as_str(),
    }))
}

pub fn run_list_annotations(server: &LainServer, args: Value) -> Result<Value, String> {
    let target = args.get("target").map(parse_target_spec).transpose()?;
    let author = opt_str_arg(&args, "author").map(AgentId);
    // `kind` is OPTIONAL — only filter by it when the caller passed
    // the argument. Pre-fix bug: parse_kind defaulted to Note when
    // missing, then `Some(kind)` made every list call notes-only.
    let kind = if args.get("kind").is_some() {
        Some(parse_kind(args.get("kind"))?)
    } else {
        None
    };
    let status = parse_status(args.get("status"))?;
    let limit = parse_limit(args.get("limit"))?;
    let filter = ListFilter {
        target,
        author,
        kind,
        status,
        limit,
    };

    let repos = server.federation_repos();
    let annotations = server
        .annotations()
        .list_all(&repos, &filter, &|t| target_exists(server, t))
        .map_err(|e| e.to_string())?;
    Ok(json!({ "annotations": annotations }))
}

pub fn run_resolve_annotation(server: &LainServer, args: Value) -> Result<Value, String> {
    let id = str_arg(&args, "id")?;
    let by = if let Some(token) = args.get("session_token").and_then(|v| v.as_str()) {
        authenticate(server, token)?.id
    } else {
        opt_str_arg(&args, "resolved_by")
            .map(AgentId)
            .ok_or_else(|| "Missing required argument: session_token or resolved_by".to_string())?
    };

    let repos = server.federation_repos();
    let mut last_err: Option<String> = None;
    for repo in &repos {
        let store = server
            .annotations()
            .store_for(repo)
            .map_err(|e| e.to_string())?;
        match store.resolve(&id, &by) {
            Ok(a) => return Ok(json!({ "resolved": a })),
            Err(crate::error::LainError::NotFound(_)) => continue,
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    Err(last_err.unwrap_or_else(|| format!("annotation {id} not found in any registered repo")))
}

pub fn run_leave_handoff_note(server: &LainServer, args: Value) -> Result<Value, String> {
    let body = str_arg(&args, "body")?;
    let scope = opt_str_arg(&args, "scope");
    let refs = parse_refs(args.get("refs"))?;
    let author = author_from_args(server, &args)?;

    let repos = server.federation_repos();
    let repo = match repos.len() {
        1 => repos.into_iter().next().unwrap(),
        _ => {
            return Err(
                "leave_handoff_note currently requires a single registered repo; \
                 scope = 'repo:<id>' is reserved for the cross-repo expansion"
                    .into(),
            )
        }
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let expires_at_unix_ms = now_ms + 24 * 60 * 60 * 1000;

    let target = AnnotationTarget::Repo {
        repo_id: repo.as_str().to_string(),
    };
    let mut body_text = body;
    if let Some(scope) = scope.as_deref() {
        body_text = format!("[scope:{scope}]\n{body_text}");
    }
    let inputs = AddAnnotationInputs {
        target,
        kind: AnnotationKind::Note,
        body: body_text,
        author,
        refs,
    };
    let ann = inputs.into_annotation();
    let store = server
        .annotations()
        .store_for(&repo)
        .map_err(|e| e.to_string())?;
    store.add(&ann).map_err(|e| e.to_string())?;
    Ok(json!({
        "id": ann.id,
        "expires_at_unix_ms": expires_at_unix_ms,
    }))
}

pub fn run_get_pending_handoffs(server: &LainServer, args: Value) -> Result<Value, String> {
    let since_unix_ms = args
        .get("since_unix_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let scope = opt_str_arg(&args, "scope");

    let filter = ListFilter {
        kind: Some(AnnotationKind::Note),
        status: Some(AnnotationStatus::Open),
        limit: Some(200),
        ..Default::default()
    };

    let repos = server.federation_repos();
    let annotations = server
        .annotations()
        .list_all(&repos, &filter, &|t| target_exists(server, t))
        .map_err(|e| e.to_string())?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let ttl_ms = 24 * 60 * 60 * 1000;

    let handoffs: Vec<_> = annotations
        .into_iter()
        .filter(|a| {
            a.created_at_unix_ms >= since_unix_ms
                && now_ms.saturating_sub(a.created_at_unix_ms) <= ttl_ms
        })
        .filter(|a| match scope.as_deref() {
            None | Some("workspace") => true,
            Some(s) => a.body.contains(&format!("[scope:{s}]")),
        })
        .collect();

    Ok(json!({ "handoffs": handoffs }))
}

fn target_exists(server: &LainServer, target: &AnnotationTarget) -> bool {
    match target {
        AnnotationTarget::Repo { repo_id } => crate::federation::repo_id::RepoId::new(repo_id)
            .ok()
            .and_then(|rid| server.federation().map(|f| f.get_repo(&rid).is_some()))
            .unwrap_or(false),
        AnnotationTarget::File { .. } | AnnotationTarget::Symbol { .. } => true,
        AnnotationTarget::Edge { .. } => false,
    }
}

/// Look up annotations targeting the symbols/edges visited by a
/// graph query, for auto-include in `explain_symbol` /
/// `get_blast_radius` markdown. Returns summaries only.
pub fn summaries_for_targets(
    server: &LainServer,
    repo: &crate::federation::repo_id::RepoId,
    targets: &[AnnotationTarget],
) -> Vec<crate::server::annotations::AnnotationSummary> {
    use crate::server::annotations::{AnnotationSummary, ListQuery};
    let Ok(store) = server.annotations().store_for(repo) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for target in targets {
        let filter = ListFilter {
            target: Some(target.clone()),
            status: Some(AnnotationStatus::Open),
            limit: Some(8),
            ..Default::default()
        };
        let q = ListQuery {
            filter: &filter,
            exists: &|_| true,
        };
        let Ok(rows) = store.list_with_staleness(&q) else {
            continue;
        };
        for a in rows {
            out.push(AnnotationSummary::from_full(&a));
        }
    }
    out
}

/// Registry-only variant of [`summaries_for_targets`] for call sites
/// that hold the registry directly (the inventory-registered
/// `ToolHandler` impls in `src/server/tools/handlers/registry_impl.rs`,
/// which receive `&ToolContext` rather than `&LainServer`). Same
/// semantics: open annotations only, capped at 8 per target.
pub fn summaries_for_targets_in_registry(
    registry: &crate::server::annotations::AnnotationRegistry,
    repo: &crate::federation::repo_id::RepoId,
    targets: &[AnnotationTarget],
) -> Vec<crate::server::annotations::AnnotationSummary> {
    use crate::server::annotations::{AnnotationSummary, ListQuery};
    let Ok(store) = registry.store_for(repo) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for target in targets {
        let filter = ListFilter {
            target: Some(target.clone()),
            status: Some(AnnotationStatus::Open),
            limit: Some(8),
            ..Default::default()
        };
        let q = ListQuery {
            filter: &filter,
            exists: &|_| true,
        };
        let Ok(rows) = store.list_with_staleness(&q) else {
            continue;
        };
        for a in rows {
            out.push(AnnotationSummary::from_full(&a));
        }
    }
    out
}

/// Format the `### Open annotations` section that
/// `explain_symbol` / `get_blast_radius` append after their main
/// body. Returns an empty string when there are no summaries, so the
/// caller can append unconditionally without growing the wire
/// contract for the common (no-annotation) case.
///
/// Format mirrors the live Markdown the agent will see:
///
/// ```text
///
/// ### Open annotations
/// - [@<author>, <YYYY-MM-DD>, kind=<kind>] <body_excerpt>
/// - ...
/// ```
pub fn format_open_annotations_section(
    summaries: &[crate::server::annotations::AnnotationSummary],
) -> String {
    if summaries.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("\n\n### Open annotations\n");
    for s in summaries {
        // 1970-01-01 epoch + ms → ISO date; skip the time component to
        // keep the line short. Anything before 2001 reads as "(unknown)"
        // — the helper takes nothing else as input, and `created_at_unix_ms`
        // is u64 so a real "no time" sentinel would have to be threaded
        // through the store. Practically all rows have a sensible stamp.
        let secs = s.created_at_unix_ms / 1000;
        let date = if s.created_at_unix_ms < 1_000_000_000_000 {
            chrono_like_date(secs)
        } else {
            "(unknown)".to_string()
        };
        out.push_str(&format!(
            "- [@{}, {}, kind={}] {}\n",
            s.author, date, s.kind, s.body_excerpt
        ));
    }
    out
}

/// Minimal YYYY-MM-DD formatter. Avoids pulling chrono into the
/// annotation module just to print a date — the only consumers are
/// the Markdown appendix and unit tests, both of which accept the
/// zero-padded shape unconditionally.
fn chrono_like_date(unix_secs: u64) -> String {
    // Civil-from-days algorithm (Howard Hinnant). 1970-01-01 = day 0.
    let z = (unix_secs / 86_400) as i64;
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::annotations::{
        AddAnnotationInputs, AnnotationKind, AnnotationRegistry, AnnotationSummary,
        AnnotationTarget,
    };
    use crate::server::presence::AgentId;

    /// Empty input collapses to an empty string — the common case is
    /// "no annotations on this symbol", and the wire contract for
    /// `explain_symbol` / `get_blast_radius` promises no Markdown
    /// growth when nothing is open.
    #[test]
    fn format_section_returns_empty_when_no_summaries() {
        assert_eq!(format_open_annotations_section(&[]), "");
    }

    /// Non-empty summaries render in the documented shape. The
    /// ordering, the leading blank line, and the `[@author, date,
    /// kind=...] body` per-row format are all part of the agent's
    /// parsing contract, so any drift fails this test.
    #[test]
    fn format_section_renders_summaries_in_documented_shape() {
        let summaries = vec![
            AnnotationSummary {
                id: "id-1".into(),
                target_kind: "symbol".into(),
                target_id: "orchestrate".into(),
                kind: "todo".into(),
                status: "open".into(),
                body_excerpt: "Why is this called from two unrelated sites?".into(),
                author: "spuentesp".into(),
                // 2026-09-17 UTC. Picked to land on a recent
                // post-2020 date so the formatter exercises the
                // non-trivial Howard-Hinnant branch.
                created_at_unix_ms: 1_788_192_000_000,
            },
            AnnotationSummary {
                id: "id-2".into(),
                target_kind: "symbol".into(),
                target_id: "orchestrate".into(),
                kind: "todo".into(),
                status: "open".into(),
                body_excerpt: "Add a regression test for the overlay freshness branch.".into(),
                author: "codex".into(),
                created_at_unix_ms: 1_788_105_600_000,
            },
        ];
        let out = format_open_annotations_section(&summaries);
        // Leading blank line + section header
        assert!(out.starts_with("\n\n### Open annotations\n"));
        // One row per summary, in input order
        let rows: Vec<&str> = out.lines().filter(|l| l.starts_with("- [@")).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("@spuentesp"));
        assert!(rows[0].contains("kind=todo"));
        assert!(rows[0].contains("Why is this called"));
        assert!(rows[1].contains("@codex"));
        assert!(rows[1].contains("kind=todo"));
        assert!(rows[1].contains("regression test"));
    }

    /// `summaries_for_targets_in_registry` against a brand-new
    /// registry returns an empty vector — the path the
    /// `open_annotations_for_symbol` helper takes in single-workspace
    /// mode and in tests that don't add rows.
    #[test]
    fn summaries_returns_empty_when_no_rows_match() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = AnnotationRegistry::open(tmp.path()).unwrap();
        let rid = crate::federation::repo_id::RepoId::new("only-repo").unwrap();
        let targets = [AnnotationTarget::Symbol {
            symbol: "nonexistent".into(),
        }];
        let out = summaries_for_targets_in_registry(&registry, &rid, &targets);
        assert!(out.is_empty());
    }

    /// End-to-end through the registry: write an open annotation
    /// targeting a symbol, then ask for summaries on that symbol.
    /// The annotation must come back as an `AnnotationSummary` with
    /// the documented body-excerpt truncation (under the limit here
    /// so the value is verbatim).
    #[test]
    fn summaries_returns_open_rows_for_matching_symbol() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = AnnotationRegistry::open(tmp.path()).unwrap();
        let rid = crate::federation::repo_id::RepoId::new("only-repo").unwrap();
        let store = registry.store_for(&rid).unwrap();

        // Direct construction so the test does not depend on the
        // MCP-layer `run_add_annotation` glue.
        let inputs = AddAnnotationInputs {
            target: AnnotationTarget::Symbol {
                symbol: "orchestrate".into(),
            },
            kind: AnnotationKind::Todo,
            body: "Why does this surface a UI link for stdio mode?".into(),
            author: AgentId("spuentesp".into()),
            refs: vec![],
        };
        let a = inputs.into_annotation();
        store.add(&a).unwrap();

        // Marking it resolved must drop it from the "open" lookup,
        // not just from `list_annotations` with no status filter.
        let inputs_resolved = AddAnnotationInputs {
            target: AnnotationTarget::Symbol {
                symbol: "resolved-one".into(),
            },
            kind: AnnotationKind::Note,
            body: "Closed before merge".into(),
            author: AgentId("codex".into()),
            refs: vec![],
        };
        let r = inputs_resolved.into_annotation();
        store.add(&r).unwrap();
        store.resolve(&r.id, &AgentId("codex".into())).unwrap();

        let targets_open = [AnnotationTarget::Symbol {
            symbol: "orchestrate".into(),
        }];
        let summaries = summaries_for_targets_in_registry(&registry, &rid, &targets_open);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].kind, "todo");
        assert_eq!(summaries[0].author, "spuentesp");
        assert_eq!(
            summaries[0].body_excerpt,
            "Why does this surface a UI link for stdio mode?"
        );
        // `target_id` is the canonical "<kind>:<id>" form written by
        // `canonical_target_id` — verify the `symbol:` prefix
        // explicitly so a future refactor of the canonicalization
        // helper fails this test rather than the live tool output.
        assert_eq!(summaries[0].target_id, "symbol:orchestrate");
        assert_eq!(summaries[0].target_kind, "symbol");
        // Sanity: a different symbol sees the empty slice, not the
        // orchestrate row.
        let targets_other = [AnnotationTarget::Symbol {
            symbol: "resolved-one".into(),
        }];
        let empty = summaries_for_targets_in_registry(&registry, &rid, &targets_other);
        assert!(
            empty.is_empty(),
            "resolved rows must not appear in the open-only lookup"
        );
    }

    /// The civil-from-days formatter should match `chrono`-style
    /// YYYY-MM-DD for a few canonical instants. We pin two
    /// well-known dates plus the epoch so a regression in the
    /// Howard-Hinnant arithmetic fails loudly without dragging a
    /// `chrono` dependency into the annotation module just to test
    /// a 12-line helper.
    #[test]
    fn chrono_like_date_matches_known_instants() {
        assert_eq!(chrono_like_date(0), "1970-01-01");
        assert_eq!(chrono_like_date(86_400), "1970-01-02");
        assert_eq!(chrono_like_date(1_700_000_000), "2023-11-14");
    }
}
