//! Error decoration for build/test/lint commands
//!
//! Composable decoration pattern: Command output -> Parser -> Enricher -> Report

use crate::graph::GraphDatabase;
use crate::overlay::VolatileOverlay;
use crate::schema::{EdgeType, NodeType};
use serde::Deserialize;
use std::path::PathBuf;

/// Severity level for parsed diagnostics
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
    Help,
}

impl Default for Severity {
    fn default() -> Self {
        Severity::Error
    }
}

/// A parsed error/warning from command output
#[derive(Debug, Clone)]
pub struct ParsedError {
    pub path: PathBuf,
    pub line: u32,
    pub column: Option<u32>,
    pub severity: Severity,
    pub message: String,
    pub code: Option<String>,
}

/// Per-error enriched data (cheap to compute)
#[derive(Debug, Clone)]
pub struct EnrichedError {
    pub error: ParsedError,
    pub symbol: Option<String>,
    pub anchor_score: Option<f32>,
}

/// Aggregate summary of all failures
#[derive(Debug, Clone)]
pub struct FailureSummary {
    pub affected_files: Vec<PathBuf>,
    pub affected_symbols: Vec<String>,
    pub combined_blast_radius: Vec<String>,
    pub co_change_partners: Vec<(String, usize)>,
    pub architectural_note: Option<String>,
}

/// Full enrichment report
#[derive(Debug, Clone)]
pub struct EnrichedReport {
    pub errors: Vec<EnrichedError>,
    pub summary: FailureSummary,
}

// ─── Project Detection ─────────────────────────────────────────────────────────

/// Detected toolchain type (Top 10 TIOBE Index + others)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toolchain {
    // Top 10 TIOBE
    Python,   // pyproject.toml, setup.py, requirements.txt
    C,        // Makefile, CMakeLists.txt
    Cpp,      // CMakeLists.txt, Makefile
    Java,     // pom.xml, build.gradle
    CSharp,   // .csproj, .sln
    JavaScript, // package.json (Node/JS)
    Go,       // go.mod
    Rust,     // Cargo.toml
    Ruby,     // Gemfile, Rakefile
    Php,      // composer.json
    // Additional
    TypeScript, // tsconfig.json, package.json with typescript
    Swift,    // Package.swift
    Kotlin,   // build.gradle.kts
    Scala,    // build.sbt
    // Emerging / Niche
    Zig,      // build.zig, zig.mod
    R,        // DESCRIPTION, NAMESPACE (R packages)
    Perl,     // Makefile.PL, cpanfile, *.pm
    Matlab,   // *.prj, MATLAB project files
}

impl Toolchain {
    pub fn name(&self) -> &'static str {
        match self {
            Toolchain::Python => "python",
            Toolchain::C => "c",
            Toolchain::Cpp => "cpp",
            Toolchain::Java => "java",
            Toolchain::CSharp => "csharp",
            Toolchain::JavaScript => "javascript",
            Toolchain::Go => "go",
            Toolchain::Rust => "rust",
            Toolchain::Ruby => "ruby",
            Toolchain::Php => "php",
            Toolchain::TypeScript => "typescript",
            Toolchain::Swift => "swift",
            Toolchain::Kotlin => "kotlin",
            Toolchain::Scala => "scala",
            Toolchain::Zig => "zig",
            Toolchain::R => "r",
            Toolchain::Perl => "perl",
            Toolchain::Matlab => "matlab",
        }
    }

    pub fn all_names() -> &'static [&'static str] {
        &[
            "python", "c", "cpp", "java", "csharp", "javascript",
            "go", "rust", "ruby", "php", "typescript", "swift", "kotlin", "scala",
            "zig", "r", "perl", "matlab",
        ]
    }
}

/// Project profile containing detected toolchains and metadata
#[derive(Debug, Clone)]
pub struct ProjectProfile {
    pub toolchains: Vec<Toolchain>,
    pub primary: Option<Toolchain>,
    /// For Node/JS: detected test runner (jest, vitest, mocha)
    pub js_test_runner: Option<String>,
    /// For Java: detected build tool (maven, gradle)
    pub java_build_tool: Option<String>,
}

impl ProjectProfile {
    /// Get the toolchain to use, respecting explicit override or auto-detection
    pub fn resolve(&self, explicit: Option<&str>) -> Option<Toolchain> {
        if let Some(name) = explicit {
            self.toolchains.iter().find(|t| t.name() == name).copied()
        } else {
            self.primary
        }
    }

    /// Check if multiple toolchains are present (ambiguity warning)
    pub fn is_polyglot(&self) -> bool {
        self.toolchains.len() > 1
    }

