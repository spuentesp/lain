//! Claim-shaped facade over the zoanthrope proxy.
//!
//! The method names mirror the old in-memory claims registry
//! (`OccupancyMap`) so the code-graph integration points and the
//! observability surface (audit, SSE, metrics) keep their call
//! shapes — but every arbitration, bookkeeping, and lease decision
//! below is delegated to zoanthrope. There is no local registry: the
//! facade maps lain's `ClaimRequest`/`ClaimResult` wire types onto
//! zoanthrope's per-file claim contract and back.

use super::shapes::{symbols_from_stamps, ClaimOutcome};
use super::CoordinationProxy;
use crate::server::presence::{
    AgentId, AgentSession, Claim, ClaimIntent, ClaimRequest, ClaimResult, ConflictEntry, Holder,
    OccupancyEntry, SymbolOccupancy,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

impl CoordinationProxy {
    /// Claim the requested files on behalf of a registered session.
    /// One zoanthrope call per file (per-file intents, per-file
    /// partial grants); the symbol pins ride along as `caller_stamps`.
    pub fn claim_with_session(
        &self,
        session: &AgentSession,
        requests: Vec<ClaimRequest>,
    ) -> ClaimResult {
        self.claim_batch(&session.id, requests)
    }

    /// Claim without a session (attribution watcher's inferred
    /// claims). Same delegation; purpose marks the claim inferred.
    pub fn claim(&self, agent_id: &AgentId, requests: Vec<ClaimRequest>) -> ClaimResult {
        self.claim_batch(agent_id, requests)
    }

    pub fn claim_inferred(&self, agent_id: &AgentId, requests: Vec<ClaimRequest>) -> ClaimResult {
        self.claim_batch(agent_id, requests)
    }

    fn claim_batch(&self, agent_id: &AgentId, requests: Vec<ClaimRequest>) -> ClaimResult {
        let mut granted = Vec::new();
        let mut conflicts = Vec::new();
        let mut advisories = Vec::new();
        for req in requests {
            let path_str = req.path.to_string_lossy().into_owned();
            let stamps = super::symbol_stamps(&req.path, &req.symbols);
            match self.claim_file(
                agent_id.as_str(),
                &path_str,
                None,
                req.intent.clone(),
                &stamps,
            ) {
                Ok(ClaimOutcome::Granted(_)) => granted.push(req),
                Ok(ClaimOutcome::Refused(r)) => {
                    let entry = ConflictEntry {
                        agent_id: AgentId(r.holder.clone()),
                        path: req.path.clone(),
                        symbols: symbols_from_stamps(&r.caller_stamps),
                        intent: match r.intent.as_str() {
                            "read" => ClaimIntent::Read,
                            _ => ClaimIntent::Edit,
                        },
                        inferred: false,
                        last_seen_unix: SystemTime::UNIX_EPOCH
                            + Duration::from_secs_f64(r.claimed_at.max(0.0)),
                    };
                    // A refused *read* is the old advisory case under
                    // lain's "readers never block" rule, but zoanthrope
                    // arbitrates reads against edits. Surface the
                    // refusal as a conflict (holder naming preserved);
                    // advisory mode is the hook layer's
                    // `ZOANTHROPE_MODE=advisory` behavior.
                    conflicts.push(entry);
                }
                Ok(ClaimOutcome::Degraded { .. }) => {
                    // Advisory semantics: logged by the proxy, never
                    // blocks. The claim is granted-looking so callers
                    // proceed; the response carries
                    // `degraded: "zoanthrope-missing"`.
                    granted.push(req);
                }
                Err(e) => {
                    tracing::warn!("zoanthrope claim failed for {path_str}: {e}");
                    // Fail soft on transport errors too: an error must
                    // never block an agent's edit.
                    granted.push(req);
                    advisories.push(ConflictEntry {
                        agent_id: agent_id.clone(),
                        path: PathBuf::from(&path_str),
                        symbols: Vec::new(),
                        intent: ClaimIntent::Edit,
                        inferred: true,
                        last_seen_unix: SystemTime::now(),
                    });
                }
            }
        }
        ClaimResult {
            granted,
            conflicts,
            advisories,
            world_state: None,
        }
    }

    /// Presence touch — refreshes the agent's zoanthrope `last_seen`.
    /// Best-effort: a coordination outage must not fail a heartbeat.
    pub fn touch(&self, agent_id: &AgentId) {
        if let Err(e) = self.register_agent(agent_id.as_str(), "other") {
            tracing::debug!("zoanthrope touch for {}: {e}", agent_id.as_str());
        }
    }

    /// Release the given paths; returns those zoanthrope reports as
    /// released (all of them in degraded mode).
    pub fn release(&self, agent_id: &AgentId, paths: &[PathBuf]) -> Vec<PathBuf> {
        let strs: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        match self.release_paths(agent_id.as_str(), &strs) {
            Ok(body) => body
                .get("released")
                .and_then(serde_json::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(PathBuf::from))
                        .collect()
                })
                .unwrap_or_else(|| paths.to_vec()),
            Err(_) => paths.to_vec(),
        }
    }

    /// Release every claim zoanthrope credits to `agent_id` (session
    /// teardown path). Two calls: one listing, one release.
    pub fn release_all_for(&self, agent_id: &AgentId) -> Vec<PathBuf> {
        let held: Vec<PathBuf> = self
            .claims_for_agent(agent_id)
            .into_iter()
            .map(|c| c.path)
            .collect();
        if held.is_empty() {
            return held;
        }
        self.release(agent_id, &held)
    }

    /// Per-claim TTLs have no zoanthrope counterpart (zoanthrope's
    /// presence TTL and its "claims are never auto-released" policy
    /// govern claim lifetime). Kept as a no-op so the expiry loop
    /// stays one call site; always empty.
    pub fn expire_by_ttl(&self) -> Vec<(AgentId, PathBuf)> {
        Vec::new()
    }

    /// Who holds `path` — one `list_claims` pull behind the scenes.
    /// Prefer [`Self::claims_snapshot`] when looking up many paths.
    pub fn list_for_path(&self, path: &Path) -> Option<OccupancyEntry> {
        let snap = self.claims_snapshot();
        snap_entry_for_path(&snap, path)
    }

    pub fn list_all(&self) -> Vec<OccupancyEntry> {
        let snap = self.claims_snapshot();
        let mut out: Vec<OccupancyEntry> = Vec::new();
        for c in snap.all() {
            upsert_entry(&mut out, c);
        }
        out
    }

    pub fn list_for_agent(&self, agent_id: &AgentId) -> Vec<Claim> {
        self.claims_for_agent(agent_id)
    }
}

