# Agent Installation and Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every supported AI coding agent use Lain through MCP, and prove it in CI and locally with a single command.

**Architecture:** Bump the MCP protocol version Lain speaks to 2025-11-25 so Kimi and Claude handshakes succeed, then add a single-source-of-truth agent manifest plus a per-agent installer and a `lain-mcp-probe` test harness that exercises each agent's MCP wiring end-to-end. The HTTP singleton on port 9999 stays the default transport.

**Tech Stack:** Rust 2021, `rust-mcp-sdk 1.0.1`, `rust-mcp-schema 0.10.3` (with `2025_11_25` feature), `rust-mcp-macros 1.0.0`, `tokio` (already a dep), `clap` (already a dep), `toml` and `serde_json` (already deps), `tempfile` (dev dep), `parking_lot` (already a dep).

## Global Constraints

- Pin `rust-mcp-sdk 1.0.1` exactly and `rust-mcp-schema 0.10.3` exactly.
- `rust-mcp-schema` must enable the `2025_11_25` feature.
- `src/mcp/handler.rs` must advertise `ProtocolVersion::V2025_11_25`.
- The HTTP singleton on `LAIN_PORT` (default 9999) remains the default transport; stdio is fallback only.
- Per-agent adapters write only to the manifest-declared config files; no behavior changes in `hooks/`, `scripts/`, or existing agent configs.
- No new runtime dependencies beyond the version bumps above; tests may use `tempfile` and `assert_cmd`.
- No git commit, push, reset, or other mutations without explicit user authorization.
- All work lands on `main`; this is a host-managed linked worktree already on `main`.

---

## File Map

- **Create:** `agents/manifest.toml` — single source of truth for every supported agent.
- **Modify:** `Cargo.toml` — bump MCP dependency versions.
- **Create:** `crates/lain-mcp-probe/Cargo.toml` and `crates/lain-mcp-probe/src/lib.rs` — MCP probe crate.
- **Modify:** `src/mcp/handler.rs` — switch to `ProtocolVersion::V2025_11_25`; update imports.
- **Create:** `src/cmds/agents/mod.rs` — `lain agents` subcommand module.
- **Create:** `src/cmds/agents/manifest.rs` — load and validate `agents/manifest.toml`.
- **Create:** `src/cmds/agents/install.rs` — installer dispatcher.
- **Create:** `src/cmds/agents/verify.rs` — `lain agents verify` command.
- **Create:** `src/cmds/agents/adapters/mod.rs` and one file per supported agent.
- **Create:** `src/cmds/agents/adapters/claude.rs`, `kimi.rs`, `cursor.rs`, `continue.rs`, `windsurf.rs`, `cline.rs`, `codex.rs`, `omp.rs`, `gemini.rs`, `vscode_copilot.rs`.
- **Modify:** `src/main.rs` — register the `agents` subcommand in the clap tree.
- **Create:** `tests/agents_install.rs` — end-to-end install + verify harness.
- **Create:** `docs/agent-installation.md` — install and verify documentation.
- **Modify:** `README.md`, `docs/quickstart-tools.md`, `docs/QUICKSTART_AGENTS.md` — point at the new commands.

---

### Task 1: Bump MCP dependencies and switch advertised protocol version

**Files:**
- Modify: `Cargo.toml:14-15`
- Modify: `src/mcp/handler.rs:12, 189`

**Interfaces:**
- Consumes: current `rust-mcp-sdk = "=0.9.0"` and `rust-mcp-schema = "0.10.0"` pins; current `ProtocolVersion::V2024_11_05` advertisement at `src/mcp/handler.rs:189`.
- Produces: `rust-mcp-sdk = "1.0.1"` and `rust-mcp-schema = "0.10.3"` (with `2025_11_25` feature) pins; `ProtocolVersion::V2025_11_25` advertisement; `cargo build --release` still passes.

- [ ] **Step 1: Update `Cargo.toml`**

Replace the two MCP lines so the file reads:

```toml
rust-mcp-sdk = { version = "=1.0.1", default-features = false, features = ["server", "stdio", "macros"] }
rust-mcp-schema = { version = "=0.10.3", default-features = false, features = ["2025_11_25", "schema_utils"] }
```

- [ ] **Step 2: Update imports in `src/mcp/handler.rs`**

Replace the `use rust_mcp_schema::...` import block (around line 12) so it matches the new schema 0.10.3 export path. Concretely, replace any `ProtocolVersion::V2024_11_05` reference and any import that has moved between 0.10.0 and 0.10.3. Use whatever the compiler suggests; the only required semantic change is `V2024_11_05` → `V2025_11_25`.

- [ ] **Step 3: Update the protocol version in `server_info`**

At `src/mcp/handler.rs:189`, change:

```rust
protocol_version: ProtocolVersion::V2024_11_05.into(),
```

to:

```rust
protocol_version: ProtocolVersion::V2025_11_25.into(),
```

- [ ] **Step 4: Build and run all tests to verify the bump**