    /// List available toolchains as strings
    pub fn available_toolchains(&self) -> Vec<String> {
        self.toolchains.iter().map(|t| t.name().to_string()).collect()
    }
}

/// Detect which toolchains exist in the given directory
pub fn detect_project_profile(cwd: &std::path::Path) -> ProjectProfile {
    // Cache file existence checks to avoid redundant syscalls
    let has_cargo = cwd.join("Cargo.toml").exists();
    let has_pkg_json = cwd.join("package.json").exists();
    let has_tsconfig = cwd.join("tsconfig.json").exists();
    let has_go_mod = cwd.join("go.mod").exists();
    let has_pyproject = cwd.join("pyproject.toml").exists();
    let has_setup_py = cwd.join("setup.py").exists();
    let has_requirements = cwd.join("requirements.txt").exists();
    let has_pom = cwd.join("pom.xml").exists();
    let has_build_gradle = cwd.join("build.gradle").exists();
    let has_build_gradle_kts = cwd.join("build.gradle.kts").exists();
    let has_gemfile = cwd.join("Gemfile").exists();
    let has_rakefile = cwd.join("Rakefile").exists();
    let has_composer = cwd.join("composer.json").exists();
    let has_cmake = cwd.join("CMakeLists.txt").exists();
    let has_makefile = cwd.join("Makefile").exists();
    let has_package_swift = cwd.join("Package.swift").exists();
    let has_zig_mod = cwd.join("zig.mod").exists();
    let has_zig_build = cwd.join("build.zig").exists();
    let has_description = cwd.join("DESCRIPTION").exists();
    let has_namespace = cwd.join("NAMESPACE").exists();
    let has_makefile_pl = cwd.join("Makefile.PL").exists();
    let has_cpanfile = cwd.join("cpanfile").exists();
    let has_meta_json = cwd.join("META.json").exists();
    let has_matlab_prj = cwd.join("MATLAB.prj").exists();

    let mut toolchains = Vec::new();

    // Rust
    if has_cargo {
        toolchains.push(Toolchain::Rust);
    }

    // Node.js / JavaScript / TypeScript
    if has_pkg_json {
        if has_tsconfig {
            toolchains.push(Toolchain::TypeScript);
        } else {
            toolchains.push(Toolchain::JavaScript);
        }
    }

    // Go
    if has_go_mod {
        toolchains.push(Toolchain::Go);
    }

    // Python
    if has_pyproject || has_setup_py || has_requirements {
        toolchains.push(Toolchain::Python);
    }

    // Java (Maven or Gradle)
    if has_pom || has_build_gradle || has_build_gradle_kts {
        toolchains.push(Toolchain::Java);
    }

    // C# (.csproj or .sln) - need to scan directory for these patterns
    if let Ok(entries) = std::fs::read_dir(cwd) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".csproj") || name.ends_with(".sln") {
                    toolchains.push(Toolchain::CSharp);
                    break;
                }
            }
        }
    }

    // Ruby (Gemfile or Rakefile)
    if has_gemfile || has_rakefile {
        toolchains.push(Toolchain::Ruby);
    }

    // PHP (composer.json)
    if has_composer {
        toolchains.push(Toolchain::Php);
    }

    // C/C++ (Makefile or CMakeLists.txt)
    if has_cmake {
        toolchains.push(Toolchain::Cpp);
    } else if has_makefile {
        // Could be C or C++, default to C
        toolchains.push(Toolchain::C);
    }

    // Swift (Package.swift)
    if has_package_swift {
        toolchains.push(Toolchain::Swift);
    }

    // Kotlin/Scala (build.gradle.kts without pom.xml suggests Kotlin)
    if has_build_gradle_kts && !has_pom {
        // Could be Kotlin or Scala, default to Kotlin
        toolchains.push(Toolchain::Kotlin);
    }

    // Zig (build.zig or zig.mod)
    if has_zig_build || has_zig_mod {
        toolchains.push(Toolchain::Zig);
    }

    // R (DESCRIPTION or NAMESPACE in package directories)
    if has_description || has_namespace {
        toolchains.push(Toolchain::R);
    }

    // Perl (Makefile.PL, cpanfile, or META.json)
    if has_makefile_pl || has_cpanfile || has_meta_json {
        toolchains.push(Toolchain::Perl);
    }

    // MATLAB (MATLAB.prj or scan for .prj files)
    if has_matlab_prj {
        toolchains.push(Toolchain::Matlab);
    } else if let Ok(entries) = std::fs::read_dir(cwd) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".prj") {
                    toolchains.push(Toolchain::Matlab);
                    break;
                }
            }
        }
    }

    // Heuristic: primary = most likely to be the main project
    let primary = if toolchains.len() == 1 {
        toolchains.first().copied()
    } else if toolchains.len() > 1 {
        TOOLCHAIN_PRIORITY
            .iter()
            .find(|&&t| toolchains.contains(&t))
            .copied()
    } else {
        None
    };

    // Detect JS test runner from package.json
    let js_test_runner = if has_pkg_json {
        read_package_json_test_runner(cwd)
    } else {
        None
    };

    // Detect Java build tool
    let java_build_tool = if has_pom {
        Some("maven".to_string())
    } else if has_build_gradle || has_build_gradle_kts {
        Some("gradle".to_string())
    } else {
        None
    };

    ProjectProfile {
        toolchains,
        primary,
        js_test_runner,
        java_build_tool,
    }
}

