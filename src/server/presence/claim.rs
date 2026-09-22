//! Claim-related types: how an agent holds a file or a symbol.
//!
//! `ClaimIntent`, `SymbolHash`, `Claim`, `ConflictEntry`, `Holder`,
//! `SymbolOccupancy`, and `OccupancyEntry` are the wire-shape types that
//! describe one agent's hold on a workspace resource. `OccupancyMap`
//! (in `super`) stores `OccupancyEntry` per claimed path; `Presence`
//! tools (in `super::mcp::presence_tools`) read these types back to the
//! caller.
//!
//! `unix_secs` and `epoch_secs` are the SystemTime ↔ u64 epoch serde
//! helpers shared by the persistence layer in `super`.

use std::path::PathBuf;
use std::time::SystemTime;

use super::agent::AgentId;
use crate::server::revision_log::RevisionId;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ClaimIntent {
    Read,
    Edit,
}

/// Content hash for a symbol body, computed as BLAKE3-256 over the raw
/// source slice. Lets the federation layer track a symbol across index
/// rebuilds: if the body (and therefore the hash) changes, downstream
/// caches and conflict checks treat it as a different symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolHash(pub [u8; 32]);

impl SymbolHash {
    /// Compute the BLAKE3-256 hash of `b` and wrap it.
    pub fn from_bytes(b: &[u8]) -> Self {
        let mut out = [0u8; 32];
        let hash = blake3::hash(b);
        out.copy_from_slice(hash.as_bytes());
        Self(out)
    }

    /// Placeholder for "no real body hash yet" — distinct from any
    /// real hash because `blake3::hash(b"")` is not the all-zero array.
    pub fn zero() -> Self {
        Self([0u8; 32])
    }
}

impl serde::Serialize for SymbolHash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> serde::Deserialize<'de> for SymbolHash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("bad SymbolHash length"));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

/// Serialize a `SystemTime` as UNIX seconds.
///
/// These fields used to be `skip_serializing`, on the reasoning that
/// live in-memory state always wins over the persisted snapshot. That
/// stopped being true when presence became shared through the state
/// file: every call now reloads it, so a dropped timestamp came back as
/// the epoch almost immediately. Two agents driving a live server both
/// reported `claimed_at: 0` on every claim they held, and a conflict's
/// `last_seen_unix` froze — leaving no way to tell a fresh claim from a
/// stale one, which is exactly what those fields are for.
pub mod unix_secs {
    use super::SystemTime;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let secs = t
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        s.serialize_u64(secs)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }
}

/// `serde(default)` companion for [`unix_secs`], for snapshots written
/// before the timestamps were persisted.
pub(crate) fn epoch_secs() -> SystemTime {
    SystemTime::UNIX_EPOCH
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Claim {
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub symbols: Vec<String>,
    /// `None` for a file-level claim (no specific symbol hash).
    /// `Some(hash)` carries the BLAKE3-256 of the symbol body's
    /// exact byte range as recorded by the tree-sitter extractor
    /// (`byte_start..byte_end` in `SymbolDef`). Editing any byte
    /// inside that range flips the hash; bytes outside the range
    /// don't. Symbol-level claims fall back to
    /// `Some(SymbolHash::zero())` only when the file can't be read,
    /// isn't UTF-8, isn't supported by the extractor, or doesn't
    /// define the symbol.
    pub content_hash: Option<SymbolHash>,
    pub intent: ClaimIntent,
    #[serde(with = "unix_secs", default = "epoch_secs")]
    pub claimed_at: SystemTime,
    /// Wall-clock time of the most recent touch (claim grant or
    /// heartbeat refresh) on this claim. Surfaced in conflict reports
    /// so callers can answer *when* a conflicting claim was recorded,
    /// not just *who* is holding it. Defaults to `claimed_at` on
    /// construction and is serialized as epoch on persistence reload
    /// (same durability story as `claimed_at`: live state wins).
    #[serde(with = "unix_secs", default = "epoch_secs")]
    pub last_touched_unix: SystemTime,
    /// Optional expiry timestamp (PR 10 Task 3 hook). `None` means
    /// "no expiry set"; the federation expiry loop will ignore it.
    pub expires_at: Option<SystemTime>,
    /// Last plan revision the agent saw at the moment this claim was
    /// granted (Task 1.4, PR 1). `None` for legacy claims or for
    /// callers that don't track revisions yet. Tolerated on load via
    /// `default` so older state files hydrate without migration, and
    /// omitted from the wire JSON when absent (`skip_serializing_if`)
    /// so unchanged claims don't bloat the persist payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_revision: Option<RevisionId>,
    /// `true` when the server *guessed* this claim from filesystem
    /// activity rather than the agent declaring it (see
    /// `server::attribution`). A consumer should weigh "this agent told
    /// me" differently from "the server inferred it": inferred claims
    /// come from a heuristic that can and does misfire, and they carry
    /// a short TTL so a wrong guess heals itself.
    #[serde(default)]
    pub inferred: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConflictEntry {
    pub agent_id: AgentId,
    pub path: PathBuf,
    pub symbols: Vec<String>,
    /// Intent of the *existing* claim the conflict is reported
    /// against. Under the current read-vs-edit filter this is
    /// always `ClaimIntent::Edit` (reads never conflict), but
    /// surfacing it makes the conflict JSON self-describing for
    /// downstream renderers — they can branch on `intent` without
    /// re-deriving the semantics from `path`.
    pub intent: ClaimIntent,
    /// `true` when the conflicting claim was inferred from filesystem
    /// activity rather than declared by its holder. Lets a blocked
    /// agent distinguish "alice said she is editing this" from "the
    /// server saw a write and guessed it was alice".
    #[serde(default)]
    pub inferred: bool,
    /// When the conflicting claim was last touched (typically claim
    /// grant time). Serialized as a UNIX-epoch second count in the
    /// MCP conflict JSON so callers can show "alice has been holding
    /// this for 5m" — and so the value is still meaningful when the
    /// conflicting agent's session has expired (the `name` field
    /// would be lost in that case, so we never carried one).
    pub last_seen_unix: SystemTime,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolOccupancy {
    pub symbol: String,
    pub agents: Vec<AgentId>,
}

/// One agent's hold on a file, with the detail needed to decide whether
/// it is in your way.
///
/// `agents` alone was not enough. Two agents driving a live server both
/// stumbled here: one saw a peer listed on a file it held for `edit`,
/// could not see that the peer's hold was a non-blocking `read`, and
/// reported that mutual exclusion was broken. It was not — the listing
/// simply could not express the difference.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Holder {
    pub agent_id: AgentId,
    /// `edit` blocks other edits; `read` never blocks anything.
    pub intent: ClaimIntent,
    /// True when the attribution watcher guessed this hold rather than
    /// the agent declaring it.
    pub inferred: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OccupancyEntry {
    pub path: PathBuf,
    /// Agent ids holding this path. Kept for compatibility; prefer
    /// [`Self::holders`], which says *how* each one holds it.
    pub agents: Vec<AgentId>,
    pub holders: Vec<Holder>,
    pub symbols: Vec<SymbolOccupancy>,
}
