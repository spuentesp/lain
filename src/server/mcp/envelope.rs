//! MCP envelope helpers shared by the stdio and HTTP transports:
//! `CallToolResult` construction, `_meta` decoration, and argument
//! schema generation. Pure functions — no state, no I/O, no
//! async — so the dispatchers in `handler.rs` (stdio) and the HTTP
//! closures (which capture these via free-fn pointers) can both call
//! them without owning anything.

use crate::overlay::VolatileOverlay;
use rust_mcp_schema::{CallToolResult, ContentBlock, TextContent};

/// Wrap a string payload in a `CallToolResult` with a single text block.
///
/// Every response carries the overlay's current `revision` in the
/// outer `_meta` envelope (not in `content[0].text`). This is the
/// single chokepoint for adding the field — both stdio and HTTP
/// dispatch sites route every `is_error: true` / `is_error: false`
/// response through here (or through the `jsonrpc_tool_result`
/// closure in the HTTP path that adds the same `_meta`), so the
/// wire contract is uniform across the entire tool surface.
///
/// The inner `content[0].text` is passed through verbatim — bare
/// arrays, strings, Markdown, and JSON objects all retain their
/// existing shape. Revisions live in `_meta.revision`, which is
/// additive at the `CallToolResult` envelope level and is supported
/// by every `CallToolResult` constructor in
/// `rust-mcp-schema` 0.10 (the version pinned in `Cargo.toml`).
pub fn tool_text_result(
    text: String,
    is_error: bool,
    overlay: &VolatileOverlay,
    static_graph_generation_unix: Option<i64>,
) -> CallToolResult {
    CallToolResult {
        content: vec![ContentBlock::TextContent(TextContent::new(text, None, None))],
        is_error: Some(is_error),
        meta: revision_meta(overlay, static_graph_generation_unix),
        structured_content: None,
    }
}

/// Build a `_meta` map for the `CallToolResult` envelope that
/// records the overlay's current `revision`. Read once at construction
/// time so the value is stable for the lifetime of the response —
/// re-reading on the receiving end could see a different revision
/// if the overlay advances while the client is parsing.
///
/// The map is intentionally minimal (single key): the protocol
/// reserves `_meta` for arbitrary keys, but adding more would
/// require a coordinated client-side bump. Round 2 of Task 1.3
/// introduces just `revision`.
pub fn revision_meta(
    overlay: &VolatileOverlay,
    static_graph_generation_unix: Option<i64>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "revision".to_string(),
        serde_json::Value::Number(overlay.current_revision().into()),
    );
    meta.insert(
        "static_graph_generation".to_string(),
        match static_graph_generation_unix {
            Some(ts) => serde_json::Value::Number(ts.into()),
            None => serde_json::Value::Null,
        },
    );
    Some(meta)
}

/// Schema for one tool argument. The MCP `tools/list` response
/// describes each tool's `input_schema` as a JSON Schema object;
/// callers that don't include the schema field get rejected by
/// schema-respecting clients — Claude Code serialized the array into
/// a JSON *string* and the handler rejected it with "invalid type:
/// string, expected a sequence" (found in the live end-to-end review).
///
/// `name` is the argument key (e.g. `"files"`, `"symbols"`, `"limit"`).
/// The default branch returns `{"type": "string"}` for unknown
/// argument names; the typed branches cover the special-cased args
/// (claim/release's `files`, the generic `symbols` array, `limit`).
pub fn arg_property_schema(name: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut p = serde_json::Map::new();
    match name {
        "files" => {
            p.insert("type".into(), "array".into());
            p.insert(
                "items".into(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "symbols": { "type": "array", "items": { "type": "string" } },
                        "intent": { "type": "string", "enum": ["read", "edit"] },
                        "ttl_seconds": { "type": "integer" }
                    },
                    "required": ["path"]
                }),
            );
            p.insert("description".into(), "files to claim or release".into());
        }
        "symbols" => {
            p.insert("type".into(), "array".into());
            p.insert("items".into(), serde_json::json!({ "type": "string" }));
            p.insert("description".into(), "symbol names".into());
        }
        "limit" => {
            p.insert("type".into(), "integer".into());
            p.insert("description".into(), "max results".into());
        }
        // Booleans must be typed: the generic fallback is `string`, and
        // a schema-respecting client would send "true" as text, which
        // `serde_json::from_value::<Option<bool>>` rejects — the same
        // class of bug that made `claim_files`'s array arg unusable.
        "include_background" => {
            p.insert("type".into(), "boolean".into());
            p.insert(
                "description".into(),
                "also list cron/CI (background) agents".into(),
            );
        }
        "head" => {
            p.insert("type".into(), "string".into());
            p.insert(
                "description".into(),
                "git ref to compare against base; defaults to HEAD".into(),
            );
        }
        _ => {
            p.insert("type".into(), "string".into());
            p.insert("description".into(), name.into());
        }
    }
    p
}
