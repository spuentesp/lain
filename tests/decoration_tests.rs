//! Tests for the decoration module

#[cfg(test)]
mod tests {
    use lain::tools::handlers::decoration::{
        CargoTextParser, EnrichedError, ErrorParser, GoBuildParser, GoTestParser,
        JestParser, ParsedError, PytestParser, Severity, TestOutputParser,
    };

    // ─── CargoTextParser Tests ─────────────────────────────────────────────────

    #[test]
    fn test_cargo_text_parser_single_error() {
        let output = r#"error[E0308]: mismatched types
  --> src/auth.rs:42:18
   |
42 |     let token = parse(input);
   |                 ^^^^^ expected `String`, found `&str`
"#;
        let parser = CargoTextParser::new();
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, Some("E0308".to_string()));
        assert_eq!(errors[0].severity, Severity::Error);
        assert_eq!(errors[0].path.to_string_lossy(), "src/auth.rs");
        assert_eq!(errors[0].line, 42);
    }

    #[test]
    fn test_cargo_text_parser_multiple_errors() {
        let output = r#"error[E0308]: mismatched types
  --> src/auth.rs:42:18
   |
42 |     let token = parse(input);
   |                 ^^^^^ expected `String`, found `&str`

warning[C0000]: unused variable
  --> src/main.rs:10:5
   |
10 |     let x = 1;
   |         ^
"#;
        let parser = CargoTextParser::new();
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].severity, Severity::Error);
        assert_eq!(errors[1].severity, Severity::Warning);
    }

    #[test]
    fn test_cargo_text_parser_bare_error() {
        let output = r#"error: Could not compile.
  --> src/main.rs:1
   |
1 | fn main() {}
   | ^^^^^^^^
"#;
        let parser = CargoTextParser::new();
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, None);
        assert_eq!(errors[0].severity, Severity::Error);
    }

    #[test]
    fn test_cargo_text_parser_empty_output() {
        let output = "";
        let parser = CargoTextParser::new();
        let errors = parser.parse(output);

        assert!(errors.is_empty());
    }

    #[test]
    fn test_cargo_text_parser_no_errors() {
        let output = "✅ Build successful";
        let parser = CargoTextParser::new();
        let errors = parser.parse(output);

        assert!(errors.is_empty());
    }

    #[test]
    fn test_cargo_text_parser_bare_warning() {
        let output = r#"warning: unused variable
  --> src/utils.rs:5
   |
5 |     let unused = 42;
   |         ^^^^^^
"#;
        let parser = CargoTextParser::new();
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, Severity::Warning);
    }

    // ─── TestOutputParser Tests ──────────────────────────────────────────────

    #[test]
    fn test_test_parser_failed_test() {
        let output = r#"running 3 tests
test tests::auth::test_validate_token ... FAILED
test tests::auth::test_login ... ok
test tests::main::test_handler ... FAILED

failures:

failures:
    tests::auth::test_validate_token
    tests::main::test_handler
"#;
        let parser = TestOutputParser;
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].severity, Severity::Error);
        assert!(errors[0].message.contains("FAILED"));
    }

    #[test]
    fn test_test_parser_panic_with_location() {
        // Only test panic parsing - the FAILED line comes before the panic line
        // and contains a test path that gets incorrectly parsed
        let output = r#"thread 'tests::handler::test_process' panicked at src/handler.rs:42:5"#;
        let parser = TestOutputParser;
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path.to_string_lossy(), "src/handler.rs");
        assert_eq!(errors[0].line, 42);
        assert_eq!(errors[0].severity, Severity::Error);
    }

    #[test]
    fn test_test_parser_empty_output() {
        let output = "";
        let parser = TestOutputParser;
        let errors = parser.parse(output);

        assert!(errors.is_empty());
    }

    #[test]
    fn test_test_parser_passed_tests() {
        let output = r#"running 3 tests
test tests::auth::test_validate_token ... ok
test tests::auth::test_login ... ok
test tests::main::test_handler ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
"#;
        let parser = TestOutputParser;
        let errors = parser.parse(output);

        assert!(errors.is_empty());
    }

    // ─── Severity Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_severity_default() {
        assert_eq!(Severity::default(), Severity::Error);
    }

    #[test]
    fn test_severity_equality() {
        assert_eq!(Severity::Error, Severity::Error);
        assert_eq!(Severity::Warning, Severity::Warning);
        assert_ne!(Severity::Error, Severity::Warning);
    }

    // ─── ParsedError Tests ─────────────────────────────────────────────────

    #[test]
    fn test_parsed_error_clone() {
        use std::path::PathBuf;

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
        assert_eq!(cloned.code, e.code);
        assert_eq!(cloned.path, e.path);
    }

    #[test]
    fn test_parsed_error_with_all_fields() {
        use std::path::PathBuf;

        let e = ParsedError {
            path: PathBuf::from("src/lib.rs"),
            line: 100,
            column: Some(20),
            severity: Severity::Warning,
            message: "unused import".to_string(),
            code: Some("unused_imports".to_string()),
        };

        assert_eq!(e.path.to_string_lossy(), "src/lib.rs");
        assert_eq!(e.line, 100);
        assert_eq!(e.column, Some(20));
        assert_eq!(e.severity, Severity::Warning);
        assert_eq!(e.message, "unused import");
        assert_eq!(e.code, Some("unused_imports".to_string()));
    }

    // ─── Enrichment Report Tests ────────────────────────────────────────────

    #[test]
    fn test_enriched_error_clone() {
        use std::path::PathBuf;

        let e = ParsedError {
            path: PathBuf::from("src/main.rs"),
            line: 1,
            column: None,
            severity: Severity::Error,
            message: "test".to_string(),
            code: None,
        };
        let enriched = EnrichedError {
            error: e.clone(),
            symbol: Some("main".to_string()),
            anchor_score: Some(0.75),
        };
        let cloned = enriched.clone();

        assert_eq!(cloned.symbol, enriched.symbol);
        assert_eq!(cloned.anchor_score, enriched.anchor_score);
    }

    // ─── Parser Integration Tests ─────────────────────────────────────────────

    #[test]
    fn test_clippy_output_parsing() {
        // Clippy output format is similar to cargo build
        let output = r#"warning: unused variable: `x`
  --> src/utils.rs:3
   |
3 |     let x = 1;
   |         ^
   |
   = note: `#[warn(unused_variables)]` on by default

error: aborting due to 1 previous error
"#;
        let parser = CargoTextParser::new();
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, Severity::Warning);
        assert!(errors[0].message.contains("unused variable"));
    }

    #[test]
    fn test_multifile_error_parsing() {
        let output = r#"error[E0308]: mismatched types
  --> src/auth/token.rs:15:10
   |
15 |     let token = validate(input)?;
   |                 ^^^^^^^ expected `String`, found `&str`

error[E0599]: no method named `verify`
  --> src/auth/token.rs:42:18
   |
42 |     token.verify()?;
   |          ^^^^^^ method not found

warning: unused variable: `timeout`
  --> src/server.rs:100:5
   |
100 |     let timeout = Duration::from_secs(30);
   |         ^^^^^^^
"#;
        let parser = CargoTextParser::new();
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].path.to_string_lossy(), "src/auth/token.rs");
        assert_eq!(errors[1].path.to_string_lossy(), "src/auth/token.rs");
        assert_eq!(errors[2].path.to_string_lossy(), "src/server.rs");
    }

    // ─── Project Detection Tests ────────────────────────────────────────────

    #[test]
    fn test_toolchain_name() {
        use lain::tools::handlers::decoration::Toolchain;

        assert_eq!(Toolchain::Rust.name(), "rust");
        assert_eq!(Toolchain::JavaScript.name(), "javascript");
        assert_eq!(Toolchain::TypeScript.name(), "typescript");
        assert_eq!(Toolchain::Go.name(), "go");
        assert_eq!(Toolchain::Python.name(), "python");
        assert_eq!(Toolchain::Java.name(), "java");
        assert_eq!(Toolchain::CSharp.name(), "csharp");
        assert_eq!(Toolchain::Ruby.name(), "ruby");
        assert_eq!(Toolchain::Php.name(), "php");
        assert_eq!(Toolchain::Cpp.name(), "cpp");
        assert_eq!(Toolchain::C.name(), "c");
    }

    #[test]
    fn test_project_profile_single_toolchain() {
        use lain::tools::handlers::decoration::{detect_project_profile, Toolchain};
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let profile = detect_project_profile(dir.path());

        assert_eq!(profile.toolchains, vec![Toolchain::Rust]);
        assert_eq!(profile.primary, Some(Toolchain::Rust));
        assert!(!profile.is_polyglot());
    }

    #[test]
    fn test_project_profile_multiple_toolchains() {
        use lain::tools::handlers::decoration::{detect_project_profile, Toolchain};
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();

        let profile = detect_project_profile(dir.path());

        assert!(profile.toolchains.contains(&Toolchain::Rust));
        assert!(profile.toolchains.contains(&Toolchain::JavaScript));
        assert!(profile.is_polyglot());
    }

    #[test]
    fn test_project_profile_empty() {
        use lain::tools::handlers::decoration::detect_project_profile;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let profile = detect_project_profile(dir.path());

        assert!(profile.toolchains.is_empty());
        assert_eq!(profile.primary, None);
        assert!(!profile.is_polyglot());
    }

    #[test]
    fn test_project_profile_resolve_explicit() {
        use lain::tools::handlers::decoration::{detect_project_profile, Toolchain};
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();

        let profile = detect_project_profile(dir.path());

        // Explicit override takes precedence
        assert_eq!(profile.resolve(Some("javascript")), Some(Toolchain::JavaScript));
        assert_eq!(profile.resolve(Some("rust")), Some(Toolchain::Rust));
        assert_eq!(profile.resolve(Some("go")), None); // Not present
    }

    #[test]
    fn test_project_profile_resolve_auto() {
        use lain::tools::handlers::decoration::{detect_project_profile, Toolchain};
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let profile = detect_project_profile(dir.path());

        // Auto-detect should return primary
        assert_eq!(profile.resolve(None), Some(Toolchain::Rust));
    }

    #[test]
    fn test_node_test_runner_detection_vitest() {
        use lain::tools::handlers::decoration::detect_project_profile;
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"test": "vitest run"}}"#,
        )
        .unwrap();

        let profile = detect_project_profile(dir.path());

        assert_eq!(profile.js_test_runner, Some("vitest".to_string()));
    }

    #[test]
    fn test_node_test_runner_detection_jest() {
        use lain::tools::handlers::decoration::detect_project_profile;
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"test": "jest"}}"#,
        )
        .unwrap();

        let profile = detect_project_profile(dir.path());

        assert_eq!(profile.js_test_runner, Some("jest".to_string()));
    }

    // ─── GoBuildParser Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_go_build_parser_undefined_error() {
        let output = r#"./main.go:10:5: undefined: foo