Run:

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo build --release
cargo test --all-targets
```

Expected: green; no changes in `cargo test` count beyond the version bump itself.

- [ ] **Step 5: Stage but do not commit**

```bash
git add Cargo.toml src/mcp/handler.rs
```

(No commit; the user has not authorized one yet.)

---

### Task 2: Author `agents/manifest.toml` and a typed loader

**Files:**
- Create: `agents/manifest.toml`
- Create: `src/cmds/agents/manifest.rs`
- Modify: `src/cmds/agents/mod.rs` to expose the loader (add `pub mod manifest;` and re-export its API)

**Interfaces:**
- Consumes: the manifest format spec from the design.
- Produces:
  - `pub struct AgentEntry { id, display_name, binary, detect_paths, config_user, config_project, config_format, mcp_section, mcp_name, transport, command, default_args, headless_probe }`
  - `pub fn load_manifest() -> Result<Vec<AgentEntry>, ManifestError>`
  - `pub const DEFAULT_MANIFEST: &str` (the TOML embedded at compile time)
  - `pub const SUPPORTED_AGENT_IDS: &[&str]` listing every supported id

- [ ] **Step 1: Write the failing unit test for the loader**

Add to `src/cmds/agents/mod.rs` (create the file with this content):

```rust
//! Agent installation and verification

pub mod manifest;

#[cfg(test)]
mod tests {
    use super::manifest::{load_manifest, AgentEntry};

    #[test]
    fn loader_returns_known_agents() {
        let agents = load_manifest().expect("manifest parses");
        let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
        for required in [
            "claude", "kimi", "cursor", "continue", "windsurf",
            "cline", "codex", "omp", "gemini", "vscode_copilot",
        ] {
            assert!(ids.contains(&required), "missing manifest row for {required}");
        }
    }

    #[test]
    fn manifest_entries_have_non_empty_ids_and_commands() {
        let agents = load_manifest().expect("manifest parses");
        for a in &agents {
            assert!(!a.id.is_empty());
            assert!(!a.command.is_empty() || a.transport == "http");
        }
    }
}
```

- [ ] **Step 2: Run test, confirm it fails**

Run:

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::agents::tests
```

Expected: FAIL because `src/cmds/agents/manifest.rs` does not exist yet.

- [ ] **Step 3: Write `agents/manifest.toml`**

Create `agents/manifest.toml` with one row per supported agent. The minimum shape for the loader to parse is:

```toml
[[agent]]
id = "claude"
display_name = "Claude Code"
binary = "claude"
detect_paths = ["~/.claude"]
config_user = "~/.claude/settings.json"
config_project = ".claude/settings.json"
config_format = "json"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = ["claude", "--print", "--mcp-config", "{{config_path}}", "list your tools"]

[[agent]]
id = "kimi"
display_name = "Kimi Code"
binary = "kimi"
detect_paths = ["~/.kimi-code/plugins/managed/lain"]
config_user = "~/.kimi-code/plugins/managed/lain/kimi.plugin.json"
config_project = ".kimi-code/plugins/lain/kimi.plugin.json"
config_format = "kimi-plugin"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = ["kimi", "-p", "list your tools"]

[[agent]]
id = "cursor"
display_name = "Cursor"
binary = "cursor"
detect_paths = ["~/.cursor"]
config_user = "~/.cursor/mcp.json"
config_project = ".cursor/mcp.json"
config_format = "json"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = []

[[agent]]
id = "continue"
display_name = "Continue.dev"
binary = "continue-cli"
detect_paths = ["~/.continue"]
config_user = "~/.continue/config.json"
config_project = ".continue/config.json"
config_format = "continue"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = []

[[agent]]
id = "windsurf"
display_name = "Windsurf"
binary = "windsurf"
detect_paths = ["~/.windsurf"]
config_user = "~/.windsurf/mcp.json"
config_project = ".windsurf/mcp.json"
config_format = "json"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = []

[[agent]]
id = "cline"
display_name = "Cline"
binary = "cline"
detect_paths = ["~/.cline"]
config_user = "~/.cline/mcp.json"
config_project = ".cline/mcp.json"
config_format = "json"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = []

[[agent]]
id = "codex"
display_name = "Codex"
binary = "codex"
detect_paths = ["~/.codex"]
config_user = "~/.codex/mcp.json"
config_project = ".codex/mcp.json"
config_format = "json"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = ["codex", "--quiet"]

[[agent]]
id = "omp"
display_name = "OMP"
binary = "omp"
detect_paths = ["~/.omp"]
config_user = "~/.omp/mcp.json"
config_project = ".omp/mcp.json"
config_format = "json"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = ["omp", "--headless"]

[[agent]]
id = "gemini"
display_name = "Gemini"
binary = "gemini"
detect_paths = ["~/.gemini"]
config_user = "~/.gemini/settings.json"
config_project = ".gemini/settings.json"
config_format = "json"
mcp_section = "mcpServers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = []

[[agent]]
id = "vscode_copilot"
display_name = "VS Code + GitHub Copilot"
binary = "code"
detect_paths = [".vscode"]
config_user = ""
config_project = ".vscode/mcp.json"
config_format = "json"
mcp_section = "servers"
mcp_name = "lain"
transport = "stdio"
command = "/home/sebastian/.local/lain/lain"
default_args = ["--workspace", "{{workspace}}", "--transport", "stdio", "--embedding-model", "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx"]
headless_probe = []
```

- [ ] **Step 4: Implement `src/cmds/agents/manifest.rs`**

