//! LSP multiplexer for multi-language support
//!
//! Detects languages, spawns headless LSP servers via lsp-bridge, and routes queries.

use crate::error::LainError;
use crate::schema::{GraphNode, NodeType};
use lsp_bridge::{LspBridge, LspServerConfig};
use lsp_types::{DocumentSymbol, SymbolKind, SymbolTag, Position};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

/// Configuration for a specific language server
struct LspConfig {
    binary: &'static str,
    install_cmd: Option<&'static str>,
}

const LANGUAGE_MAP: &[(&str, LspConfig)] = &[
    ("rs", LspConfig { binary: "rust-analyzer", install_cmd: Some("rustup component add rust-analyzer") }),
    ("go", LspConfig { binary: "gopls", install_cmd: Some("go install golang.org/x/tools/gopls@latest") }),
    ("ts", LspConfig { binary: "typescript-language-server", install_cmd: Some("npm install -g typescript typescript-language-server") }),
    ("tsx", LspConfig { binary: "typescript-language-server", install_cmd: Some("npm install -g typescript typescript-language-server") }),
    ("js", LspConfig { binary: "typescript-language-server", install_cmd: Some("npm install -g typescript typescript-language-server") }),
    ("jsx", LspConfig { binary: "typescript-language-server", install_cmd: Some("npm install -g typescript typescript-language-server") }),
    ("py", LspConfig { binary: "pylsp", install_cmd: Some("pip install python-lsp-server") }),
    ("java", LspConfig { binary: "jdtls", install_cmd: None }),
    ("c", LspConfig { binary: "clangd", install_cmd: Some("brew install llvm") }),
    ("cpp", LspConfig { binary: "clangd", install_cmd: Some("brew install llvm") }),
    ("h", LspConfig { binary: "clangd", install_cmd: Some("brew install llvm") }),
    ("hpp", LspConfig { binary: "clangd", install_cmd: Some("brew install llvm") }),
    ("cs", LspConfig { binary: "omnisharp", install_cmd: None }),
    ("rb", LspConfig { binary: "solargraph", install_cmd: Some("gem install solargraph") }),
    ("swift", LspConfig { binary: "sourcekit-lsp", install_cmd: None }),
    ("kt", LspConfig { binary: "kotlin-language-server", install_cmd: None }),
    ("scala", LspConfig { binary: "metals", install_cmd: None }),
    ("vue", LspConfig { binary: "volar", install_cmd: Some("npm install -g @vue/language-server") }),
    ("svelte", LspConfig { binary: "svelte-language-server", install_cmd: Some("npm install -g svelte-language-server") }),
];

/// A symbol with its children for recursive processing
pub struct HierarchicalSymbol {
    pub node: GraphNode,
    pub children: Vec<HierarchicalSymbol>,
}

pub struct LspMultiplexer {
    bridge: LspBridge,
    /// ext -> language server configuration
    registry: HashMap<String, &'static LspConfig>,
    /// binary name -> started
    started: HashSet<String>,
    /// binary name -> missing from system
    unavailable: HashSet<String>,
    workspace: PathBuf,
}

impl LspMultiplexer {
    pub fn new(workspace: &Path) -> Result<Self, LainError> {
        let mut registry = HashMap::new();
        for (ext, config) in LANGUAGE_MAP {
            registry.insert(ext.to_string(), config);
        }
        Ok(Self {
            bridge: LspBridge::new(),
            registry,
            started: HashSet::new(),
            unavailable: HashSet::new(),
            workspace: workspace.to_path_buf(),
        })
    }

