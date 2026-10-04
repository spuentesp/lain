//! §10.2 envelope rendering for contract tools.

use crate::server::mcp::contract_tools::{
    ToolOutcome, ANALYZER_VERSION, API_VERSION, TEXT_CAP_CHARS,
};
use serde_json::{json, Value};
use std::time::Instant;

/// Build a successful envelope (`§10.2`). `started` is the `Instant`
/// recorded when the tool started; `meta.elapsed_ms` is the only
/// non-deterministic field — every other key is byte-identical across
/// runs of the same input.
pub fn success_envelope(
    data: Value,
    snapshot: &str,
    reproducible: bool,
    started: Instant,
) -> Value {
    json!({
        "api_version": API_VERSION,
        "analyzer_version": ANALYZER_VERSION,
        "snapshot": snapshot,
        "reproducible": reproducible,
        "data": data,
        "meta": {
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }
    })
}

/// Build an error envelope (`§10.2`). `code` is a stable wire
/// identifier (see `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §13 for the
/// canonical list); `details` is omitted when `None`.
pub fn error_envelope(
    code: &str,
    message: impl Into<String>,
    details: Option<Value>,
    snapshot: &str,
    started: Instant,
) -> Value {
    let mut error = json!({
        "code": code,
        "message": message.into(),
        "retryable": matches!(code, "snapshot_not_ready" | "busy"),
    });
    if let Some(d) = details {
        error["details"] = d;
    }
    json!({
        "api_version": API_VERSION,
        "analyzer_version": ANALYZER_VERSION,
        "snapshot": snapshot,
        "error": error,
        "meta": {
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }
    })
}

/// Render a tool result as the (structured, text, is_error) tuple
/// `dispatch_tool_call` expects (`§10.2`). When `data` carries a
/// `scope`, the §9.6 sentence is appended to the rendered text — the
/// envelope itself still carries the structured `scope` for clients
/// that read it.
pub fn outcome(envelope: Value, data: &Value, render_text: String) -> ToolOutcome {
    let text = render_text_with_scope(render_text, data);
    let text = cap_2000(text);
    ToolOutcome {
        structured: envelope,
        text,
        is_error: false,
    }
}

pub fn error_outcome(
    code: &str,
    message: impl Into<String>,
    details: Option<Value>,
    snapshot: &str,
    started: Instant,
) -> ToolOutcome {
    let envelope = error_envelope(code, message, details, snapshot, started);
    let code_owned = code.to_string();
    let message_owned = envelope["error"]["message"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let text = format!("{code_owned}: {message_owned}");
    ToolOutcome {
        structured: envelope,
        text,
        is_error: true,
    }
}

/// When `data` carries a `scope`, append the §9.6 sentence so the
/// rendered text never omits the context (`§10.2`).
fn render_text_with_scope(text: String, data: &Value) -> String {
    let Some(scope) = data.get("scope") else {
        return text;
    };
    let sentence = super::scope::render_sentence(scope);
    if sentence.is_empty() {
        return text;
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&sentence);
    out
}

/// Hard cap on the rendered text (`§10.2`): at most
/// `TEXT_CAP_CHARS` characters, with a trailing `…` when truncated.
pub fn cap_2000(s: String) -> String {
    if s.chars().count() <= TEXT_CAP_CHARS {
        return s;
    }
    let mut out: String = s.chars().take(TEXT_CAP_CHARS - 1).collect();
    out.push('…');
    out
}

/// Validate the optional `api_version` argument (`§10.3`). Returns
/// `Ok(())` when the value is `1` or absent (absent means newest).
pub fn check_api_version(args: &serde_json::Map<String, Value>) -> Result<(), Value> {
    let Some(raw) = args.get("api_version") else {
        return Ok(());
    };
    let Some(n) = raw.as_u64() else {
        return Err(json!({ "arg": "api_version", "got": raw }));
    };
    if n as u32 != API_VERSION {
        return Err(json!({ "supported": [API_VERSION] }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_carries_required_fields() {
        let env = success_envelope(json!({"x": 1}), "live", false, Instant::now());
        assert_eq!(env["api_version"], json!(API_VERSION));
        assert_eq!(env["analyzer_version"], json!(ANALYZER_VERSION));
        assert_eq!(env["snapshot"], json!("live"));
        assert_eq!(env["reproducible"], json!(false));
        assert_eq!(env["data"], json!({"x": 1}));
        assert!(env["meta"]["elapsed_ms"].is_number());
    }

    #[test]
    fn error_envelope_has_code_message_retryable() {
        let env = error_envelope(
            "snapshot_not_found",
            "no such snapshot",
            None,
            "live",
            Instant::now(),
        );
        assert_eq!(env["error"]["code"], json!("snapshot_not_found"));
        assert_eq!(env["error"]["message"], json!("no such snapshot"));
        assert_eq!(env["error"]["retryable"], json!(false));
    }

    #[test]
    fn retryable_codes_set_retryable_true() {
        let env = error_envelope(
            "snapshot_not_ready",
            "still indexing",
            None,
            "live",
            Instant::now(),
        );
        assert_eq!(env["error"]["retryable"], json!(true));
    }

    #[test]
    fn cap_2000_truncates_with_ellipsis() {
        let s = "x".repeat(TEXT_CAP_CHARS + 50);
        let capped = cap_2000(s);
        assert_eq!(capped.chars().count(), TEXT_CAP_CHARS);
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn cap_2000_keeps_short_strings_intact() {
        let s = "hello".to_string();
        assert_eq!(cap_2000(s), "hello");
    }

    #[test]
    fn render_text_with_scope_appends_sentence_when_scope_present() {
        let scope = json!({
            "reviewed": [{"repo": "orders"}],
            "unreviewed": [{"repo": "reports", "reason": "not_ready"}],
            "configured_only": true,
        });
        let out = render_text_with_scope("hello".to_string(), &json!({"scope": scope}));
        assert!(out.starts_with("hello\n"));
        assert!(out.contains("reports"));
    }

    #[test]
    fn render_text_with_scope_omits_when_no_scope() {
        let out = render_text_with_scope("hello".to_string(), &json!({}));
        assert_eq!(out, "hello");
    }

    #[test]
    fn api_version_absent_is_accepted() {
        let args = serde_json::Map::new();
        assert!(check_api_version(&args).is_ok());
    }

    #[test]
    fn api_version_one_is_accepted() {
        let mut args = serde_json::Map::new();
        args.insert("api_version".to_string(), json!(1));
        assert!(check_api_version(&args).is_ok());
    }

    #[test]
    fn api_version_other_returns_unsupported() {
        let mut args = serde_json::Map::new();
        args.insert("api_version".to_string(), json!(99));
        let err = check_api_version(&args).unwrap_err();
        assert_eq!(err["supported"], json!([1]));
    }

    #[test]
    fn outcome_carries_structured_text_and_not_error() {
        let env = success_envelope(json!({"items": []}), "live", false, Instant::now());
        let o = outcome(env, &json!({"items": []}), "rendered".to_string());
        assert!(!o.is_error);
        assert!(o.text.starts_with("rendered"));
        assert!(o.structured["data"]["items"].is_array());
    }
}