```rust
//! Loader for the single-source-of-truth agent manifest.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEntry {
    pub id: String,
    pub display_name: String,
    pub binary: String,
    #[serde(default)]
    pub detect_paths: Vec<String>,
    pub config_user: String,
    pub config_project: String,
    pub config_format: String,
    pub mcp_section: String,
    pub mcp_name: String,
    pub transport: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub default_args: Vec<String>,
    #[serde(default)]
    pub headless_probe: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub agent: Vec<AgentEntry>,
}

fn default_version() -> u32 { 1 }

pub const DEFAULT_MANIFEST: &str = include_str!("../../../agents/manifest.toml");

pub const SUPPORTED_AGENT_IDS: &[&str] = &[
    "claude", "kimi", "cursor", "continue", "windsurf", "cline",
    "codex", "omp", "gemini", "vscode_copilot",
];

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to parse agent manifest: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("agent manifest is missing required id field")]
    MissingId,
    #[error("agent manifest is missing required agent rows")]
    Empty,
}

pub fn load_manifest_from_str(src: &str) -> Result<Vec<AgentEntry>, ManifestError> {
    let parsed: ManifestFile = toml::from_str(src)?;
    if parsed.agent.is_empty() {
        return Err(ManifestError::Empty);
    }
    for a in &parsed.agent {
        if a.id.is_empty() {
            return Err(ManifestError::MissingId);
        }
    }
    Ok(parsed.agent)
}

pub fn load_manifest() -> Result<Vec<AgentEntry>, ManifestError> {
    load_manifest_from_str(DEFAULT_MANIFEST)
}

#[allow(dead_code)]
pub fn write_manifest(path: &Path, agents: &[AgentEntry]) -> std::io::Result<()> {
    let body = ManifestFile { version: 1, agent: agents.to_vec() };
    let s = toml::to_string_pretty(&body).map_err(std::io::Error::other)?;
    std::fs::write(path, s)
}
```

- [ ] **Step 5: Run the loader tests, confirm they pass**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::agents::tests
```

Expected: PASS.

---

### Task 3: Build the `lain-mcp-probe` crate

**Files:**
- Create: `crates/lain-mcp-probe/Cargo.toml`
- Create: `crates/lain-mcp-probe/src/lib.rs`
- Modify: workspace `Cargo.toml` to register the new crate in `[workspace]`

**Interfaces:**
- Consumes: `rust-mcp-sdk 1.0.1` client API.
- Produces:
  - `pub struct ProbeReport { pub installed: bool, pub config_valid: bool, pub mcp_reachable: bool, pub tools_count: Option<usize>, pub health: ProbeHealth }`
  - `pub enum ProbeHealth { Operational, Unreachable(String), Error(String) }`
  - `pub async fn probe_http(url: &str) -> ProbeReport`
  - `pub async fn probe_stdio(command: &str, args: &[&str]) -> ProbeReport`

- [ ] **Step 1: Register the new crate in the workspace**

In the top-level `Cargo.toml`, add `crates/lain-mcp-probe` to `[workspace] members`. Example edit:

```toml
[workspace]
members = [".", "crates/lain-mcp-probe"]
```

- [ ] **Step 2: Write the failing test for the probe**

In `crates/lain-mcp-probe/src/lib.rs`, start with:

```rust
//! MCP probe used by `lain agents verify`.

use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    pub installed: bool,
    pub config_valid: bool,
    pub mcp_reachable: bool,
    pub tools_count: Option<usize>,
    pub health: ProbeHealth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeHealth {
    Operational,
    Unreachable(String),
    Error(String),
}

impl ProbeReport {
    fn not_installed() -> Self {
        Self { installed: false, config_valid: false, mcp_reachable: false, tools_count: None, health: ProbeHealth::Unreachable("not installed".into()) }
    }
    fn from_error(stage: &str, e: impl ToString) -> Self {
        Self { installed: true, config_valid: true, mcp_reachable: false, tools_count: None, health: ProbeHealth::Error(format!("{stage}: {}", e.to_string())) }
    }
}

pub async fn probe_http(url: &str) -> ProbeReport {
    use std::time::Duration;
    use tokio::time::timeout;
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(5)).build() {
        Ok(c) => c,
        Err(e) => return ProbeReport::from_error("client", e),
    };
    let init_body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-11-25", "capabilities": {},
                    "clientInfo": { "name": "lain-agents-verify", "version": env!("CARGO_PKG_VERSION") } }
    });
    let resp = match client.post(url).json(&init_body).send().await {
        Ok(r) => r,
        Err(e) => return ProbeReport::from_error("initialize", e),
    };
    if !resp.status().is_success() {
        return ProbeReport::from_error("initialize", format!("http {}", resp.status()));
    }
    let _ = resp.bytes().await;
    let list_body = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}});
    let resp = match client.post(url).json(&list_body).send().await {
        Ok(r) => r,
        Err(e) => return ProbeReport::from_error("tools/list", e),
    };
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return ProbeReport::from_error("tools/list decode", e),
    };
    let tools_count = body.pointer("/result/tools").and_then(|t| t.as_array()).map(|a| a.len());
    let call_body = serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"get_health","arguments":{}}});
    let resp = match client.post(url).json(&call_body).send().await {
        Ok(r) => r,
        Err(e) => return ProbeReport::from_error("get_health", e),
    };
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return ProbeReport::from_error("get_health decode", e),
    };
    let text = body.pointer("/result/content/0/text").and_then(|t| t.as_str()).unwrap_or("");
    let health = if text.contains("Operational") {
        ProbeHealth::Operational
    } else {
        ProbeHealth::Error(text.chars().take(200).collect())
    };
    ProbeReport { installed: true, config_valid: true, mcp_reachable: true, tools_count, health }
}

