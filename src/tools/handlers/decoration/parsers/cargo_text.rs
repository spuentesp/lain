//! Regex-based fallback parser for human-readable cargo output

use std::path::PathBuf;

use crate::tools::handlers::decoration::types::{ParsedError, Severity};
use crate::tools::handlers::decoration::parsers::ErrorParser;

/// Regex-based fallback parser for human-readable cargo output
pub struct CargoTextParser;

impl CargoTextParser {
    pub fn new() -> Self {
        Self
    }
}

impl ErrorParser for CargoTextParser {
    fn parse(&self, output: &str) -> Vec<ParsedError> {
        let mut errors = Vec::new();
        let mut pending_error: Option<(String, Severity, Option<String>)> = None;

        for line in output.lines() {
            let trimmed = line.trim();

            // Look for error/warning headers: "error[E0308]: ..." or "warning[C0000]: ..."
            if trimmed.starts_with("error[E") || trimmed.starts_with("warning[C") {
                let (code, msg, severity) = if trimmed.starts_with("error[E") {
                    let (code, msg) = extract_code_and_message(trimmed, "error[", "error:");
                    (code, msg, Severity::Error)
                } else {
                    let (code, msg) = extract_code_and_message(trimmed, "warning[", "warning:");
                    (code, msg, Severity::Warning)
                };
                pending_error = Some((msg, severity, code));
            }
            // Handle bare "error:" and "warning:" without codes
            else if let Some(msg) = trimmed.strip_prefix("error:") {
                pending_error = Some((msg.trim().to_string(), Severity::Error, None));
            } else if trimmed.starts_with("warning:") && !trimmed.starts_with("warning[C") {
                pending_error = Some((trimmed[9..].trim().to_string(), Severity::Warning, None));
            }
            // Look for path markers: "  --> src/foo.rs:42:5" or "--> src/foo.rs:42:5"
            else if trimmed.starts_with("-->") || trimmed.contains("-->") {
                let path_part = if let Some(rest) = trimmed.strip_prefix("-->") {
                    rest.trim()
                } else if let Some(marker) = trimmed.find("-->") {
                    &trimmed[marker + 3..]
                } else {
                    continue;
                };

                // Parse path:line:col - be careful with Windows paths that have colons
                let parts: Vec<&str> = path_part.rsplitn(3, ':').collect();
                if parts.len() >= 2 {
                    let path = parts[parts.len() - 1].to_string();
                    let line_str = parts[parts.len() - 2];
                    let col_str = if parts.len() >= 3 { Some(parts[0]) } else { None };

                    if let Ok(line_num) = line_str.parse::<u32>() {
                        let column: Option<u32> = col_str.and_then(|c| c.parse().ok());

                        if let Some((message, severity, code)) = pending_error.take() {
                            errors.push(ParsedError {
                                path: PathBuf::from(path),
                                line: line_num,
                                column,
                                severity,
                                message,
                                code,
                            });
                        }
                    }
                }
            }
        }

        errors
    }
}

/// Extract error code and message from header line
fn extract_code_and_message(line: &str, prefix: &str, bare_prefix: &str) -> (Option<String>, String) {
    if let Some(bracket_end) = line.find(']') {
        let code = &line[prefix.len()..bracket_end];
        let message = if bracket_end + 2 <= line.len() {
            line[bracket_end + 2..].trim().to_string()
        } else {
            String::new()
        };
        (Some(code.to_string()), message)
    } else if let Some(msg) = line.strip_prefix(bare_prefix) {
        (None, msg.trim().to_string())
    } else {
        (None, line.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_error_with_code() {
        let output = "error[E0308]: mismatched types\n  --> src/lib.rs:10:5\n   |\n10 |     let x: i32 = \"hello\";\n   |                       ^^^^^^^ expected `,`";
        let parser = CargoTextParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Some("E0308".to_string()));
        assert_eq!(errors[0].line, 10);
    }

    #[test]
    fn test_parse_warning() {
        let output = "warning[C0000]: unused variable\n  --> src/lib.rs:5:3\n   |\n5  |     let unused = 42;\n   |                 ^^";
        let parser = CargoTextParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, Severity::Warning);
    }
}