/// Priority order for heuristic: Rust > Go > Python > Node > Java > C# > Ruby > PHP > C/C++
const TOOLCHAIN_PRIORITY: [Toolchain; 18] = [
    Toolchain::Rust,
    Toolchain::Go,
    Toolchain::Python,
    Toolchain::JavaScript,
    Toolchain::TypeScript,
    Toolchain::Java,
    Toolchain::CSharp,
    Toolchain::Ruby,
    Toolchain::Php,
    Toolchain::Cpp,
    Toolchain::C,
    Toolchain::Zig,
    Toolchain::Swift,
    Toolchain::Kotlin,
    Toolchain::Scala,
    Toolchain::R,
    Toolchain::Perl,
    Toolchain::Matlab,
];

fn read_package_json_test_runner(cwd: &std::path::Path) -> Option<String> {
    use std::fs;

    let pkg_path = cwd.join("package.json");
    let content = fs::read_to_string(&pkg_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;

    let test_script = value.get("scripts")?.get("test")?.as_str()?;

    if test_script.contains("vitest") {
        Some("vitest".to_string())
    } else if test_script.contains("jest") {
        Some("jest".to_string())
    } else if test_script.contains("mocha") {
        Some("mocha".to_string())
    } else {
        Some("unknown".to_string())
    }
}

// ─── Parsers ─────────────────────────────────────────────────────────────────

/// Trait for parsing structured output into errors
pub trait ErrorParser: Send + Sync {
    fn parse(&self, output: &str) -> Vec<ParsedError>;
}

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
                } else {
                    // "-->" appears somewhere after position 0
                    &trimmed[trimmed.find("-->").unwrap() + 3..]
                };

                // Parse path:line:col - be careful with Windows paths that have colons
                let parts: Vec<&str> = path_part.rsplitn(3, ':').collect();
                if parts.len() >= 2 {
                    // parts are [col, line, path] in reverse (if 3 parts)
                    // or [line, path] in reverse (if 2 parts)
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

                let title = assertion.get("title").and_then(|t| t.as_str()).unwrap_or("unknown");
                let line = assertion.get("lineNumber").and_then(|l| l.as_u64()).unwrap_or(1) as u32;
                let path = assertion.get("fullName").and_then(|_f| {
                    // Jest gives full test name, try to extract file path
                    // Format: "Test Suites > test-name > test-name > it('should do thing')"
                    // The file path is usually in ancestorResults
                    if let Some(ancestors) = assertion.get("ancestorTitles").and_then(|a| a.as_array()) {
                        for ancestor in ancestors {
                            if let Some(a) = ancestor.as_str() {
                                if a.ends_with(".test.js") || a.ends_with(".spec.js") || a.ends_with(".js") {
                                    return Some(a.to_string());
                                }
                                if a.contains(".test.") || a.contains(".spec.") {
                                    return Some(a.to_string());
                                }
                            }
                        }
                    }
                    None
                }).unwrap_or_else(|| "unknown".to_string());

                let message = assertion.get("failureMessages")
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

// ─── Enricher ─────────────────────────────────────────────────────────────────

/// Trait for enriching parsed errors with graph context
pub trait ErrorEnricher: Send + Sync {
    fn enrich(
        &self,
        errors: &[ParsedError],
        graph: &GraphDatabase,
        overlay: &VolatileOverlay,
    ) -> EnrichedReport;
}

/// Enricher that uses the knowledge graph
pub struct GraphEnricher;

impl ErrorEnricher for GraphEnricher {
    fn enrich(
        &self,
        errors: &[ParsedError],
        graph: &GraphDatabase,
        overlay: &VolatileOverlay,
    ) -> EnrichedReport {
        let mut enriched_errors = Vec::new();
        let mut affected_files: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut affected_symbols: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Phase 1: Per-error enrichment (cheap)
        for error in errors {
            let (symbol, anchor_score) = resolve_symbol(error, graph, overlay);

            if let Some(ref s) = symbol {
                affected_symbols.insert(s.clone());
            }
            affected_files.insert(error.path.clone());

            enriched_errors.push(EnrichedError {
                error: error.clone(),
                symbol,
                anchor_score,
            });
        }

        // Phase 2: Summary enrichment (expensive, batch)
        let combined_blast_radius = compute_combined_blast_radius(&affected_symbols, graph);
        let co_change_partners = compute_co_change_partners(&affected_files, graph);
        let architectural_note = generate_architectural_note(&enriched_errors, &affected_symbols);

        let summary = FailureSummary {
            affected_files: affected_files.into_iter().collect(),
            affected_symbols: affected_symbols.into_iter().collect(),
            combined_blast_radius,
            co_change_partners,
            architectural_note,
        };

        EnrichedReport {
            errors: enriched_errors,
            summary,
        }
    }
}

/// Resolve a symbol name from file:line using the graph
fn resolve_symbol(
    error: &ParsedError,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
) -> (Option<String>, Option<f32>) {
    let path_str = error.path.to_string_lossy();

    // Try overlay first (uncommitted changes), then persistent graph
    let node = overlay
        .find_nodes_by_path(&path_str)
        .first()
        .cloned()
        .or_else(|| graph.find_node_by_path(&path_str));

    if let Some(node) = node {
        (Some(node.name.clone()), node.anchor_score)
    } else {
        (None, None)
    }
}

/// Compute combined blast radius for all affected symbols
fn compute_combined_blast_radius(
    symbols: &std::collections::HashSet<String>,
    graph: &GraphDatabase,
) -> Vec<String> {
    let mut all_reachable: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Build a name -> node mapping once (O(m) where m = total function nodes)
    let Ok(all_func_nodes) = graph.get_nodes_by_type(NodeType::Function) else {
        return Vec::new();
    };

    // Create a lookup map: symbol name -> node IDs
    // This reduces O(n*m) to O(n+m)
    let mut name_to_nodes: std::collections::HashMap<&str, Vec<_>> =
        std::collections::HashMap::new();
    for node in &all_func_nodes {
        name_to_nodes
            .entry(node.name.as_str())
            .or_default()
            .push(&node.id);
    }

    // For each symbol, look up nodes and traverse edges
    for symbol_name in symbols {
        if let Some(node_ids) = name_to_nodes.get(symbol_name.as_str()) {
            for node_id in node_ids {
                if let Ok(edges) = graph.get_edges_from(node_id) {
                    for edge in edges.iter().filter(|e| e.edge_type == EdgeType::Calls) {
                        if let Some(target) = graph.get_node(&edge.target_id).ok().flatten() {
                            all_reachable.insert(target.name.clone());
                        }
                    }
                }
            }
        }
    }

    let mut result: Vec<String> = all_reachable.into_iter().take(20).collect();
    result.sort();
    result
}

/// Find files that co-change with the affected files
fn compute_co_change_partners(
    files: &std::collections::HashSet<PathBuf>,
    graph: &GraphDatabase,
) -> Vec<(String, usize)> {
    let mut partners: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for file in files {
        if let Ok(pairs) = graph.get_co_change_partners(&file.to_string_lossy()) {
            for (name, count) in pairs {
                *partners.entry(name).or_insert(0) += count;
            }
        }
    }

    let mut result: Vec<(String, usize)> = partners.into_iter().collect();
    result.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    result.truncate(5);
    result
}

/// Generate an architectural note if there's a pattern in the failures
fn generate_architectural_note(
    errors: &[EnrichedError],
    symbols: &std::collections::HashSet<String>,
) -> Option<String> {
    if errors.len() >= 3 && !symbols.is_empty() {
        let mut symbol_error_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for e in errors {
            if let Some(ref s) = e.symbol {
                *symbol_error_counts.entry(s).or_insert(0) += 1;
            }
        }

        if let Some((most_repeated, count)) =
            symbol_error_counts.iter().max_by_key(|(_, c)| *c)
        {
            if *count >= 2 && errors.len() > 1 {
                return Some(format!(
                    "{} of {} failures are in `{}` — likely a single root cause",
                    count,
                    errors.len(),
                    most_repeated
                ));
            }
        }
    }
    None
}

// ─── Decorator ────────────────────────────────────────────────────────────────

/// Decorate command output by parsing errors and enriching with graph context
pub fn decorate_output(
    output: &str,
    parser: &(impl ErrorParser + ?Sized),
    enricher: &impl ErrorEnricher,
    graph: &GraphDatabase,
    overlay: &VolatileOverlay,
) -> String {
    let errors = parser.parse(output);

    // If no structured errors found, return original output
    if errors.is_empty() {
        return output.to_string();
    }

    // Filter to Error and Warning only (skip Note, Help for enrichment)
    let actionable: Vec<ParsedError> = errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error || e.severity == Severity::Warning)
        .collect();

    if actionable.is_empty() {
        return output.to_string();
    }

    let report = enricher.enrich(&actionable, graph, overlay);
    render_enriched_report(&report)
}

