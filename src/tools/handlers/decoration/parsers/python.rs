//! Parser for Python pytest output (human-readable short format)

use std::path::PathBuf;

use crate::tools::handlers::decoration::types::{ParsedError, Severity};
use crate::tools::handlers::decoration::parsers::ErrorParser;

/// Parser for Python pytest output (human-readable short format)
/// Format: `path/file.py:line: error: message`
pub struct PytestParser;

impl ErrorParser for PytestParser {
    fn parse(&self, output: &str) -> Vec<ParsedError> {
        let mut errors = Vec::new();

        for line in output.lines() {
            let trimmed = line.trim();
            // Pytest error format: "path/file.py:line: [error|warning]: message"
            // Also handles: "path/file.py:line: error: message"
            if let Some(pos) = find_python_error_pos(trimmed) {
                // pos is where ".py" starts in the filename
                // Split at the ".", then the rest starts with ":line: message"
                let (path_part, rest) = trimmed.split_at(pos + 3); // +3 to skip past ".py"
                let rest = &rest[1..]; // skip the ":" after ".py"

                // rest is now "line: message"
                if let Some(colon_pos) = rest.find(':') {
                    let path = path_part.trim();
                    let line_part = &rest[..colon_pos];
                    let message = rest[colon_pos + 1..].trim();

                    // Determine severity
                    let severity = if message.to_lowercase().contains("error")
                        || message.to_lowercase().contains("failed")
                        || message.to_lowercase().contains("exception") {
                        Severity::Error
                    } else {
                        Severity::Warning
                    };

                    if let Ok(line_num) = line_part.parse::<u32>() {
                        errors.push(ParsedError {
                            path: PathBuf::from(path),
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

/// Find the position of a Python file reference in a line (e.g., ".py:10:")
fn find_python_error_pos(line: &str) -> Option<usize> {
    // Look for .py:digits: pattern (digits may be preceded by non-digit chars like ':')
    let mut pos = 0;
    while pos < line.len() {
        if let Some(py_pos) = line[pos..].find(".py:") {
            let after_py = py_pos + 3; // skip ".py"
            let rest = &line[pos + after_py..];
            // Skip non-digit chars (like leading ':' in ":10:") then find digits
            let after_non_digit = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
            let after_digits = rest[after_non_digit..].find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len() - after_non_digit);
            let total_skipped = after_non_digit + after_digits;
            if after_non_digit < rest.len()
                && rest.len() > total_skipped
                && &rest[total_skipped..total_skipped + 1] == ":"
            {
                return Some(pos + py_pos);
            }
            pos += py_pos + 1;
        } else {
            break;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pytest_parse_error() {
        let output = "tests/test_foo.py:42: Error: assertion failed";
        let parser = PytestParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 42);
        assert_eq!(errors[0].severity, Severity::Error);
    }

    #[test]
    fn test_pytest_parse_warning() {
        let output = "src/bar.py:10: Warning: unused import 'os'";
        let parser = PytestParser;
        let errors = parser.parse(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, Severity::Warning);
    }
}
