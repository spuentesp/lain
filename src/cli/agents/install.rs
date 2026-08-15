use super::adapters::{adapter_for, InstallScope};
use super::manifest::load_manifest;
use anyhow::{anyhow, Result};

pub fn run_install(id: Option<&str>, all: bool, scope: InstallScope) -> Result<()> {
    // The adapter's `http` branch writes `url: http://localhost:<LAIN_PORT>/mcp`,
    // and the sidecar stdio fallback uses the same value to find the owner.
    // Propagate LAIN_PORT from the caller (or default to 9999) so the
    // generated config always points at the running owner's port. We only
    // set it if it's not already in the env so we don't clobber the caller.
    if std::env::var_os("LAIN_PORT").is_none() {
        std::env::set_var("LAIN_PORT", "9999");
    }
    let agents = load_manifest()?;
    if all {
        for a in &agents {
            install_one(a, scope)?;
        }
        return Ok(());
    }
    let target = id.ok_or_else(|| anyhow!("--all or <id> is required"))?;
    let entry = agents
        .iter()
        .find(|a| a.id == target)
        .ok_or_else(|| anyhow!("unknown agent id: {target}"))?;
    install_one(entry, scope)
}

fn install_one(entry: &super::manifest::AgentEntry, scope: InstallScope) -> Result<()> {
    let adapter = adapter_for(&entry.id).ok_or_else(|| anyhow!("no adapter for {}", entry.id))?;
    adapter.install(entry, scope)?;
    println!("installed {} ({} scope)", entry.id, scope_name(scope));
    Ok(())
}

fn scope_name(s: InstallScope) -> &'static str {
    match s { InstallScope::User => "user", InstallScope::Project => "project", InstallScope::Workspace => "workspace" }
}