/// Render the enriched report to a string
fn render_enriched_report(report: &EnrichedReport) -> String {
    let error_count = report
        .errors
        .iter()
        .filter(|e| e.error.severity == Severity::Error)
        .count();
    let warning_count = report
        .errors
        .iter()
        .filter(|e| e.error.severity == Severity::Warning)
        .count();
    let file_count = report.summary.affected_files.len();

    let mut out = String::new();
    out.push_str("\n## Enrichment Report\n");
    out.push_str(&format!(
        "{} error(s), {} warning(s) in {} file(s)\n\n",
        error_count, warning_count, file_count
    ));

    // Render each error with its enrichment
    out.push_str("### Errors\n\n");
    for enriched in &report.errors {
        let marker = match enriched.error.severity {
            Severity::Error => "❌",
            Severity::Warning => "⚠️",
            Severity::Note => "  ",
            Severity::Help => "  ",
        };

        out.push_str(&format!(
            "{}{}:{} [{}] {}\n",
            marker,
            enriched.error.path.display(),
            enriched.error.line,
            enriched.error.code.as_deref().unwrap_or(""),
            enriched.error.message
        ));

        if let Some(ref symbol) = enriched.symbol {
            out.push_str("  → in `");
            out.push_str(symbol);
            if let Some(score) = enriched.anchor_score {
                out.push_str(&format!("` (anchor: {:.3})", score));
            } else {
                out.push('`');
            }
            out.push('\n');
        }
    }

    // Render summary
    if let Some(ref note) = report.summary.architectural_note {
        out.push_str("\n### Architectural Context\n\n");
        out.push_str(note);
        out.push_str("\n\n");
    }

    if !report.summary.affected_symbols.is_empty() {
        let symbols: Vec<String> = report
            .summary
            .affected_symbols
            .iter()
            .take(5)
            .map(|s| format!("`{}`", s))
            .collect();
        out.push_str(&format!(
            "**Affected symbols ({}):** {}\n",
            report.summary.affected_symbols.len(),
            symbols.join(", ")
        ));
    }

    if !report.summary.combined_blast_radius.is_empty() {
        let blast: Vec<String> = report
            .summary
            .combined_blast_radius
            .iter()
            .take(5)
            .map(|s| format!("`{}`", s))
            .collect();
        out.push_str(&format!(
            "**Transitive blast radius ({} nodes):** {}\n",
            report.summary.combined_blast_radius.len(),
            blast.join(", ")
        ));
    }

    if !report.summary.co_change_partners.is_empty() {
        out.push_str("\n**Frequently co-changes with:**\n");
        for (name, count) in &report.summary.co_change_partners {
            out.push_str(&format!("  - {} ({}x)\n", name, count));
        }
    }

    out
}

// ─── Parser Registry ──────────────────────────────────────────────────────────

// Static instances for stateless parsers (no fields)
static TEXT_PARSER: CargoTextParser = CargoTextParser;

/// Get a parser by its registered ID.
/// IDs are configured in toolchains/*.toml files.
///
/// Built-in IDs:
/// - `cargo-json` — cargo JSON output (rust build)
/// - `cargo-test` — cargo test output (rust test)
/// - `go-build` — go build human-readable output
/// - `go-test` — go test output with test names
/// - `jest` — jest JSON or text output
/// - `pytest` — pytest error output
/// - `text` — generic fallback parser
pub fn get_parser(id: &str) -> Option<&'static dyn ErrorParser> {
    match id {
        "cargo-json" => Some(&CargoJsonParser as &dyn ErrorParser),
        "cargo-test" => Some(&TestOutputParser as &dyn ErrorParser),
        "go-build" => Some(&GoBuildParser as &dyn ErrorParser),
        "go-test" => Some(&GoTestParser as &dyn ErrorParser),
        "jest" => Some(&JestParser as &dyn ErrorParser),
        "pytest" => Some(&PytestParser as &dyn ErrorParser),
        "text" => Some(&TEXT_PARSER as &dyn ErrorParser),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
