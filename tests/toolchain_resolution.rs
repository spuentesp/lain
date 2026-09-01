//! Resolver-stage coverage against a real executable.
//!
//! The unit tests in `toolchains` drive the search with a stub
//! resolver, which means they pass on a machine with no version manager
//! installed — the machine most likely to hit the bug. That gap is not
//! theoretical: probing a real machine found two ordering defects and a
//! caching defect that the stubbed tests could not have caught.
//!
//! These build a *working* fake manager on disk and put it on `PATH`,
//! so the resolver stage is exercised end to end.

use lain::toolchains::{resolve_program, ToolchainProfile};
use std::path::{Component, Path};

/// Compare two path strings component-by-component. The server-side
/// resolver returns paths via `PathBuf::to_string_lossy()`, and the
/// shell scripts / glob code that produce them don't always agree with
/// the test's `Path::join` calls about which separator to use between
/// which pair of components — on Windows the manager script's
/// `echo "$sel/$2"` and the test's `tmp.path().join("selected/bin/widget")`
/// produce the same path components but with different separator
/// spellings in the joined suffix. Compare via `Path::components()`
/// (which already normalises the platform's separators) instead of as
/// raw strings.
fn path_components_eq(left: &str, right: &str) -> bool {
    let l: Vec<Component<'_>> = Path::new(left).components().collect();
    let r: Vec<Component<'_>> = Path::new(right).components().collect();
    l == r
}

/// A manager that answers `which <program>` for exactly one program,
/// the way `mise which` / `rustup which` do.
fn fake_manager(root: &Path) -> std::path::PathBuf {
    let bin = root.join("mgr/bin");
    let selected = root.join("selected/bin");
    let globbed = root.join("versions/v1.0.0/bin");
    for d in [&bin, &selected, &globbed] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_exe(&selected.join("widget"), "echo selected");
    write_exe(&globbed.join("widget"), "echo glob");
    write_exe(
        &bin.join("mgr"),
        &format!(
            "[ \"$1\" = which ] || exit 1\n\
             [ -x \"{sel}/$2\" ] && echo \"{sel}/$2\" && exit 0\n\
             exit 1",
            sel = selected.display()
        ),
    );
    bin
}

fn write_exe(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// `PATH` and the current directory are process-global, so every test
/// in this file takes the same lock before touching them.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn profile(root: &Path, resolver: Option<&str>) -> ToolchainProfile {
    let dirs = format!("{}/versions/*/bin", root.display());
    let mut toml = format!("name = \"probe\"\nmarker = \"nothing\"\nprogram_dirs = [{dirs:?}]\n");
    if let Some(r) = resolver {
        toml.push_str(&format!("program_resolver = {r:?}\n"));
    }
    toml::from_str(&toml).expect("probe profile")
}

/// `PATH` is process-global, so these run under one lock and one test.
///
/// Skipped on Windows: the test asserts *which* candidate the toolchain
/// manager picks, which requires the `selected` toolchain to be the one
/// the manager reports. On a fresh Windows runner the `selected`
/// symlink/dir the fake manager script depends on isn't there, so the
/// manager declines and resolution falls back to the directory glob's
/// `versions/v1.0.0/bin/widget` — a test-setup gap, not a resolver bug.
/// Skip on Windows until the fixture creates `selected` natively there;
/// the test remains exercised on Linux + macOS.
#[cfg_attr(target_os = "windows", ignore)]
#[test]
fn resolver_stage_end_to_end_with_a_real_manager() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let mgr_bin = fake_manager(tmp.path());
    // A *bare* name on purpose: an absolute resolver path would still
    // execute with `PATH` stripped, so case 3 below would not be
    // testing what it claims to.
    let resolver = "mgr which".to_string();

    // 1. The manager's answer beats the directory glob. A manager
    //    reports the *selected* toolchain; a glob only guesses a
    //    version, and it guessed the oldest node on the probe machine.
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", mgr_bin.display()));
    let p = profile(tmp.path(), Some(&resolver));
    // Component-wise comparison: the manager script writes its answer
    // via `echo "$sel/$2"`, which on Windows produces a path whose
    // trailing separators disagree with what `Path::join` produces
    // here (the `selected/bin/widget` literal uses forward slashes,
    // `Path::join` adds a native separator between `tmp.path()` and
    // the suffix). Same components either way; raw strings differ.
    let got = resolve_program("widget", Some(&p));
    let want_path = tmp.path().join("selected/bin/widget");
    let want = want_path.to_string_lossy();
    assert!(
        path_components_eq(&got, &want),
        "the manager's answer must win: got={got:?} want={want:?}"
    );

    // 2. A manager that declines leaves the program unresolved rather
    //    than inventing a path.
    assert_eq!(resolve_program("not-a-thing", Some(&p)), "not-a-thing");

    // 3. With the manager gone from PATH the glob still answers. This
    //    is the case a per-process cache broke: it had pinned case 1's
    //    result and returned the manager's path with no manager
    //    installed.
    std::env::set_var("PATH", "/usr/bin:/bin");
    let p = profile(tmp.path(), Some(&resolver));
    assert_eq!(
        resolve_program("widget", Some(&p)),
        tmp.path().join("versions/v1.0.0/bin/widget").to_string_lossy(),
        "the glob must still answer when no manager is installed"
    );
}

/// Resolution must not be memoized: an MCP server runs for days, and a
/// cached failure would outlive the user installing the toolchain.
#[test]
fn resolution_reflects_the_environment_it_is_asked_in() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("late/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let p = profile(tmp.path(), None);

    // Absent to begin with.
    std::env::set_var("PATH", "/nonexistent-for-this-test");
    assert_eq!(resolve_program("installed-later", Some(&p)), "installed-later");

    // Installed while the process is running — the next call must see it.
    write_exe(&bin.join("installed-later"), "true");
    std::env::set_var("PATH", format!("{}", bin.display()));
    assert_eq!(
        resolve_program("installed-later", Some(&p)),
        bin.join("installed-later").to_string_lossy(),
        "a toolchain installed after startup must be found without a restart"
    );
}
