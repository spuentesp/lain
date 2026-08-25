use std::time::Duration;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

/// Shared reqwest blocking client (2 s request, 500 ms connect).
/// A wedged server can't hang the caller for the OS's full TCP
/// connect timeout (~75 s on Linux).
fn mcp_http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .connect_timeout(Duration::from_millis(500))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

/// Normalize `url` to the canonical MCP endpoint:
/// bare URL → append `/mcp`; full URL unchanged; trailing `/` stripped.
pub fn mcp_endpoint(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    if trimmed.ends_with("/mcp") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/mcp")
    }
}

/// Issue a `tools/call` JSON-RPC request to `url` and return the
/// response's `result` field as `serde_json::Value`. The `id: 1`
/// is fixed (the lain server is single-threaded per request).
pub fn post_tool_call(url: &str, name: &str, args: Value) -> Result<Value> {
    let endpoint = mcp_endpoint(url);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": args},
    });
    let client = mcp_http_client();
    let resp = client.post(&endpoint).json(&body).send()
        .context("HTTP send")?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {} from lain server", resp.status()));
    }
    let value: Value = resp.json().context("parse JSON-RPC response")?;
    if let Some(err) = value.get("error") {
        return Err(anyhow!("MCP error: {err}"));
    }
    value.get("result").cloned()
        .ok_or_else(|| anyhow!("no result in MCP response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_endpoint_appends_mcp_path() {
        assert_eq!(mcp_endpoint("http://localhost:9999"), "http://localhost:9999/mcp");
        assert_eq!(mcp_endpoint("http://localhost:9999/"), "http://localhost:9999/mcp");
    }

    #[test]
    fn mcp_endpoint_strips_trailing_slash_on_full_url() {
        assert_eq!(mcp_endpoint("http://localhost:9999/mcp"), "http://localhost:9999/mcp");
        assert_eq!(mcp_endpoint("http://localhost:9999/mcp/"), "http://localhost:9999/mcp");
    }
}