pub async fn probe_stdio(command: &str, args: &[&str]) -> ProbeReport {
    let mut child = match Command::new(command).args(args).stdin(Stdio::piped())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return ProbeReport::from_error("spawn", e),
    };
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let init = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
        "params":{"protocolVersion":"2025-11-25","capabilities":{},
                  "clientInfo":{"name":"lain-agents-verify","version":env!("CARGO_PKG_VERSION")}}});
    if let Err(e) = stdin.write_all(format!("{}\n", init).as_bytes()).await {
        return ProbeReport::from_error("initialize", e);
    }
    let mut buf = [0u8; 65536];
    let _ = timeout(std::time::Duration::from_secs(5), stdout.read(&mut buf)).await;
    let list = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}});
    if let Err(e) = stdin.write_all(format!("{}\n", list).as_bytes()).await {
        return ProbeReport::from_error("tools/list", e);
    }
    let _ = timeout(std::time::Duration::from_secs(5), stdout.read(&mut buf)).await;
    let call = serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"get_health","arguments":{}}});
    if let Err(e) = stdin.write_all(format!("{}\n", call).as_bytes()).await {
        return ProbeReport::from_error("get_health", e);
    }
    let _ = timeout(std::time::Duration::from_secs(5), stdout.read(&mut buf)).await;
    let _ = child.kill().await;
    let text = String::from_utf8_lossy(&buf).to_string();
    let health = if text.contains("Operational") { ProbeHealth::Operational } else { ProbeHealth::Unreachable(text.chars().take(200).collect()) };
    ProbeReport { installed: true, config_valid: true, mcp_reachable: true, tools_count: None, health }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_http_against_unreachable_url() {
        let r = probe_http("http://127.0.0.1:1/mcp").await;
        assert!(!r.mcp_reachable);
        assert!(!matches!(r.health, ProbeHealth::Operational));
    }

    #[test]
    fn not_installed_shape() {
        let r = ProbeReport::not_installed();
        assert!(!r.installed);
        assert_eq!(r.health, ProbeHealth::Unreachable("not installed".into()));
    }
}

use tokio::time::timeout;
```

- [ ] **Step 3: Create the probe `Cargo.toml`**

In `crates/lain-mcp-probe/Cargo.toml`:

```toml
[package]
name = "lain-mcp-probe"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1.35", features = ["full"] }
reqwest = { version = "0.11", features = ["json", "rustls-tls"] }
thiserror = "1.0"

[dev-dependencies]
tokio = { version = "1.35", features = ["full"] }
```

- [ ] **Step 4: Run the probe tests, confirm they pass**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test -p lain-mcp-probe
```

Expected: PASS for both tests.

- [ ] **Step 5: Stage but do not commit**

```bash
git add Cargo.toml Cargo.lock crates/lain-mcp-probe
```

---

### Task 4: Implement the Claude and Kimi adapters

**Files:**
- Create: `src/cmds/agents/adapters/mod.rs`
- Create: `src/cmds/agents/adapters/claude.rs`
- Create: `src/cmds/agents/adapters/kimi.rs`
- Modify: `src/cmds/agents/mod.rs` to add `pub mod adapters;`

**Interfaces:**
- Consumes: `AgentEntry` from `manifest`; `std::path::Path`.
- Produces:
  - `pub trait AgentAdapter { fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError>; fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<serde_json::Value, AdapterError>; }`
  - `pub enum InstallScope { User, Project, Workspace }`
  - `pub fn adapter_for(id: &str) -> Option<Box<dyn AgentAdapter>>` mapping id → adapter.
  - `pub fn expand_home(p: &str) -> std::path::PathBuf`
  - `pub fn render_args(template: &[String], workspace: &str) -> Vec<String>` substitutes `{{workspace}}`.

- [ ] **Step 1: Write failing unit tests for the Claude adapter**

Append to `src/cmds/agents/mod.rs` `tests` module:

```rust
    #[test]
    fn expand_home_tilde() {
        use crate::cmds::agents::adapters::expand_home;
        let p = expand_home("~/foo");
        assert!(p.to_string_lossy().ends_with("/foo"));
    }

    #[test]
    fn render_args_substitutes_workspace() {
        use crate::cmds::agents::adapters::render_args;
        let out = render_args(
            &["--workspace".into(), "{{workspace}}".into(), "--transport".into(), "stdio".into()],
            "/abs/path",
        );
        assert_eq!(out, vec!["--workspace", "/abs/path", "--transport", "stdio"]);
    }
```

- [ ] **Step 2: Run test, confirm it fails**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::agents::tests
```

Expected: FAIL because `src/cmds/agents/adapters/mod.rs` does not exist.

- [ ] **Step 3: Implement `src/cmds/agents/adapters/mod.rs`**

```rust
//! Per-agent config adapters.

pub mod claude;
pub mod kimi;

use crate::cmds::agents::manifest::AgentEntry;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope { User, Project, Workspace }

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("serde: {0}")] Serde(#[from] serde_json::Error),
    #[error("toml: {0}")] Toml(#[from] toml::de::Error),
    #[error("config has unexpected shape: {0}")] Shape(String),
    #[error("adapter does not support {0:?} for {1}")] Unsupported(InstallScope, String),
}

pub trait AgentAdapter {
    fn id(&self) -> &'static str;
    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError>;
    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<serde_json::Value, AdapterError>;
}

pub fn adapter_for(id: &str) -> Option<Box<dyn AgentAdapter>> {
    match id {
        "claude" => Some(Box::new(claude::ClaudeAdapter)),
        "kimi"   => Some(Box::new(kimi::Kim iAdapter)),
        _ => None,
    }
}

pub fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

pub fn render_args(template: &[String], workspace: &str) -> Vec<String> {
    template.iter().map(|t| t.replace("{{workspace}}", workspace)).collect()
}

