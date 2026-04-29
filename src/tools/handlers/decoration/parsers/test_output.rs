//! Parser for cargo test output (human-readable format)

use std::path::PathBuf;

use crate::tools::handlers::decoration::types::{ParsedError, Severity};
use crate::tools::handlers::decoration::parsers::ErrorParser;

/// Parser for cargo test output (human-readable format)
pub struct TestOutputParser;

impl ErrorParser for TestOutputParser {
    fn parse(&self, output: &str) -> Vec<ParsedError> {
        let mut errors = Vec::new();

        for line in output.lines() {
            // Test failures look like: "test tests::module::test_name ... FAILED"
            if line.contains(" FAILED") {
                if let Some(start) = line.find("test ") {
                    let rest = &line[start + 5..];
                    if let Some(end) = rest.find(" ...") {
                        let test_path = &rest[..end];
                        // Convert test path to file path heuristic: tests::foo::bar -> tests/foo/bar
                        let file_path = test_path.replace("::", "/");

                        errors.push(ParsedError {
                            path: PathBuf::from(file_path),
                            line: 0, // Test output doesn't give line numbers
                            column: None,
                            severity: Severity::Error,
                            message: line.to_string(),
                            code: None,
                        });
                    }
                }
            }
            // Thread panics: "thread 'thread_name' panicked at 'message', src/file.rs:line"
            else if line.contains("panicked") && line.contains(".rs:") {
                for part in line.split_whitespace() {
                    if part.contains(".rs:") {
                        if let Some(path_end) = part.find(".rs:") {
                            let path = format!("{}{}", &part[..path_end], ".rs");
                            let after_rs = &part[path_end + 4..]; // skip ".rs:" (4 chars), e.g., "42:5"
                            // Split by ':' to get line and column
                            let line_parts: Vec<&str> = after_rs.splitn(2, ':').collect();
                            if let Ok(line_num) = line_parts[0].parse::<u32>() {
                                let column: Option<u32> = line_parts.get(1).and_then(|c| c.parse().ok());
                                errors.push(ParsedError {
                                    path: PathBuf::from(path),
                                    line: line_num,
                                    column,
                                    severity: Severity::Error,
                                    message: line.to_string(),
                                    code: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_failed_test() {
        let output = "test tests::module::test_name ... FAILED";
        let parser = TestOutputParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, Severity::Error);
    }

    #[test]
    fn test_parse_panic_line() {
        let output = "thread 'worker' panicked at 'assertion failed', src/producer.rs:42:5";
        let parser = TestOutputParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 42);
        assert_eq!(errors[0].column, Some(5));
    }
}
