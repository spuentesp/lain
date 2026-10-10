//! Revision-aware view resolution shared by every contract tool.
//!
//! A resolved view owns the snapshot residency guard alongside the
//! exact graph backend, contract index, scope, and commit manifest.
//! Keeping these values together prevents a tool from resolving a
//! snapshot index and then accidentally traversing the live graph.

use super::envelope::{error_outcome, ViewInfo};
use super::scope::{live_scope, snapshot_scope};
use super::ToolOutcome;
use crate::federation::contracts::index::ContractIndex;
use crate::federation::contracts::snapshots::manager::{
    HoldGuard, PrepareError, SnapshotFederation,
};
use crate::federation::contracts::snapshots::record::SnapshotRecord;
use crate::federation::federated_index::FederatedIndex;
use crate::federation::graph_backend::GraphBackend;
use crate::server::mcp::handler::McpContext;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

enum ViewSource<'a> {
    Live(&'a FederatedIndex),
    Snapshot {
        federation: Arc<SnapshotFederation>,
        record: Box<SnapshotRecord>,
        /// Prevents residency eviction until the tool response has
        /// been completely assembled.
        _hold: HoldGuard,
    },
}

/// One internally consistent live or snapshot view.
pub struct ContractView<'a> {
    label: String,
    index: Option<Arc<ContractIndex>>,
    backend: Arc<dyn GraphBackend>,
    scope: Value,
    source: ViewSource<'a>,
}

impl<'a> ContractView<'a> {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn index(&self) -> Option<&Arc<ContractIndex>> {
        self.index.as_ref()
    }

    pub fn backend(&self) -> Arc<dyn GraphBackend> {
        Arc::clone(&self.backend)
    }

    pub fn scope(&self) -> &Value {
        &self.scope
    }

    pub fn live_federation(&self) -> Option<&FederatedIndex> {
        match &self.source {
            ViewSource::Live(federation) => Some(*federation),
            ViewSource::Snapshot { .. } => None,
        }
    }

    pub fn snapshot_federation(&self) -> Option<&Arc<SnapshotFederation>> {
        match &self.source {
            ViewSource::Live(_) => None,
            ViewSource::Snapshot { federation, .. } => Some(federation),
        }
    }

    pub fn snapshot_record(&self) -> Option<&SnapshotRecord> {
        match &self.source {
            ViewSource::Live(_) => None,
            ViewSource::Snapshot { record, .. } => Some(record),
        }
    }

    pub fn commits(&self) -> BTreeMap<String, String> {
        match &self.source {
            ViewSource::Live(federation) => federation
                .list_repos()
                .into_iter()
                .filter_map(|(id, _)| {
                    let commit = federation
                        .get_repo(&id)
                        .and_then(|repo| repo.db().get_last_commit().ok().flatten())?;
                    Some((id.as_str().to_string(), commit))
                })
                .collect(),
            ViewSource::Snapshot { record, .. } => record.repos.clone(),
        }
    }

    pub fn reproducible(&self) -> bool {
        matches!(self.source, ViewSource::Snapshot { .. })
    }

    pub fn view_info(&self) -> ViewInfo {
        if self.reproducible() {
            ViewInfo::snapshot(self.label.clone(), self.commits())
        } else {
            ViewInfo::live(self.commits())
        }
    }
}

pub fn snapshot_label(args: &Map<String, Value>) -> String {
    args.get("snapshot")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| "live".to_string())
}

/// Resolve all revision-dependent state once for a contract tool call.
pub async fn resolve_view<'a>(
    ctx: &'a McpContext<'a>,
    args: &Map<String, Value>,
    started: Instant,
) -> Result<ContractView<'a>, ToolOutcome> {
    let label = snapshot_label(args);
    if label == "live" {
        let federation = ctx.federation.ok_or_else(|| {
            error_outcome(
                "federation_disabled",
                "this server is not configured with a federation",
                None,
                &label,
                started,
            )
        })?;
        federation.rejoin_contracts_if_dirty().map_err(|error| {
            error_outcome(
                "invalid_argument",
                format!("rejoin failed: {error}"),
                None,
                &label,
                started,
            )
        })?;
        return Ok(ContractView {
            label,
            index: federation.contract_index(),
            backend: federation.backend(),
            scope: live_scope(federation),
            source: ViewSource::Live(federation),
        });
    }

    if !label.starts_with(crate::federation::contracts::snapshots::SNAPSHOT_ID_PREFIX) {
        return Err(error_outcome(
            "snapshot_not_found",
            format!("snapshot {label:?} not found"),
            Some(json!({"snapshot": label})),
            &label,
            started,
        ));
    }
    let manager = ctx.snapshots.ok_or_else(|| {
        error_outcome(
            "snapshot_manager_unavailable",
            "snapshot manager is not configured for this server",
            None,
            &label,
            started,
        )
    })?;
    let wait_ms = args
        .get("wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5_000)
        .min(60_000);
    let outcome = manager
        .get(&label, wait_ms)
        .await
        .map_err(|error| prepare_error_outcome(error, &label, started))?;
    let (federation, hold) = manager
        .from_snapshot_with_wait_ms(&outcome.record, wait_ms)
        .map_err(|error| match error {
            crate::error::LainError::SnapshotResidencyBusy { retry_after_ms } => error_outcome(
                "busy",
                "snapshot residency busy",
                Some(json!({"retry_after_ms": retry_after_ms})),
                &label,
                started,
            ),
            other => error_outcome(
                "invalid_argument",
                format!("from_snapshot failed: {other}"),
                None,
                &label,
                started,
            ),
        })?;
    let index = federation.contract_index.read().clone();
    let backend: Arc<dyn GraphBackend> = federation.backend.clone();
    let scope = snapshot_scope(&outcome.record);

    Ok(ContractView {
        label,
        index,
        backend,
        scope,
        source: ViewSource::Snapshot {
            federation,
            record: Box::new(outcome.record),
            _hold: hold,
        },
    })
}

fn prepare_error_outcome(error: PrepareError, label: &str, started: Instant) -> ToolOutcome {
    match error {
        PrepareError::SnapshotNotFound { snapshot } => error_outcome(
            "snapshot_not_found",
            format!("snapshot {snapshot:?} not found"),
            Some(json!({"snapshot": snapshot})),
            label,
            started,
        ),
        PrepareError::Busy { retry_after_ms } => error_outcome(
            "busy",
            "snapshot residency busy",
            Some(json!({"retry_after_ms": retry_after_ms})),
            label,
            started,
        ),
        PrepareError::RepoNotRegistered { repo } => error_outcome(
            "repo_not_registered",
            format!("repo {repo:?} not configured"),
            Some(json!({"repo": repo})),
            label,
            started,
        ),
        other => error_outcome(
            "invalid_argument",
            format!("{other:?}"),
            None,
            label,
            started,
        ),
    }
}
