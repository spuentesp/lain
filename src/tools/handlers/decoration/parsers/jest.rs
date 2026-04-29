//! Parser for Jest JSON or human-readable output

use std::path::PathBuf;

use crate::tools::handlers::decoration::types::{ParsedError, Severity};
use crate::tools::handlers::decoration::parsers::ErrorParser;

/// Parser for Jest JSON output (--json flag)
/// Format: JSON with "results" array containing failed test messages
pub struct JestParser;

impl ErrorParser for JestParser {
    fn parse(&self, output: &str) -> Vec<ParsedError> {
        // Try to parse as JSON
        let json: serde_json::Value = match serde_json::from_str(output) {
            Ok(v) => v,
            Err(_) => return parse_jest_text(output),
        };

        // Look for testResults array
        let results = json.get("testResults").and_then(|r| r.as_array());
        let Some(results) = results else {
            return parse_jest_text(output);
        };

        let mut errors = Vec::new();
        for result in results {
            let Some(assertion_results) = result.get("assertionResults").and_then(|a| a.as_array()) else {
                continue;
            };

            for assertion in assertion_results {
                let status = assertion.get("status").and_then(|s| s.as_str());
                if status != Some("failed") {
                    continue;
                }

                let title = assertion
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown");
                let line = assertion
                    .get("lineNumber")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(1) as u32;
                let path = assertion
                    .get("fullName")
                    .and_then(|_f| {
                        // Jest gives full test name, try to extract file path
                        // Format: "Test Suites > test-name > test-name > it('should do thing')"
                        // The file path is usually in ancestorResults
                        if let Some(ancestors) =
                            assertion.get("ancestorTitles").and_then(|a| a.as_array())
                        {
                            for ancestor in ancestors {
                                if let Some(a) = ancestor.as_str() {
                                    if a.ends_with(".test.js")
                                        || a.ends_with(".spec.js")
                                        || a.ends_with(".js")
                                    {
                                        return Some(a.to_string());
                                    }
                                    if a.contains(".test.") || a.contains(".spec.") {
                                        return Some(a.to_string());
                                    }
                                }
                            }
                        }
                        None
                    })
                    .unwrap_or_else(|| "unknown".to_string());

                let message = assertion
                    .get("failureMessages")
                    .and_then(|m| m.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|s| s.as_str())
                    .unwrap_or(title);

                errors.push(ParsedError {
                    path: PathBuf::from(&path),
                    line,
                    column: None,
                    severity: Severity::Error,
                    message: message.to_string(),
                    code: None,
                });
            }
        }

        errors
    }
}

/// Fallback text parser for Jest human-readable output
fn parse_jest_text(output: &str) -> Vec<ParsedError> {
    let mut errors = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        // Jest FAIL output: "FAIL path/to/file.js"
        if trimmed.starts_with("FAIL ") {
            if let Some(path) = trimmed.strip_prefix("FAIL ") {
                errors.push(ParsedError {
                    path: PathBuf::from(path.trim()),
                    line: 0,
                    column: None,
                    severity: Severity::Error,
                    message: trimmed.to_string(),
                    code: None,
                });
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jest_text_fail() {
        let output = "FAIL src/components/Button.test.js";
        let parser = JestParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path.to_string_lossy(), "src/components/Button.test.js");
    }

    #[test]
    fn test_parse_jest_json() {
        let json = r#"{"testResults":[{"assertionResults":[{"status":"failed","title":"renders correctly","fullName":"Button","lineNumber":42,"failureMessages":["Error: expect(received).toBe(expected)"]}]}]}"#;
        let parser = JestParser;
        let errors = parser.parse(json);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 42);
    }
}