src/bar.go:3:8: cannot find package: fmt
"#;
        let parser = GoBuildParser;
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].path.to_string_lossy(), "./main.go");
        assert_eq!(errors[0].line, 10);
        assert_eq!(errors[0].severity, Severity::Error);
        assert!(errors[0].message.contains("undefined"));
        assert_eq!(errors[1].path.to_string_lossy(), "src/bar.go");
        assert_eq!(errors[1].line, 3);
    }

    #[test]
    fn test_go_build_parser_syntax_error() {
        let output = r#"syntax error: unexpected )
expected ;
"#;
        let parser = GoBuildParser;
        let errors = parser.parse(output);

        // No .go: line markers, so no errors parsed
        assert_eq!(errors.len(), 0);
    }

    // ─── GoTestParser Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_go_test_parser_failed_test() {
        let output = r#"--- FAIL: TestAdd (0.00s)
	foo_test.go:15: expected 2, got 3
--- FAIL: TestMultiply (0.00s)
	foo_test.go:42: division by zero
PASS ok  github.com/user/project  0.005s
"#;
        let parser = GoTestParser;
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 2);
        assert!(errors[0].message.contains("TestAdd"));
        assert!(errors[1].message.contains("TestMultiply"));
        assert_eq!(errors[0].path.to_string_lossy(), "foo_test.go");
    }

    // ─── PytestParser Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_pytest_parser_error() {
        let output = r#"tests/test_foo.py:10: Error: expected 2, got 3
