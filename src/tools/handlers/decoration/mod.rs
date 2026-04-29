//! Error decoration for build/test/lint commands
//!
//! Composable decoration pattern: Command output -> Parser -> Enricher -> Report

pub mod decorator;
pub mod enricher;
pub mod parsers;
pub mod project;
pub mod types;

// ─── Public re-exports ─────────────────────────────────────────────────────────

pub use decorator::decorate_output;
pub use enricher::{ErrorEnricher, GraphEnricher};
pub use parsers::{get_parser, ErrorParser};
pub use parsers::{
    CargoJsonParser, CargoTextParser, GoBuildParser, GoTestParser, JestParser,
    PytestParser, TestOutputParser,
};
pub use project::{detect_project_profile, Toolchain, ProjectProfile};
pub use types::{
    EnrichedError, EnrichedReport, FailureSummary, ParsedError, Severity,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_severity_default() {
        assert_eq!(Severity::default(), Severity::Error);
    }

    #[test]
    fn test_parsed_error_clone() {
        let e = ParsedError {
            path: PathBuf::from("src/main.rs"),
            line: 42,
            column: Some(10),
            severity: Severity::Error,
            message: "mismatched types".to_string(),
            code: Some("E0308".to_string()),
        };
        let cloned = e.clone();
        assert_eq!(cloned.message, e.message);
    }
}