#[allow(dead_code)]
pub fn resolve_target(entry: &AgentEntry, scope: InstallScope) -> Option<PathBuf> {
    let raw = match scope {
        InstallScope::User => entry.config_user.as_str(),
        InstallScope::Project => entry.config_project.as_str(),
        InstallScope::Workspace => return None,
    };
    if raw.is_empty() { None } else { Some(expand_home(raw)) }
}
```

(Note: replace `kimi::Kim iAdapter` with `kimi::KimiAdapter` in the real file; the typo is in the prompt only.)

- [ ] **Step 4: Implement `src/cmds/agents/adapters/claude.rs`**

```rust
use super::{expand_home, render_args, AdapterError, AgentAdapter, InstallScope};
use crate::cmds::agents::manifest::AgentEntry;
use serde_json::{json, Value};
use std::path::Path;

pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str { "claude" }

    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path = expand_home(path);
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let mut doc: Value = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
        } else { json!({}) };
        let workspace = std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let args = render_args(&entry.default_args, &workspace);
        let server = json!({
            "command": entry.command,
            "args": args,
        });
        let obj = doc.as_object_mut().ok_or_else(|| AdapterError::Shape("root not object".into()))?;
        let servers = obj.entry(entry.mcp_section.clone()).or_insert_with(|| json!({}));
        let servers_obj = servers.as_object_mut().ok_or_else(|| AdapterError::Shape("mcp section not object".into()))?;
        servers_obj.insert(entry.mcp_name.clone(), server);
        let serialized = serde_json::to_string_pretty(&doc)?;
        std::fs::write(&path, serialized)?;
        Ok(())
    }

    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<Value, AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path: &Path = &expand_home(path);
        if !path.exists() { return Ok(Value::Null); }
        let raw = std::fs::read_to_string(path)?;
        let doc: Value = serde_json::from_str(&raw)?;
        Ok(doc.pointer(&format!("/{}", entry.mcp_section))
            .and_then(|s| s.get(&entry.mcp_name)).cloned()
            .unwrap_or(Value::Null))
    }
}
```

- [ ] **Step 5: Implement `src/cmds/agents/adapters/kimi.rs`**

```rust
use super::{expand_home, render_args, AdapterError, AgentAdapter, InstallScope};
use crate::cmds::agents::manifest::AgentEntry;
use serde_json::{json, Value};
use std::path::Path;

pub struct KimiAdapter;

impl AgentAdapter for KimiAdapter {
    fn id(&self) -> &'static str { "kimi" }

    fn install(&self, entry: &AgentEntry, scope: InstallScope) -> Result<(), AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path = expand_home(path);
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let workspace = std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let args = render_args(&entry.default_args, &workspace);
        let doc = json!({
            "name": "lain",
            "version": "0.4.2",
            "mcpServers": { "lain": { "command": entry.command, "args": args } }
        });
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
        Ok(())
    }

    fn read(&self, entry: &AgentEntry, scope: InstallScope) -> Result<Value, AdapterError> {
        let Some(path) = (match scope {
            InstallScope::User => entry.config_user.as_str(),
            InstallScope::Project => entry.config_project.as_str(),
            InstallScope::Workspace => return Err(AdapterError::Unsupported(scope, self.id().into())),
        }).into() else { return Err(AdapterError::Unsupported(scope, self.id().into())); };
        let path: &Path = &expand_home(path);
        if !path.exists() { return Ok(Value::Null); }
        let raw = std::fs::read_to_string(path)?;
        let doc: Value = serde_json::from_str(&raw)?;
        Ok(doc.pointer("/mcpServers/lain").cloned().unwrap_or(Value::Null))
    }
}
```

- [ ] **Step 6: Run the adapter unit tests, confirm they pass**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::agents::tests
```

Expected: PASS for `expand_home_tilde` and `render_args_substitutes_workspace`.

- [ ] **Step 7: Add a Claude adapter round-trip test**

Append to `src/cmds/agents/mod.rs` `tests` module:

```rust
    #[test]
    fn claude_round_trip_under_temp_home() {
        use crate::cmds::agents::adapters::{adapter_for, InstallScope};
        use crate::cmds::agents::manifest::load_manifest;
        use std::env;
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = env::var_os("HOME");
        env::set_var("HOME", tmp.path());
        let agents = load_manifest().expect("manifest");
        let entry = agents.iter().find(|a| a.id == "claude").expect("claude row");
        let adapter = adapter_for("claude").expect("claude adapter");
        adapter.install(entry, InstallScope::User).expect("install");
        let written = std::fs::read_to_string(tmp.path().join(".claude/settings.json")).expect("read");
        assert!(written.contains("\"mcpServers\""));
        assert!(written.contains("\"lain\""));
        if let Some(prev) = prev { env::set_var("HOME", prev); }
    }
```

Run:

```bash
cargo test --lib cmds::agents::tests
```

Expected: PASS.

- [ ] **Step 8: Stage but do not commit**

```bash
git add src/cmds/agents/
```

---

### Task 5: Implement the dispatcher and the `agents install` subcommand

**Files:**
- Create: `src/cmds/agents/install.rs`
- Modify: `src/cmds/agents/mod.rs` to register the install command
- Modify: `src/main.rs` to expose `lain agents install`

**Interfaces:**
- Consumes: `clap` (already a dep), `AgentEntry`, `AgentAdapter` trait, `InstallScope`.
- Produces:
  - `pub fn run_install(id: Option<&str>, all: bool, scope: InstallScope) -> anyhow::Result<()>`
  - Adds `Agents { install: ..., list: ..., verify: ... }` to the existing clap tree at `src/main.rs:34-62`.

