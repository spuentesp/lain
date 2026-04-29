//! Parsers for Go build and test output

use std::path::PathBuf;

use crate::tools::handlers::decoration::types::{ParsedError, Severity};
use crate::tools::handlers::decoration::parsers::ErrorParser;

/// Parser for Go build output (human-readable format)
/// Format: `path/to/file.go:line:col: message`
pub struct GoBuildParser;

impl ErrorParser for GoBuildParser {
    fn parse(&self, output: &str) -> Vec<ParsedError> {
        let mut errors = Vec::new();

        for line in output.lines() {
            let trimmed = line.trim();
            // Go build errors: "path/file.go:line:col: message"
            // Examples:
            //   "./main.go:10:5: undefined: foo"
            //   "src/bar.go:3:8: cannot find package"
            if let Some(pos) = trimmed.find(".go:") {
                let path_part = &trimmed[..pos];
                let rest = &trimmed[pos + 4..]; // skip ".go:"
                let parts: Vec<&str> = rest.splitn(2, ':').collect();
                if parts.len() >= 2 {
                    let line_str = parts[0];
                    let message = parts[1].trim();

                    let severity = if message.starts_with("undefined")
                        || message.starts_with("cannot find")
                        || message.starts_with("missing")
                        || message.starts_with("invalid")
                        || message.starts_with("syntax error")
                        || message.starts_with("expected")
                        || message.starts_with("assigned")
                        || message.starts_with("cannot")
                        || message.starts_with("undeclared")
                    {
                        Severity::Error
                    } else if message.starts_with("warning:") {
                        Severity::Warning
                    } else {
                        Severity::Error
                    };

                    if let Ok(line_num) = line_str.parse::<u32>() {
                        errors.push(ParsedError {
                            path: PathBuf::from(format!("{}.go", path_part)),
                            line: line_num,
                            column: None,
                            severity,
                            message: message.to_string(),
                            code: None,
                        });
                    }
                }
            }
        }

        errors
    }
}

/// Parser for Go test output (human-readable format)
/// Format: `--- FAIL: TestName (time)\n\tpath/file.go:line: message`
pub struct GoTestParser;

impl ErrorParser for GoTestParser {
    fn parse(&self, output: &str) -> Vec<ParsedError> {
        let mut errors = Vec::new();
        let mut pending_test: Option<String> = None;

        for line in output.lines() {
            let trimmed = line.trim();
            // Test failure header: "--- FAIL: TestName"
            if trimmed.starts_with("--- FAIL:") {
                if let Some(name) = trimmed.strip_prefix("--- FAIL:") {
                    pending_test = Some(name.trim().to_string());
                }
            }
            // Failure details: "\tpath/file.go:line: message"
            else if line.starts_with("\t") && trimmed.contains(".go:") {
                if let Some(pos) = trimmed.find(".go:") {
                    let path_part = &trimmed[..pos];
                    let rest = &trimmed[pos + 4..];
                    let parts: Vec<&str> = rest.splitn(2, ':').collect();
                    if parts.len() >= 2 {
                        let line_str = parts[0];
                        let message = parts[1].trim();

                        if let Ok(line_num) = line_str.parse::<u32>() {
                            let test_name = pending_test.take();
                            errors.push(ParsedError {
                                path: PathBuf::from(format!("{}.go", path_part)),
                                line: line_num,
                                column: None,
                                severity: Severity::Error,
                                message: format!(
                                    "{}{}",
                                    test_name.map(|t| format!("[{}] ", t)).unwrap_or_default(),
                                    message
                                ),
                                code: None,
                            });
                        }
                    }
                }
            }
            // Reset on non-failure output
            else if !trimmed.is_empty() && !trimmed.starts_with("---") && !trimmed.starts_with("===") {
                pending_test = None;
            }
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_build_parse() {
        // Format: path/file.go:line:col: message
        let output = "./main.go:10:5: undefined: foo";
        let parser = GoBuildParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 10);
        // message is "5: undefined: foo" (col preserved in message)
        assert_eq!(errors[0].message, "5: undefined: foo");
    }

    #[test]
    fn test_go_test_parse() {
        // Format: --- FAIL: TestName\n\tpath/file.go:line:col: message
        let output = "--- FAIL: TestFoo (0.00s)\n\t./main.go:42:15: expected 'a' but found 'b'";
        let parser = GoTestParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 42);
        // message includes full test name "[TestFoo (0.00s)]" as prefix
        assert!(errors[0].message.contains("TestFoo"));
    }
}
