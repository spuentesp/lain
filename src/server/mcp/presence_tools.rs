//! Eight MCP tools for the multiplayer layer.
//!
//! All tools read or mutate `PresenceRegistry` + `OccupancyMap` on the
//! `LainServer`. Tools that mutate state return the new state; tools
//! that read return JSON the agent can render.

use crate::server::ingest::LainServer;
use crate::server::presence::{
    AgentId, AgentKind, AgentMode, ClaimIntent, ClaimRequest, OccupancyEntry,
    PresenceEvent,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
pub struct RegisterAgentArgs {
    pub name: String,
    pub kind: Option<String>,
    pub mode: Option<String>,
    pub parent_session_id: Option<String>,
    pub pid: Option<u32>,
}

pub fn run_register_agent(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: RegisterAgentArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let kind = a.kind.as_deref().map(AgentKind::parse).unwrap_or(AgentKind::Other("unknown".into()));
    let mode = a.mode.as_deref().map(AgentMode::parse).unwrap_or(AgentMode::Interactive);
    let parent = a.parent_session_id.map(AgentId);
    let session = server.presence.register(a.name, kind, mode, a.pid, parent);
    let _ = server.presence_event_tx.send(PresenceEvent::AgentJoined(session.clone()));
    let expires_at_unix = system_time_to_unix_secs(session.started_at)
        + system_time_to_unix_secs_delta(server.presence_expires_after());
    Ok(json!({
        "agent_id": session.id.as_str(),
        "session_token": session.session_token,
        "expires_at_unix": expires_at_unix,
    }))
}

#[derive(Debug, Deserialize)]
pub struct HeartbeatArgs {
    pub agent_id: String,
    pub session_token: String,
}

pub fn run_heartbeat(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: HeartbeatArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    server.presence.heartbeat(&AgentId(a.agent_id), &a.session_token)
        .map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true }))
}

#[derive(Debug, Deserialize)]
pub struct ListActiveAgentsArgs {
    pub include_background: Option<bool>,
}

pub fn run_list_active_agents(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ListActiveAgentsArgs = serde_json::from_value(args).unwrap_or(ListActiveAgentsArgs { include_background: None });
    let sessions = server.presence.list_active(a.include_background.unwrap_or(false));
    let out: Vec<Value> = sessions.into_iter().map(|s| {
        let claims = server.occupancy.list_for_agent(&s.id);
        json!({
            "agent_id": s.id.as_str(),
            "name": s.name,
            "kind": s.kind.as_str(),
            "mode": s.mode.as_str(),
            "started_at": system_time_to_unix_secs(s.started_at),
            "last_heartbeat": system_time_to_unix_secs(s.last_heartbeat),
            "claims_count": claims.len(),
        })
    }).collect();
    Ok(json!(out))
}

#[derive(Debug, Deserialize)]
pub struct WhoAmIArgs {
    pub session_token: String,
}

