use crate::cmds::agents::adapters::{adapter_for, expand_home, InstallScope};
use crate::cmds::agents::manifest::load_manifest;
use anyhow::{anyhow, Result};

pub fn run_remove(id: &str, scope: InstallScope) -> Result<()> {
    let agents = load_manifest()?;
    let entry = agents.iter().find(|a| a.id == id).ok_or_else(|| anyhow!("unknown agent: {id}"))?;
    let adapter = adapter_for(id).ok_or_else(|| anyhow!("no adapter for {id}"))?;
    let value = adapter.read(entry, scope)?;
    if value.is_null() {
        println!("{id} not installed in {:?} scope", scope);
        return Ok(());
    }
    let path = match scope {
        InstallScope::User => entry.config_user.clone(),
        InstallScope::Project => entry.config_project.clone(),
        InstallScope::Workspace => return Err(anyhow!("workspace scope not supported")),
    };
    let path = expand_home(&path);
    let raw = std::fs::read_to_string(&path)?;
    let mut doc: serde_json::Value = serde_json::from_str(&raw)?;
    if let Some(obj) = doc.as_object_mut() {
        if let Some(servers) = obj.get_mut(&entry.mcp_section) {
            if let Some(servers_obj) = servers.as_object_mut() {
                servers_obj.remove(&entry.mcp_name);
            }
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    println!("removed {id} from {} scope", match scope { InstallScope::User => "user", InstallScope::Project => "project", InstallScope::Workspace => "workspace" });
    Ok(())
}