    fn detect_config(&self, path: &Path) -> Option<&&'static LspConfig> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(|e| self.registry.get(e))
    }

    pub async fn ensure_server(&mut self, path: &Path) -> Result<String, LainError> {
        let config = self
            .detect_config(path)
            .ok_or_else(|| LainError::Lsp(format!("No LSP for {:?}", path.extension())))?;

        let binary = config.binary.to_string();

        if self.unavailable.contains(&binary) {
            return Err(LainError::Lsp(format!("LSP server '{}' is missing.", binary)));
        }

        if !self.started.contains(&binary) {
            if which::which(&binary).is_err() {
                self.unavailable.insert(binary.clone());
                return Err(LainError::Lsp(format!("LSP server '{}' not found in PATH.", binary)));
            }

            let lsp_config = LspServerConfig::new()
                .command(&binary)
                .root_path(self.workspace.clone());

            self.bridge.register_server(&binary, lsp_config).await.map_err(|e| LainError::Lsp(e.to_string()))?;
            self.bridge.start_server(&binary).await.map_err(|e| LainError::Lsp(e.to_string()))?;
            self.started.insert(binary.clone());
            info!("Started LSP server: {}", binary);
        }

        Ok(binary)
    }

    /// Get hierarchical document symbols
    pub async fn get_document_symbols_hierarchical(&mut self, path: &Path) -> Result<Vec<HierarchicalSymbol>, LainError> {
        let server_id = self.ensure_server(path).await?;
        let uri = format!("file://{}", path.display());

        let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
        self.bridge.open_document(&server_id, &uri, &content).await.map_err(|e| LainError::Lsp(e.to_string()))?;

        // Wait for LSP to analyze (intelligent polling)
        let mut symbols = Vec::new();
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(2);
        let tick = std::time::Duration::from_millis(50);

        while start.elapsed() < timeout {
            symbols = self.bridge.get_document_symbols(&server_id, &uri).await
                .map_err(|e| LainError::Lsp(e.to_string()))?;

            if !symbols.is_empty() {
                break;
            }
            tokio::time::sleep(tick).await;
        }

        Ok(self.process_lsp_symbols(symbols, path))
    }

    fn process_lsp_symbols(&self, symbols: Vec<DocumentSymbol>, path: &Path) -> Vec<HierarchicalSymbol> {
        let mut results = Vec::new();
        for sym in symbols {
            if is_noisy_symbol(&sym.kind) {
                continue;
            }

            let node_type = symbol_kind_to_node_type(sym.kind);
            let mut node = GraphNode::new(
                node_type,
                sym.name.clone(),
                path.to_string_lossy().to_string(),
            )
            .with_location(sym.range.start.line, sym.range.end.line);

            if let Some(detail) = sym.detail {
                node.signature = Some(detail);
            }

            // Check for Deprecated tag
            if let Some(tags) = &sym.tags {
                if tags.contains(&SymbolTag::DEPRECATED) {
                    node.is_deprecated = true;
                }
            }

            let children = if let Some(child_syms) = sym.children {
                self.process_lsp_symbols(child_syms, path)
            } else {
                Vec::new()
            };

            results.push(HierarchicalSymbol { node, children });
        }
        results
    }

    pub async fn get_hover_info(&mut self, path: &Path, line: u32, col: u32) -> Option<String> {
        let server_id = self.ensure_server(path).await.ok()?;
        let uri = format!("file://{}", path.display());
        
        let position = Position::new(line, col);
        let hover = self.bridge.get_hover(&server_id, &uri, position).await.ok()??;
        match hover.contents {
            lsp_types::HoverContents::Scalar(marked) => Some(marked_string_to_string(marked)),
            lsp_types::HoverContents::Array(arr) => Some(arr.into_iter().map(marked_string_to_string).collect::<Vec<_>>().join("\n")),
            lsp_types::HoverContents::Markup(m) => Some(m.value),
        }
    }

    /// Get all references to a symbol at a specific location
    pub async fn get_references(&mut self, path: &Path, line: u32, col: u32) -> Result<Vec<ReferenceLocation>, LainError> {
        let server_id = self.ensure_server(path).await?;
        let uri = format!("file://{}", path.display());
        let position = Position::new(line, col);

        let locations = self.bridge.find_references(&server_id, &uri, position).await
            .map_err(|e| LainError::Lsp(e.to_string()))?;

        let mut results = Vec::new();
        for loc in locations {
            // fluent-uri doesn't directly convert to PathBuf, use string manipulation
            let path_str = loc.uri.to_string().replace("file://", "");
            results.push(ReferenceLocation {
                path: PathBuf::from(path_str),
                line: loc.range.start.line,
                col: loc.range.start.character,
                context: String::new(),
            });
        }
        Ok(results)
    }

    pub async fn install_server(&mut self, ext: &str) -> Result<String, LainError> {
        let config = self.registry.get(ext)
            .ok_or_else(|| LainError::NotFound(format!("No LSP configuration found for extension: {}", ext)))?;

        let install_cmd = config.install_cmd
            .ok_or_else(|| LainError::Lsp(format!("No automated install command available for {} ({})", ext, config.binary)))?;

        // Platform-specific guard for brew
        if install_cmd.contains("brew install") && !cfg!(target_os = "macos") {
            return Err(LainError::Lsp(format!(
                "The install command for {} ({}) requires Homebrew and is only supported on macOS. Please install it manually for your platform.",
                ext, config.binary
            )));
        }

        info!("Attempting to install LSP server for '{}' using: {}", ext, install_cmd);

        let parts: Vec<&str> = install_cmd.split_whitespace().collect();
        let mut cmd = tokio::process::Command::new(parts[0]);
        if parts.len() > 1 { cmd.args(&parts[1..]); }

        let output = cmd.output().await.map_err(|e| LainError::Lsp(format!("Failed to execute install command: {}", e)))?;

        if output.status.success() {
            self.unavailable.remove(config.binary);
            Ok(format!("Successfully installed {}.", config.binary))
        } else {
            Err(LainError::Lsp(format!("Installation failed: {}", String::from_utf8_lossy(&output.stderr))))
        }
    }

    pub fn get_supported_languages(&self) -> Vec<(String, String, bool)> {
        let mut langs = Vec::new();
        for (ext, config) in &self.registry {
            let is_available = !self.unavailable.contains(config.binary);
            langs.push((ext.clone(), config.binary.to_string(), is_available));
        }
        langs.sort_by(|a, b| a.0.cmp(&b.0));
        langs
    }

    pub async fn shutdown(&mut self) {
        if let Err(e) = self.bridge.shutdown().await {
            warn!("LSP bridge shutdown error: {}", e);
        }
    }
}