pub fn run_who_am_i(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: WhoAmIArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    let claims = server.occupancy.list_for_agent(&session.id);
    Ok(json!({
        "agent_id": session.id.as_str(),
        "name": session.name,
        "kind": session.kind.as_str(),
        "mode": session.mode.as_str(),
        "claims": claims.into_iter().map(|c| json!({
            "path": c.path.to_string_lossy(),
            "symbols": c.symbols,
            "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
            "claimed_at": system_time_to_unix_secs(c.claimed_at),
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct ClaimFilesArgs {
    pub agent_id: String,
    pub session_token: String,
    pub files: Vec<ClaimFilesEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ClaimFilesEntry {
    pub path: String,
    pub symbols: Option<Vec<String>>,
    pub intent: Option<String>,
}

pub fn run_claim_files(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ClaimFilesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let requests: Vec<ClaimRequest> = a.files.into_iter().map(|f| ClaimRequest {
        path: std::path::PathBuf::from(f.path),
        symbols: f.symbols.unwrap_or_default(),
        intent: f.intent.as_deref().map(|s| if s == "read" { ClaimIntent::Read } else { ClaimIntent::Edit }).unwrap_or(ClaimIntent::Edit),
        ttl_seconds: None,
    }).collect();
    let result = server.occupancy.claim(&session.id, requests);
    if !result.granted.is_empty() {
        for g in &result.granted {
            let _ = server.presence_event_tx.send(PresenceEvent::ClaimGranted {
                agent_id: session.id.clone(),
                path: g.path.clone(),
            });
        }
    }
    if !result.conflicts.is_empty() {
        let _ = server.presence_event_tx.send(PresenceEvent::ConflictDetected {
            agent_id: session.id.clone(),
            conflicts: result.conflicts.clone(),
        });
    }
    Ok(json!({
        "granted": result.granted.iter().map(|g| json!({
            "path": g.path.to_string_lossy(),
            "symbols": g.symbols,
        })).collect::<Vec<_>>(),
        "conflicts": result.conflicts.iter().map(|c| json!({
            "agent_id": c.agent_id.as_str(),
            "name": c.name,
            "path": c.path.to_string_lossy(),
            "symbols": c.symbols,
        })).collect::<Vec<_>>(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct ReleaseFilesArgs {
    pub agent_id: String,
    pub session_token: String,
    pub files: Vec<ReleaseFilesEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ReleaseFilesEntry {
    pub path: String,
    pub symbols: Option<Vec<String>>,
}

pub fn run_release_files(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ReleaseFilesArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let paths: Vec<std::path::PathBuf> = a.files.into_iter().map(|f| std::path::PathBuf::from(f.path)).collect();
    let released = server.occupancy.release(&session.id, &paths);
    for path in &released {
        let _ = server.presence_event_tx.send(PresenceEvent::ClaimReleased {
            agent_id: session.id.clone(),
            path: path.clone(),
        });
    }
    Ok(json!({ "released": released.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>() }))
}

#[derive(Debug, Deserialize)]
pub struct ListOccupancyArgs {
    pub path: Option<String>,
}

pub fn run_list_occupancy(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: ListOccupancyArgs = serde_json::from_value(args).unwrap_or(ListOccupancyArgs { path: None });
    let entries: Vec<OccupancyEntry> = if let Some(p) = a.path.as_deref() {
        server.occupancy.list_for_path(std::path::Path::new(p)).into_iter().collect()
    } else {
        server.occupancy.list_all()
    };
    let out: Vec<Value> = entries.into_iter().map(|e| {
        let names: Vec<String> = e.agents.iter().filter_map(|id| server.presence.get(id).map(|s| s.name)).collect();
        json!({
            "path": e.path.to_string_lossy(),
            "agents": e.agents.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            "agent_names": names,
            "symbols": e.symbols.iter().map(|s| json!({
                "symbol": s.symbol,
                "agents": s.agents.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }).collect();
    Ok(json!(out))
}

#[derive(Debug, Deserialize)]
pub struct MyClaimsArgs {
    pub agent_id: String,
    pub session_token: String,
}

pub fn run_my_claims(server: &LainServer, args: Value) -> Result<Value, String> {
    let a: MyClaimsArgs = serde_json::from_value(args).map_err(|e| e.to_string())?;
    let session = server.presence.by_token(&a.session_token).ok_or("unknown session token")?;
    if session.id.as_str() != a.agent_id {
        return Err("agent_id does not match session token".into());
    }
    let claims = server.occupancy.list_for_agent(&session.id);
    Ok(json!(claims.into_iter().map(|c| json!({
        "path": c.path.to_string_lossy(),
        "symbols": c.symbols,
        "intent": match c.intent { ClaimIntent::Read => "read", ClaimIntent::Edit => "edit" },
        "claimed_at": system_time_to_unix_secs(c.claimed_at),
    })).collect::<Vec<_>>()))
}

fn system_time_to_unix_secs(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn system_time_to_unix_secs_delta(d: std::time::Duration) -> u64 { d.as_secs() }