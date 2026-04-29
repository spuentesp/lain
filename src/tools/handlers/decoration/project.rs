//! Project profile detection for multi-language workspaces

/// Detected toolchain type (Top 10 TIOBE Index + others)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toolchain {
    // Top 10 TIOBE
    Python,    // pyproject.toml, setup.py, requirements.txt
    C,         // Makefile, CMakeLists.txt
    Cpp,       // CMakeLists.txt, Makefile
    Java,      // pom.xml, build.gradle
    CSharp,    // .csproj, .sln
    JavaScript, // package.json (Node/JS)
    Go,        // go.mod
    Rust,      // Cargo.toml
    Ruby,      // Gemfile, Rakefile
    Php,       // composer.json
    // Additional
    TypeScript, // tsconfig.json, package.json with typescript
    Swift,     // Package.swift
    Kotlin,    // build.gradle.kts
    Scala,     // build.sbt
    // Emerging / Niche
    Zig,       // build.zig, zig.mod
    R,         // DESCRIPTION, NAMESPACE (R packages)
    Perl,      // Makefile.PL, cpanfile, *.pm
    Matlab,    // *.prj, MATLAB project files
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
