//! `lain models`: list the declared embedding models and install them.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;

use crate::server::models::{self, Catalog};

#[derive(Debug, Subcommand)]
pub enum ModelsAction {
    /// List the declared models, whether each is installed, and the default.
    List,
    /// Fetch and verify a declared model. Defaults to the catalog's default.
    Install {
        /// Model name from the catalog; omit for the default.
        name: Option<String>,
    },
    /// Print where models are installed.
    Path,
}

pub fn run(action: ModelsAction) -> Result<()> {
    let catalog = Catalog::load()?;
    match action {
        ModelsAction::List => {
            for m in &catalog.models {
                let dir = models::model_dir(m);
                let installed = dir
                    .as_ref()
                    .map(|d| models::is_installed(m, d))
                    .unwrap_or(false);
                let mark = if m.name == catalog.default {
                    " (default)"
                } else {
                    ""
                };
                println!(
                    "{}{}\n  role: {}  license: {}  installed: {}",
                    m.name,
                    mark,
                    m.role,
                    if m.license.is_empty() {
                        "unknown"
                    } else {
                        &m.license
                    },
                    if installed { "yes" } else { "no" }
                );
            }
        }
        ModelsAction::Install { name } => {
            let wanted = name.unwrap_or_else(|| catalog.default.clone());
            let spec = catalog
                .get(&wanted)
                .ok_or_else(|| anyhow::anyhow!("no model named `{wanted}` in the catalog"))?;
            let dir = models::install(spec)?;
            println!("{} installed at {}", spec.name, dir.display());
        }
        ModelsAction::Path => {
            let root: Option<PathBuf> = models::install_root();
            match root {
                Some(p) => println!("{}", p.display()),
                None => anyhow::bail!("no data directory for models on this system"),
            }
        }
    }
    Ok(())
}
