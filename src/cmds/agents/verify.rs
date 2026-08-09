use crate::cmds::agents::adapters::{adapter_for, InstallScope};
use crate::cmds::agents::manifest::load_manifest;
use anyhow::Result;
use lain_mcp_probe::{probe_http, ProbeHealth, ProbeReport};

#[derive(serde::Serialize)]
struct VerifyRow {
    id: String,
    installed: bool,
    config_valid: bool,
    mcp_reachable: bool,
    tools_count: Option<usize>,
    health: String,
    error: Option<String>,
}

pub async fn run_verify_async(all: bool, id: Option<&str>, json: bool) -> Result<()> {
    let agents = load_manifest()?;
    let targets: Vec<_> = if all {
        agents.iter().collect()
    } else {
        let id = id.expect("--all or <id> required");
        agents.iter().filter(|a| a.id == id).collect()
    };
    let mut rows = Vec::new();
    for a in targets {
        let adapter = adapter_for(&a.id);
        let read = adapter
            .as_ref()
            .and_then(|ad| ad.read(a, InstallScope::User).ok())
            .unwrap_or(serde_json::Value::Null);
        let installed = !read.is_null();
        let config_valid = installed;
        let report = if installed {
            // `lain agents verify` always probes the shared HTTP singleton
            // regardless of the per-agent config transport, because the
            // singleton is the single shared server every wired agent ends
            // up talking to. The `transport` field on the manifest row is
            // still useful for documentation and for future per-agent
            // changes, but the verify path itself only ever hits HTTP.
            let url = format!("http://localhost:{}/mcp", std::env::var("LAIN_PORT").unwrap_or_else(|_| "9999".into()));
            probe_http(&url).await
        } else {
            ProbeReport::not_installed()
        };
        rows.push(row(a.id.clone(), report, installed, config_valid));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!("{:<14} {:<10} {:<8} {:<8} {:<8} {:<14} {}", "AGENT", "INSTALLED", "CONFIG", "MCP", "TOOLS", "HEALTH", "ERROR");
        for r in &rows {
            println!("{:<14} {:<10} {:<8} {:<8} {:<8} {:<14} {}",
                r.id, yn(r.installed), yn(r.config_valid), yn(r.mcp_reachable),
                r.tools_count.map(|n| n.to_string()).unwrap_or_else(|| "-".into()),
                r.health, r.error.clone().unwrap_or_else(|| "-".into()));
        }
    }
    Ok(())
}

fn row(id: String, report: ProbeReport, installed: bool, config_valid: bool) -> VerifyRow {
    let (health, err) = match report.health {
        ProbeHealth::Operational => ("Operational".to_string(), None),
        ProbeHealth::Unreachable(msg) => ("Unreachable".to_string(), Some(msg)),
        ProbeHealth::Error(msg) => ("Error".to_string(), Some(msg)),
    };
    VerifyRow {
        id, installed, config_valid, mcp_reachable: report.mcp_reachable,
        tools_count: report.tools_count, health, error: err,
    }
}

fn yn(b: bool) -> &'static str { if b { "yes" } else { "no" } }

pub async fn run_verify(all: bool, id: Option<&str>, json: bool) -> Result<()> {
    run_verify_async(all, id, json).await
}
