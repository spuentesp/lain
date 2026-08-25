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

/// Build the JSON-RPC 2.0 envelope used by both `post_json_rpc` and
/// `post_tool_call`. Extracted so the envelope shape (most importantly
/// the `method` field) is unit-testable without spinning up an HTTP
/// server. The `id: 1` is fixed — the lain server is single-threaded
/// per request, so a constant id is sufficient (and is what the prior
/// hand-rolled request bodies used).
fn build_envelope(method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    })
}

/// Issue a JSON-RPC request to `url` and return the response's
/// `result` field as `serde_json::Value`. `params` is sent verbatim
/// as the top-level `params` of the envelope, so callers shape it
/// however the target method expects (e.g. `tools/list` wants
/// `json!({})`, `tools/call` wants `{"name": ..., "arguments": ...}`).
pub fn post_json_rpc(url: &str, method: &str, params: Value) -> Result<Value> {
    let endpoint = mcp_endpoint(url);
    let body = build_envelope(method, params);
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

/// Issue a `tools/call` JSON-RPC request — thin wrapper around
/// `post_json_rpc` for the common case of calling a named tool.
pub fn post_tool_call(url: &str, name: &str, args: Value) -> Result<Value> {
    post_json_rpc(url, "tools/call", json!({"name": name, "arguments": args}))
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

    /// `build_envelope` must put the caller-supplied `method` in the
    /// top-level `method` field — NOT under `params.name`. This is the
    /// regression guard for the doctor `tools/list` bug: a previous
    /// refactor conflated tool-call and JSON-RPC envelopes and sent
    /// `{"method": "tools/call", "params": {"name": "tools/list"}}`,
    /// which no server-side dispatcher can match.
    #[test]
    fn build_envelope_uses_supplied_method_at_top_level() {
        let env = build_envelope("tools/list", json!({}));
        assert_eq!(env["jsonrpc"], "2.0");
        assert_eq!(env["id"], 1);
        assert_eq!(env["method"], "tools/list");
        assert_eq!(env["params"], json!({}));
        // Defensive: assert `params.name` is NOT set, so a future
        // refactor that re-introduces a tool-call-shaped envelope
        // for top-level methods fails fast.
        assert!(env["params"].get("name").is_none());
    }

    /// `post_tool_call`'s envelope must match what `post_json_rpc`
    /// would build for `("tools/call", {"name": N, "arguments": A})`.
    /// `cli/hooks.rs` callers depend on this exact shape — a change
    /// silently shifts the JSON the lain server receives and breaks
    /// every hook.
    #[test]
    fn post_tool_call_envelope_matches_post_json_rpc() {
        let name = "register_agent";
        let args = json!({"agent": "x", "kind": "claude-code"});
        let env = build_envelope("tools/call", json!({"name": name, "arguments": args}));
        assert_eq!(env["method"], "tools/call");
        assert_eq!(env["params"]["name"], name);
        assert_eq!(env["params"]["arguments"], args);
    }
}