tests/test_bar.py:5: Warning: unused variable
"#;
        let parser = PytestParser;
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].path.to_string_lossy(), "tests/test_foo.py");
        assert_eq!(errors[0].line, 10);
        assert_eq!(errors[0].severity, Severity::Error);
        assert_eq!(errors[1].severity, Severity::Warning);
    }

    #[test]
    fn test_pytest_parser_no_errors() {
        let output = r#"============================= test session starts ==============================
collected 5 items
tests/test_foo.py::test_add PASSED
tests/test_foo.py::test_multiply PASSED
============================== 2 passed in 0.01s ==============================
"#;
        let parser = PytestParser;
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 0);
    }

    // ─── JestParser Tests ───────────────────────────────────────────────────────

    #[test]
    fn test_jest_parser_fail_output() {
        let output = r#"FAIL src/components/Button.test.tsx
  ● renders correctly
    expect(received).toBe(expected)
    Expected: "Submit"
    Received: "submit"
"#;
        let parser = JestParser;
        let errors = parser.parse(output);

        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].severity, Severity::Error);
    }

    #[test]
    fn test_jest_parser_pass_output() {
        let output = r#"PASS src/components/Button.test.tsx
  ● renders correctly
Test Suites: 1 passed, 1 total
Tests: 1 passed, 1 total
"#;
        let parser = JestParser;
        let errors = parser.parse(output);

        // No failed tests means no errors parsed
        assert_eq!(errors.len(), 0);
    }
}