- [ ] **Step 1: Write failing test for the dispatcher**

Append to `src/cmds/agents/mod.rs` `tests` module:

```rust
    #[test]
    fn run_install_all_writes_per_id() {
        use crate::cmds::agents::adapters::InstallScope;
        use crate::cmds::agents::install::run_install;
        use std::env;
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = env::var_os("HOME");
        env::set_var("HOME", tmp.path());
        // Limit to claude + kimi for speed.
        let ids = ["claude", "kimi"];
        for id in ids {
            run_install(Some(id), false, InstallScope::User).expect("install");
        }
        assert!(tmp.path().join(".claude/settings.json").exists());
        assert!(tmp.path().join(".kimi-code/plugins/managed/lain/kimi.plugin.json").exists());
        if let Some(prev) = prev { env::set_var("HOME", prev); }
    }
```

- [ ] **Step 2: Run test, confirm it fails**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::agents::tests
```

Expected: FAIL because `src/cmds/agents/install.rs` does not exist.

- [ ] **Step 3: Implement `src/cmds/agents/install.rs`**

```rust
use super::adapters::{adapter_for, InstallScope};
use super::manifest::load_manifest;
use anyhow::{anyhow, Result};

pub fn run_install(id: Option<&str>, all: bool, scope: InstallScope) -> Result<()> {
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
```

- [ ] **Step 4: Run dispatcher test, confirm it passes**

```bash
cargo test --lib cmds::agents::tests
```

Expected: PASS.

---

### Task 6: Wire the `agents` subcommand into the CLI

**Files:**
- Modify: `src/main.rs:33-79` (clap `Commands` enum) to add `Agents` variant
- Modify: `src/cmds/agents/mod.rs` to expose `install::run_install`, future `verify::run_verify`, `list::run_list`

- [ ] **Step 1: Add the clap `Agents` variant**

In `src/main.rs`, inside `enum Commands`, after the existing `Use { name: String }` variant, add:

```rust
    Agents {
        #[command(subcommand)]
        action: AgentsAction,
    },
```

Then add a new enum:

```rust
#[derive(Debug, Subcommand)]
enum AgentsAction {
    List,
    Install {
        #[arg(long, default_value = "user", value_parser = ["user", "project", "workspace"])]
        scope: String,
        #[arg(long)]
        all: bool,
        id: Option<String>,
    },
    Verify {
        #[arg(long)]
        all: bool,
        id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Remove {
        #[arg(long, default_value = "user", value_parser = ["user", "project", "workspace"])]
        scope: String,
        id: String,
    },
}
```

In `main`, inside the `match cmd { ... }` block, before the `Use` arm, add:

```rust
        Commands::Agents { action } => match action {
            AgentsAction::List => cmds::agents::list::run_list(),
            AgentsAction::Install { scope, all, id } => {
                use cmds::agents::adapters::InstallScope;
                let scope = match scope.as_str() {
                    "user" => InstallScope::User,
                    "project" => InstallScope::Project,
                    _ => InstallScope::Workspace,
                };
                cmds::agents::install::run_install(id.as_deref(), all, scope)
            }
            AgentsAction::Verify { all, id, json } => cmds::agents::verify::run_verify(all, id.as_deref(), json),
            AgentsAction::Remove { scope, id } => {
                use cmds::agents::adapters::InstallScope;
                let scope = match scope.as_str() {
                    "user" => InstallScope::User,
                    "project" => InstallScope::Project,
                    _ => InstallScope::Workspace,
                };
                cmds::agents::remove::run_remove(&id, scope)
            }
        },
```

(`list::run_list` and `remove::run_remove` are scaffolded in Task 7 alongside `verify`.)

- [ ] **Step 2: Build and confirm no regressions**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo build
cargo test --lib
```

Expected: PASS; new subcommand appears in `lain --help` output.

- [ ] **Step 3: Stage but do not commit**

```bash
git add src/main.rs src/cmds/agents/
```

---

### Task 7: Implement `list`, `verify`, and `remove`

**Files:**
- Create: `src/cmds/agents/list.rs`
- Create: `src/cmds/agents/verify.rs`
- Create: `src/cmds/agents/remove.rs`
- Modify: `src/cmds/agents/mod.rs` to add `pub mod list; pub mod verify; pub mod remove;`

**Interfaces:**
- Consumes: `AgentEntry`, `lain-mcp-probe` crate, the `AgentAdapter` trait, `InstallScope`.
- Produces:
  - `pub fn run_list() -> Result<()>`
  - `pub fn run_verify(all: bool, id: Option<&str>, json: bool) -> Result<()>`
  - `pub fn run_remove(id: &str, scope: InstallScope) -> Result<()>`

- [ ] **Step 1: Write failing test for `list`**

Append to `src/cmds/agents/mod.rs` `tests` module:

```rust
    #[test]
    fn list_returns_known_ids() {
        use crate::cmds::agents::list::run_list;
        run_list().expect("list runs");
    }
```

- [ ] **Step 2: Run test, confirm it fails**

```bash
cargo test --lib cmds::agents::tests
```

Expected: FAIL because `src/cmds/agents/list.rs` does not exist.

- [ ] **Step 3: Implement `src/cmds/agents/list.rs`**

```rust
use crate::cmds::agents::adapters::{adapter_for, InstallScope};
use crate::cmds::agents::manifest::load_manifest;
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
```

- [ ] **Step 4: Run test, confirm it passes**

```bash
cargo test --lib cmds::agents::tests
```

Expected: PASS.

- [ ] **Step 5: Implement `src/cmds/agents/verify.rs`**

```rust
use crate::cmds::agents::adapters::{adapter_for, InstallScope};
use crate::cmds::agents::manifest::load_manifest;
use anyhow::Result;
use lain_mcp_probe::{probe_http, probe_stdio, ProbeHealth, ProbeReport};

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
            if a.transport == "http" {
                let url = format!("http://localhost:{}/mcp", std::env::var("LAIN_PORT").unwrap_or_else(|_| "9999".into()));
                probe_http(&url).await
            } else {
                let mut args = a.default_args.clone();
                let workspace = std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                for arg in args.iter_mut() { *arg = arg.replace("{{workspace}}", &workspace); }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                probe_stdio(&a.command, &arg_refs).await
            }
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

pub fn run_verify(all: bool, id: Option<&str>, json: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(run_verify_async(all, id, json))
}
```

- [ ] **Step 6: Implement `src/cmds/agents/remove.rs`**

```rust
use crate::cmds::agents::adapters::{adapter_for, InstallScope};
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
    let path = crate::cmds::agents::adapters::expand_home(&path);
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
```

- [ ] **Step 7: Add `lain-mcp-probe` as a workspace dep**

In the top-level `Cargo.toml`, add a `lain-mcp-probe = { path = "crates/lain-mcp-probe" }` entry to the existing `[dependencies]` block so the binary crate can use it.

- [ ] **Step 8: Build, run all tests**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo build
cargo test --all-targets
```

Expected: PASS for everything, including the new `cmds::agents` tests.

- [ ] **Step 9: Stage but do not commit**

```bash
git add Cargo.toml Cargo.lock src/cmds/agents/
```

---

### Task 8: Add the end-to-end install-and-verify integration test

**Files:**
- Create: `tests/agents_install.rs`

- [ ] **Step 1: Write the integration test**

```rust
//! End-to-end harness: install every supported agent into a temp HOME,
//! then run `lain agents verify --all` against a temp Lain instance.

use std::process::{Command, Stdio};

fn lain_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_lain"))
}

#[test]
fn install_and_verify_for_supported_agents() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let port = pick_port();
    let env_overrides: Vec<(&str, &str)> = vec![
        ("HOME", tmp.path().to_str().unwrap()),
        ("XDG_CONFIG_HOME", tmp.path().join(".config").to_str().unwrap()),
        ("LAIN_PORT", Box::leak(port.to_string().into_boxed_str()) as &str),
    ];

    // 1. Start a fresh Lain server on the chosen port.
    let model = "/home/sebastian/.local/lain/models/all-MiniLM-L6-v2.onnx";
    let mut server = Command::new(lain_bin())
        .args(["--workspace", tmp.path().to_str().unwrap(),
               "--transport", "http", "--port", &port.to_string(),
               "--embedding-model", model])
        .envs(env_overrides.iter().copied())
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().expect("spawn lain");
    wait_for_health(&port);

    // 2. Install Claude and Kimi (smoke test of the two most-used agents).
    for id in ["claude", "kimi"] {
        let status = Command::new(lain_bin())
            .args(["agents", "install", "--scope", "user", id])
            .envs(env_overrides.iter().copied())
            .status().expect("install");
        assert!(status.success(), "install {id} failed");
    }

    // 3. Run `lain agents verify --all --json` and parse.
    let output = Command::new(lain_bin())
        .args(["agents", "verify", "--all", "--json"])
        .envs(env_overrides.iter().copied())
        .output().expect("verify");
    assert!(output.status.success());
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).expect("parse json");
    assert!(rows.iter().any(|r| r["id"] == "claude" && r["mcp_reachable"].as_bool() == Some(true)));
    assert!(rows.iter().any(|r| r["id"] == "kimi" && r["mcp_reachable"].as_bool() == Some(true)));

    // 4. Tear down the server.
    let _ = server.kill();
}

