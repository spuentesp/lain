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
    pub fn not_installed() -> Self {
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

    /// Live-HTTP probe. Disabled by default; opt in with
    /// `PROBE_LIVE_HTTP=1` to exercise the probe against a real `-t http`
    /// singleton on `LAIN_PORT` (default 9999). Useful when the operator
    /// has a Lain server running on the loopback and wants to verify the
    /// HTTP code path end-to-end.
    #[tokio::test]
    async fn probe_http_against_live_singleton_when_enabled() {
        if std::env::var("PROBE_LIVE_HTTP").is_err() { return; }
        let port = std::env::var("LAIN_PORT").unwrap_or_else(|_| "9999".into());
        let url = format!("http://localhost:{port}/mcp");
        let r = probe_http(&url).await;
        assert!(r.mcp_reachable, "expected live singleton at {url}; got {:?}", r.health);
        assert_eq!(r.health, ProbeHealth::Operational);
        assert!(r.tools_count.unwrap_or(0) > 0, "expected tool list from singleton");
    }
}

use tokio::time::timeout;
