//! ErrorParser trait and parser registry

use crate::tools::handlers::decoration::types::ParsedError;

pub mod cargo;
pub mod cargo_text;
pub mod test_output;
pub mod go;
pub mod python;
pub mod jest;

pub use cargo::CargoJsonParser;
pub use cargo_text::CargoTextParser;
pub use test_output::TestOutputParser;
pub use go::{GoBuildParser, GoTestParser};
pub use python::PytestParser;
pub use jest::JestParser;

/// Trait for parsing structured output into errors
pub trait ErrorParser: Send + Sync {
    fn parse(&self, output: &str) -> Vec<ParsedError>;
}

// Static instance for stateless text parser.
static TEXT_PARSER: CargoTextParser = CargoTextParser;

/// Get a parser by its registered ID.
///
/// Built-in IDs:
/// - `cargo-json` - cargo JSON output (rust build)
/// - `cargo-test` - cargo test output (rust test)
/// - `go-build` - go build human-readable output
/// - `go-test` - go test output with test names
/// - `jest` - jest JSON or text output
/// - `pytest` - pytest error output
/// - `cargo-text` / `text` - generic cargo-style fallback parser
pub fn get_parser(id: &str) -> Option<&'static dyn ErrorParser> {
    match id {
        "cargo-json" => Some(&CargoJsonParser as &dyn ErrorParser),
        "cargo-test" => Some(&TestOutputParser as &dyn ErrorParser),
        "go-build" => Some(&GoBuildParser as &dyn ErrorParser),
        "go-test" => Some(&GoTestParser as &dyn ErrorParser),
        "jest" => Some(&JestParser as &dyn ErrorParser),
        "pytest" => Some(&PytestParser as &dyn ErrorParser),
        "cargo-text" | "text" => Some(&TEXT_PARSER as &dyn ErrorParser),
        _ => None,
    }
}