fn snap_entry_for_path(snap: &super::proxy::ClaimSnapshot, path: &Path) -> Option<OccupancyEntry> {
    let mut out: Vec<OccupancyEntry> = Vec::new();
    for c in snap.all().iter().filter(|c| c.path == path) {
        upsert_entry(&mut out, c);
    }
    out.into_iter().next()
}

/// Fold one claim into the per-path [`OccupancyEntry`] listing.
fn upsert_entry(out: &mut Vec<OccupancyEntry>, c: &Claim) {
    let entry = match out.iter_mut().find(|e| e.path == c.path) {
        Some(e) => e,
        None => {
            out.push(OccupancyEntry {
                path: c.path.clone(),
                agents: Vec::new(),
                holders: Vec::new(),
                symbols: Vec::new(),
            });
            out.last_mut().expect("just pushed")
        }
    };
    if !entry.agents.contains(&c.agent_id) {
        entry.agents.push(c.agent_id.clone());
    }
    entry.holders.push(Holder {
        agent_id: c.agent_id.clone(),
        intent: c.intent.clone(),
        inferred: c.inferred,
    });
    for sym in &c.symbols {
        match entry.symbols.iter_mut().find(|s| s.symbol == *sym) {
            Some(s) => {
                if !s.agents.contains(&c.agent_id) {
                    s.agents.push(c.agent_id.clone());
                }
            }
            None => entry.symbols.push(SymbolOccupancy {
                symbol: sym.clone(),
                agents: vec![c.agent_id.clone()],
            }),
        }
    }
}