fn marked_string_to_string(m: lsp_types::MarkedString) -> String {
    match m {
        lsp_types::MarkedString::String(s) => s,
        lsp_types::MarkedString::LanguageString(ls) => ls.value,
    }
}

fn is_noisy_symbol(kind: &SymbolKind) -> bool {
    matches!(
        *kind,
        SymbolKind::VARIABLE | SymbolKind::FIELD | SymbolKind::STRING | SymbolKind::NUMBER | SymbolKind::BOOLEAN | SymbolKind::ARRAY | SymbolKind::OBJECT | SymbolKind::KEY | SymbolKind::NULL
    )
}

fn symbol_kind_to_node_type(kind: SymbolKind) -> NodeType {
    match kind {
        SymbolKind::FILE => NodeType::File,
        SymbolKind::MODULE => NodeType::Module,
        SymbolKind::NAMESPACE => NodeType::Namespace,
        SymbolKind::PACKAGE => NodeType::Package,
        SymbolKind::CLASS => NodeType::Class,
        SymbolKind::METHOD => NodeType::Method,
        SymbolKind::PROPERTY => NodeType::Property,
        SymbolKind::INTERFACE => NodeType::Interface,
        SymbolKind::FUNCTION => NodeType::Function,
        SymbolKind::VARIABLE => NodeType::Variable,
        SymbolKind::CONSTANT => NodeType::Constant,
        SymbolKind::STRUCT => NodeType::Struct,
        SymbolKind::ENUM => NodeType::Enum,
        _ => NodeType::Module,
    }
}

/// Location of a definition
#[derive(Debug, Clone)]
pub struct DefinitionLocation {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
}

/// Location of a reference
#[derive(Debug, Clone)]
pub struct ReferenceLocation {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
    pub context: String,
}

// ── LSP Pool for Parallel Language Server Communication ──────────────────────

use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Mutex as AsyncMutex;

/// Pool of LspMultiplexer instances for parallel LSP communication
pub struct LspPool {
    multiplexers: Vec<Arc<AsyncMutex<LspMultiplexer>>>,
    next: AtomicUsize,
}

impl LspPool {
    pub fn new(workspace: &Path, size: usize) -> Result<Self, LainError> {
        let mut multiplexers = Vec::with_capacity(size);
        for _ in 0..size {
            multiplexers.push(Arc::new(AsyncMutex::new(LspMultiplexer::new(workspace)?)));
        }
        Ok(Self {
            multiplexers,
            next: AtomicUsize::new(0),
        })
    }

    /// Get next multiplexer in round-robin fashion
    pub fn next(&self) -> Arc<AsyncMutex<LspMultiplexer>> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.multiplexers.len();
        Arc::clone(&self.multiplexers[idx])
    }

    /// Shutdown all multiplexers in the pool
    pub async fn shutdown_all(&self) {
        for m in &self.multiplexers {
            m.lock().await.shutdown().await;
        }
    }
}
