//! Fuzz target: parse arbitrary bytes as an MCP JSON-RPC envelope.
//!
//! `src/server/mcp/handler.rs` parses untrusted HTTP body bytes as
//! a JSON-RPC 2.0 envelope using `serde_json::from_str::<Value>`, then
//! dispatches on the `method` field. We exercise the same parse path
//! here; the dispatch logic itself is `match` on the method string,
//! which serde_json's parser has been fuzzed against upstream, so the
//! remaining attack surface for us is custom `serde::Deserialize`
//! impls and any pre-dispatch structural checks.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // Parse as serde_json::Value (the same type the handler parses).
    let value: serde_json::Value = match serde_json::from_slice(data) {
        Ok(v) => v,
        Err(_) => return,
    };

    // Mirror the structural checks the handler does before dispatch:
    // it reads `method` and `id` and `params` fields. We're looking
    // for panics in custom deserializers or in any post-parse
    // validation; serde_json's own Value parse is upstream-tested.
    let _ = value.get("method").and_then(|m| m.as_str());
    let _ = value.get("id");
    let _ = value.get("params");
});