fn pick_port() -> String {
    use std::net::TcpListener;
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let p = l.local_addr().expect("addr").port();
    drop(l);
    p.to_string()
}

fn wait_for_health(port: &str) {
    let client = reqwest::blocking::Client::new();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"get_health","arguments":{}}}).to_string();
    for _ in 0..100 {
        if let Ok(r) = client.post(&url).header("content-type","application/json").body(body.clone()).send() {
            if r.status().is_success() { return; }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    panic!("server did not become healthy on port {port}");
}
```

- [ ] **Step 2: Run the new integration test**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --test agents_install -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Run the entire suite**

```bash
cargo test --all-targets
```

Expected: PASS for the whole repo, including the watcher tests and the new harness.

- [ ] **Step 4: Stage but do not commit**

```bash
git add tests/agents_install.rs
```

---

### Task 9: Author the agent-installation documentation

**Files:**
- Create: `docs/agent-installation.md`
- Modify: `README.md` (add a short section)
- Modify: `docs/quickstart-tools.md` (point at the new commands)
- Modify: `docs/QUICKSTART_AGENTS.md` (point at the new commands)

- [ ] **Step 1: Write `docs/agent-installation.md`**

Create the file with this content:

```markdown
# Agent Installation and Verification

Lain can be installed into every supported AI coding agent with a single
command. The HTTP singleton on port 9999 stays the shared server.

## Install

```bash
# Single agent, user-scope (recommended)
lain agents install --scope user claude

# All detected agents
lain agents install --all --scope user

# Project-scope (writes .vscode/mcp.json and friends)
lain agents install --scope project claude
```

`--scope` accepts `user` (writes to the agent's home config), `project`
(writes to the project's own config), or `workspace` (uses the active
Orca worktree context).

## Verify

```bash
# All installed agents, human-readable
lain agents verify --all

# One agent, machine-readable
lain agents verify --agent claude --json
```

Each row reports whether the agent is installed, whether the config
parses, whether MCP is reachable, the tool count, and the get_health
result.

## List

```bash
lain agents list
```

Prints every supported agent id, display name, install status, and
config path.

## Remove

```bash
lain agents remove --scope user claude
```

Removes the Lain entry from the chosen scope.

## Adding a new agent

Append a new `[[agent]]` row to `agents/manifest.toml` and, if needed,
a new adapter in `src/cmds/agents/adapters/<id>.rs`. Re-run
`cargo test --all-targets`. The integration test
`tests/agents_install.rs` exercises the install + verify path for every
manifest row.
```

- [ ] **Step 2: Update `README.md`**

Find the section that lists the install/verify commands and add a one-line
reference to the new chapter. Example:

```markdown
For per-agent installation across Kimi, Claude, Cursor, Continue,
Windsurf, Cline, Codex, OMP, and Gemini, see
`docs/agent-installation.md`.
```

- [ ] **Step 3: Update `docs/quickstart-tools.md`**

Replace any per-agent curl/script snippets with a pointer to
`lain agents install <agent>` and `lain agents verify --all`.

- [ ] **Step 4: Update `docs/QUICKSTART_AGENTS.md`**

Same edit: replace the manual agent glue with a pointer to the
manifest-driven installer.

- [ ] **Step 5: Build docs and confirm no broken cross-references**

```bash
grep -R "lain agents" docs/ README.md
```

Expected: every reference resolves to either a working command or a
documented section.

- [ ] **Step 6: Stage but do not commit**

```bash
git add docs/agent-installation.md README.md docs/quickstart-tools.md docs/QUICKSTART_AGENTS.md
```

---

### Task 10: Run the final automated verification

**Files:** none.

- [ ] **Step 1: Run focused tests**

```bash
export PATH="/home/sebastian/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
cargo test --lib cmds::agents
cargo test --test agents_install -- --nocapture
```

Expected: PASS.

- [ ] **Step 2: Run the full Rust test suite**

```bash
cargo test --all-targets
```

Expected: PASS with no regressions in the existing watcher, state,
tools, or handler suites.

- [ ] **Step 3: Build the release binary**

```bash
cargo build --release
```

Expected: clean build; `target/release/lain` is regenerated.

- [ ] **Step 4: Inspect the diff and status**

```bash
git diff --check
git status --short
git diff --stat -- Cargo.toml Cargo.lock src/cmds/agents/ tests/agents_install.rs docs/agent-installation.md README.md docs/quickstart-tools.md docs/QUICKSTART_AGENTS.md agents/manifest.toml crates/lain-mcp-probe/
```

Expected: only files declared in the File Map are changed; no
permission or generated-data drift.

- [ ] **Step 5: Stage but do not commit**

```bash
git add -N Cargo.toml Cargo.lock src/cmds/agents/ tests/agents_install.rs docs/agent-installation.md README.md docs/quickstart-tools.md docs/QUICKSTART_AGENTS.md agents/manifest.toml crates/lain-mcp-probe/
git status --short
```

(Use `git add -N` so all the touched files appear in `git status` as
intended-to-add; do not actually commit until the user authorizes.)

---

### Task 11: Live verification with Kimi and Claude clients

**Files:** none beyond runtime.

- [ ] **Step 1: Confirm permission baseline**

```bash
stat -c '%A %u:%g %n' /home/sebastian/monitor/monitor_dm_system/infra/neo4j/import
```

Expected: `drwx------ 7474:7474 ...` (unchanged from the previous plan).

- [ ] **Step 2: Install the rebuilt binary and restart the singleton**

After explicit user approval:

```bash
install -m 755 target/release/lain /home/sebastian/.local/lain/lain
/home/sebastian/monitor/monitor_dm_system/scripts/lain-server-manager.sh restart
```

- [ ] **Step 3: Run `lain agents verify --all --json` against the real user config**

```bash
$HOME/.local/lain/lain agents verify --all --json | head -100
```

Expected: every installed agent row shows `installed: true`,
`mcp_reachable: true`, `health: "Operational"`. No
`"Incompatible protocol version"` errors.

- [ ] **Step 4: Open one Claude terminal and one Kimi terminal in Orca**

Ask each to list its tools and run `get_health` through MCP. Expect
both to report `Operational` with no protocol-version error.

- [ ] **Step 5: Confirm server, watcher, and permissions are stable**

```bash
ps -p "$(cat /home/sebastian/monitor/monitor_dm_system/.lain/server.pid)" -o pid=,args=
stat -c '%A %u:%g %n' /home/sebastian/monitor/monitor_dm_system/infra/neo4j/import
```

Expected: same PID, same `drwx------ 7474:7474` permissions, no new
disconnect lines in `.lain/server.log`.

- [ ] **Step 6: Do not commit or push unless the user explicitly asks

No `git commit`, `git push`, `git reset`, or other mutations without
explicit authorization. Record the live evidence in
`/home/sebastian/orca/workspaces/lain/langostino/.superpowers/sdd/2026-08-08-agent-installation-design/task-11-report.md` when prompted.
