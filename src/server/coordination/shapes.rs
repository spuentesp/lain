//! Wire shapes at the lain ↔ zoanthrope boundary.
//!
//! Zoanthrope speaks its own JSON (the `claim_files` / `list_claims`
//! contract in its README). Lain's clients speak lain's JSON
//! (`granted` / `conflicts` with holder naming). The types here are
//! the zoanthrope side, parsed out of CLI/MCP responses; the lain
//! side is assembled by the MCP handlers as adapters over these.

use serde::{Deserialize, Serialize};

/// Marker key lain injects into every coordination response produced
/// without a live zoanthrope: `"degraded": "zoanthrope-missing"`.
pub const DEGRADED_MARKER: &str = "zoanthrope-missing";

/// Opaque `{name, value}` pin computed by lain and stored by
/// zoanthrope (`caller_stamps`). zoanthrope verifies only its own
/// file content stamp; verifying these is the caller's business.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerStamp {
    pub name: String,
    pub value: String,
}

impl CallerStamp {
    /// `--stamp name=value` spelling for the zoanthrope CLI.
    pub fn cli_arg(&self) -> String {
        format!("{}={}", self.name, self.value)
    }
}

/// The symbol names a claim pinned, recovered from `caller_stamps`
/// entries named `symbol:<name>`. The inverse of
/// [`crate::server::coordination::symbol_stamps`].
pub fn symbols_from_stamps(stamps: &[CallerStamp]) -> Vec<String> {
    stamps
        .iter()
        .filter_map(|s| s.name.strip_prefix("symbol:").map(str::to_string))
        .collect()
}

/// One granted claim, as zoanthrope's `grant_json` reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub path: String,
    #[serde(default)]
    pub claimed_at: f64,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub caller_stamps: Vec<CallerStamp>,
}

impl Grant {
    pub fn symbols(&self) -> Vec<String> {
        symbols_from_stamps(&self.caller_stamps)
    }
}

/// A refused claim — zoanthrope's conflict shape (or its
/// `stale-claim` variant), which names who is in the way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    pub holder: String,
    pub path: String,
    #[serde(default)]
    pub claimed_at: f64,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub holder_stale: bool,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub caller_stamps: Vec<CallerStamp>,
    /// Set for the `stale-claim` refusal: the holder is the caller and
    /// the file changed under its own claim.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub hint: Option<String>,
}

impl Refusal {
    pub fn symbols(&self) -> Vec<String> {
        symbols_from_stamps(&self.caller_stamps)
    }

    pub fn is_stale_claim(&self) -> bool {
        self.reason.as_deref() == Some("stale-claim")
    }
}

/// Per-file arbitration result. Lain claims one file per zoanthrope
/// call so the wire response keeps its per-file partial-grant shape
/// (`granted` and `conflicts` in one body) and per-file intents work
/// (zoanthrope arbitrates one intent per call).
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimOutcome {
    Granted(Grant),
    Refused(Refusal),
    /// No zoanthrope on this machine: advisory semantics — logged
    /// locally, never blocks.
    Degraded { path: String },
}

/// Parse one zoanthrope claim body (grant or conflict shape) into a
/// [`ClaimOutcome`]. Bodies that are neither yield the raw `error`
/// as a `Refused` with empty holder only when they carry one; the
/// caller surfaces parse failures instead of guessing.
pub fn parse_claim_body(path: &str, body: &serde_json::Value) -> Result<ClaimOutcome, String> {
    if body.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        let granted = body
            .get("granted")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "zoanthrope claim: ok body without granted".to_string())?;
        let g = granted
            .iter()
            .find(|g| g.get("path").and_then(|p| p.as_str()) == Some(path))
            .or_else(|| granted.first())
            .ok_or_else(|| "zoanthrope claim: empty granted".to_string())?;
        let grant: Grant =
            serde_json::from_value(g.clone()).map_err(|e| format!("zoanthrope grant: {e}"))?;
        return Ok(ClaimOutcome::Granted(grant));
    }
    if body.get("holder").is_some() || body.get("reason").and_then(|r| r.as_str()) == Some("stale-claim") {
        let mut v = body.clone();
        if v.get("path").is_none() {
            v["path"] = serde_json::Value::String(path.to_string());
        }
        if v.get("caller_stamps").is_none() {
            v["caller_stamps"] = serde_json::Value::Array(Vec::new());
        }
        let refusal: Refusal =
            serde_json::from_value(v).map_err(|e| format!("zoanthrope conflict: {e}"))?;
        return Ok(ClaimOutcome::Refused(refusal));
    }
    let err = body
        .get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("zoanthrope claim failed");
    Err(err.to_string())
}
