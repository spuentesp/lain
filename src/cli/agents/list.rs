use crate::cli::agents::adapters::{adapter_for, InstallScope};
use crate::cli::agents::manifest::load_manifest;
use anyhow::Result;

pub fn run_list() -> Result<()> {
    let agents = load_manifest()?;
    println!("{:<18} {:<28} {:<12} {}", "AGENT", "DISPLAY", "INSTALLED", "PATH");
    for a in &agents {
        let installed = adapter_for(&a.id)
            .and_then(|ad| ad.read(a, InstallScope::User).ok())
            .map(|v| !v.is_null())
            .unwrap_or(false);
        let path = if a.config_user.is_empty() { "(project only)".to_string() } else { a.config_user.clone() };
        println!("{:<18} {:<28} {:<12} {}", a.id, a.display_name, if installed { "yes" } else { "no" }, path);
    }
    Ok(())
}
