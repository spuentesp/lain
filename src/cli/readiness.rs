//! Compact CLI projections of the canonical diagnostic report.

use anyhow::Result;
use serde_json::json;
use std::path::Path;

pub fn capabilities(json_output: bool, workspace: Option<&Path>) -> Result<i32> {
    let report = super::doctor::build_report(workspace)?;
    let repository = report
        .repository
        .as_ref()
        .and_then(|repo| repo.root.file_name())
        .map(|name| name.to_string_lossy());
    let freshness = report.repository.as_ref().map(|repo| {
        json!({
            "head": repo.head,
            "indexed_commit": repo.indexed_commit,
            "working_tree_overlay": repo.working_tree_overlay,
        })
    });
    let value = json!({
        "schema_version": report.schema_version,
        "server_version": report.server_version,
        "repository": repository,
        "capabilities": report.capabilities,
        "freshness": freshness,
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "Capabilities for {}",
            repository.as_deref().unwrap_or("unresolved repository")
        );
        let entries = value["capabilities"]
            .as_object()
            .expect("fixed capability object");
        for (name, capability) in entries {
            println!(
                "  {name:<18} {}",
                capability["state"].as_str().unwrap_or("unknown")
            );
        }
    }
    Ok(report.exit_code())
}

pub fn status(json_output: bool, workspace: Option<&Path>) -> Result<i32> {
    let report = super::doctor::build_report(workspace)?;
    let exit_code = report.exit_code();
    let value = json!({
        "schema_version": report.schema_version,
        "server_version": report.server_version,
        "agent_ready": report.agent_ready,
        "repository": report.repository,
        "capabilities": report.capabilities,
        "transport": report.transport,
        "indexing": null,
        "problems": report.problems,
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("LAIN {}", report.server_version);
        if let Some(repo) = value["repository"].as_object() {
            println!(
                "  Repository  {}",
                repo["root"].as_str().unwrap_or("unknown")
            );
            println!(
                "  HEAD        {}",
                repo["head"].as_str().unwrap_or("unknown")
            );
            println!(
                "  Indexed     {}",
                repo["indexed_commit"].as_str().unwrap_or("missing")
            );
        }
        println!(
            "  MCP         {}",
            if report.transport.healthy {
                "healthy"
            } else {
                "unavailable"
            }
        );
        println!(
            "Agent-ready: {}",
            if report.agent_ready { "YES" } else { "NO" }
        );
    }
    Ok(exit_code)
}
