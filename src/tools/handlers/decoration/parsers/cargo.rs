//! Parser for cargo JSON output (--message-format=json)

use serde::Deserialize;
use std::path::PathBuf;

use crate::tools::handlers::decoration::types::{ParsedError, Severity};
use crate::tools::handlers::decoration::parsers::ErrorParser;

/// Parser for cargo JSON output (--message-format=json)
pub struct CargoJsonParser;

impl ErrorParser for CargoJsonParser {
    fn parse(&self, output: &str) -> Vec<ParsedError> {
        output
            .lines()
            .filter_map(|line| serde_json::from_str::<CargoMessage>(line).ok())
            .filter(|msg| msg.reason.as_deref() == Some("compiler-message"))
            .filter_map(convert_cargo_message)
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct CargoMessage {
    reason: Option<String>,
    #[serde(rename = "target")]
    _target: Option<CargoTarget>,
    message: Option<CargoDiagnostic>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(rename = "src_path")]
    _src_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoDiagnostic {
    message: String,
    code: Option<CargoCode>,
    level: String,
    spans: Vec<CargoSpan>,
    #[serde(rename = "rendered")]
    _rendered: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoCode {
    code: String,
}

#[derive(Debug, Deserialize)]
struct CargoSpan {
    #[serde(rename = "file_name")]
    file_name: String,
    line_start: u32,
    #[allow(dead_code)]
    line_end: u32,
    column_start: u32,
    #[allow(dead_code)]
    column_end: u32,
    is_primary: bool,
    #[allow(dead_code)]
    text: Vec<CargoSpanText>,
}

#[derive(Debug, Deserialize)]
struct CargoSpanText {
    #[allow(dead_code)]
    text: String,
}

fn convert_cargo_message(msg: CargoMessage) -> Option<ParsedError> {
    let diag = msg.message?;
    let level = match diag.level.as_str() {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "note" => Severity::Note,
        "help" => Severity::Help,
        _ => return None,
    };

    let primary = diag.spans.iter().find(|s| s.is_primary)?;

    let message = diag.message.clone();
    let code = diag.code.as_ref().map(|c| c.code.clone());

    Some(ParsedError {
        path: PathBuf::from(&primary.file_name),
        line: primary.line_start,
        column: Some(primary.column_start),
        severity: level,
        message,
        code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cargo_json_parse_valid() {
        let json = r#"{"reason":"compiler-message","target":{"name":"lib","src_path":"/src/lib.rs"},"message":{"message":"expected `,`","code":{"code":"E0277"},"level":"error","spans":[{"file_name":"src/lib.rs","line_start":10,"line_end":10,"column_start":20,"column_end":21,"is_primary":true,"text":[{"text":"fn foo()"}]}],"rendered":null}}"#;
        let parser = CargoJsonParser;
        let errors = parser.parse(json);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].message, "expected `,`");
        assert_eq!(errors[0].code, Some("E0277".to_string()));
    }

    #[test]
    fn test_cargo_json_ignores_non_message() {
        let json = r#"{"reason":"build-finished","message":null}"#;
        let parser = CargoJsonParser;
        let errors = parser.parse(json);
        assert_eq!(errors.len(), 0);
    }
}
