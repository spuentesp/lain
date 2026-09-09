//! Guard: lain must never tell an agent to run a command it does not have.
//!
//! `semantic_search`'s unavailable path used to say "Install embeddings
//! with: lain install-embeddings", a subcommand that does not exist —
//! following it returns `error: unrecognized subcommand`. An agent that
//! hits that has to decide whether to trust the next thing lain says,
//! which is a worse failure than the missing feature.
//!
//! This scans string literals in `src/**/*.rs` for `lain <word>` and
//! checks the word against clap's own subcommand list.

use clap::CommandFactory;
use std::collections::HashSet;

/// Words that follow "lain" in ordinary prose rather than naming a
/// subcommand. Each is a sentence about lain, not an instruction.
const PROSE: &[&str] = &[
    "binary",     // "...the lain binary..."
    "expires",    // "lain expires sessions 60 seconds after..."
    "hook",       // "lain hook: 1 granted" — an output prefix, not a command
    "subprocess", // "spawn lain subprocess" — describes the child process, not a command
];

fn subcommands() -> HashSet<String> {
    lain::cli::Args::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect()
}

fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Extract the double-quoted segments of a whole file, each paired with
/// the line it started on.
///
/// Scanning the whole file rather than line by line is load-bearing:
/// Rust string literals routinely span lines via `\` continuations, and
/// a per-line scanner sees a continuation line as having no opening
/// quote and skips it. The first version of this test did exactly that
/// and silently passed when the phantom command was reintroduced.
fn string_literals(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    let mut current: Option<(usize, String)> = None;
    let mut line = 1usize;
    while let Some(c) = chars.next() {
        if c == '\n' {
            line += 1;
        }
        // Skip `//` comments when not inside a literal. Doc comments
        // quote things constantly, and an unpaired quote in prose would
        // otherwise open a fake literal that swallows the lines after
        // it and reports matches that are not in any string.
        if current.is_none() && c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    line += 1;
                    break;
                }
            }
            continue;
        }
        match (c, &mut current) {
            ('\\', Some((_, buf))) => {
                buf.push(c);
                if let Some(next) = chars.next() {
                    if next == '\n' {
                        line += 1;
                    }
                    buf.push(next);
                }
            }
            ('"', Some(_)) => {
                if let Some(pair) = current.take() {
                    out.push(pair);
                }
            }
            ('"', None) => current = Some((line, String::new())),
            (_, Some((_, buf))) => buf.push(c),
            (_, None) => {}
        }
    }
    out
}

#[test]
fn user_facing_strings_never_name_a_command_that_does_not_exist() {
    let known = subcommands();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&root, &mut files);
    assert!(!files.is_empty(), "found no sources to scan under {root:?}");

    let mut bad: Vec<String> = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for (lineno, literal) in string_literals(&text) {
            for (idx, _) in literal.match_indices("lain ") {
                let rest = &literal[idx + "lain ".len()..];
                let word: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                    .collect();
                if word.is_empty() || PROSE.contains(&word.as_str()) {
                    continue;
                }
                if !known.contains(&word) {
                    let mut k: Vec<_> = known.iter().cloned().collect();
                    k.sort();
                    bad.push(format!(
                        "{}:{}: `lain {}` is not a subcommand (have: {:?})",
                        file.display(),
                        lineno,
                        word,
                        k
                    ));
                }
            }
        }
    }

    assert!(
        bad.is_empty(),
        "user-facing strings name commands that do not exist:\n{}",
        bad.join("\n")
    );
}

/// The documented command table must match the binary, both directions.
///
/// It said "After install, `lain` exposes exactly five subcommands" above
/// a table listing nine, while `lain --help` printed ten — and the
/// paragraph below the table announced that `init` had been removed,
/// which it had not. `oneshot` existed and appeared nowhere. Someone
/// reading the README to learn the tool got a count, a table, and a
/// binary that disagreed with each other three ways.
#[test]
fn the_documented_command_table_matches_the_binary() {
    let manual = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/USER_MANUAL.md"),
    )
    .expect("read docs/USER_MANUAL.md");

    // Rows look like: | `lain server` | Start the MCP server ... |
    let mut documented = HashSet::new();
    for line in manual.lines() {
        let t = line.trim();
        if !t.starts_with("| `lain ") {
            continue;
        }
        if let Some(rest) = t.strip_prefix("| `lain ") {
            if let Some((cmd, _)) = rest.split_once('`') {
                let name = cmd.trim();
                if !name.is_empty() && !name.contains(' ') {
                    documented.insert(name.to_string());
                }
            }
        }
    }
    assert!(
        !documented.is_empty(),
        "found no `| \\`lain <cmd>\\` |` rows in the user manual's command table"
    );

    let actual = subcommands();

    let phantom: Vec<_> = documented.difference(&actual).cloned().collect();
    assert!(
        phantom.is_empty(),
        "the user manual documents commands the binary does not have: {phantom:?}"
    );

    // `help` is clap's own and is not worth a table row.
    let mut missing: Vec<_> = actual
        .difference(&documented)
        .filter(|c| *c != "help")
        .cloned()
        .collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "the binary has commands the user manual never mentions: {missing:?}"
    );
}

/// Smoke test for `lain reindex`. Builds a 1-repo fixture, plants a
/// pre-v2 (no-envelope) `federated_graph.bin` to simulate the
/// upgrade-from-0.7.0 path the operator runs after bumping the
/// federation schema, then runs the binary and asserts the backup
/// holds the original bytes and the rebuilt file carries the v2
/// header. Pattern modeled on `tests/doctor_smoke.rs::lain()`.
#[test]
fn lain_reindex_backs_up_graph_and_rebuilds() {
    use lain::federation::graph_backend::{FEDERATION_GRAPH_VERSION, GraphBackend, PetgraphBackend};
    use std::process::Command;

    let project = tempfile::tempdir().expect("tempdir");
    let repo_dir = project.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("src")).expect("mkdir repo/src");
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"reindex-smoke\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    std::fs::write(
        repo_dir.join("src/lib.rs"),
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { alpha() + 1 }\n",
    )
    .expect("write lib.rs");

    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo_dir)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed: {status}");
    };
    git(&["init", "-q", "--initial-branch=main"]);
    git(&["config", "user.email", "smoke@lain"]);
    git(&["config", "user.name", "smoke"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);

    let data_dir = project.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("mkdir data");
    let repos_yaml = project.path().join("repos.yaml");
    std::fs::write(
        &repos_yaml,
        format!(
            "data_dir: {}\nrepos:\n  - id: smoke\n    source:\n      type: workspace_dir\n      path: {}\n",
            data_dir.display(),
            repo_dir.display()
        ),
    )
    .expect("write repos.yaml");

    // Plant a pre-v2 (no LNF2 envelope) federated_graph.bin so the
    // recovery path actually exercises the backup step. The bytes
    // are otherwise opaque — the loader would refuse to read them
    // with `FederationSchemaMismatch`, which is exactly what `lain
    // reindex` exists to recover from.
    let fake_v1 = b"\x07\x00\x00\x00\x00\x00\x00\x00not-a-real-payload-just-bytes";
    std::fs::write(data_dir.join("federated_graph.bin"), fake_v1).expect("plant v1 bin");
    // Also plant a sidecar with garbage bytes. If `lain reindex` failed to
    // clear the sidecar, the next startup would hydrate from this stale
    // sidecar and miss the rebuild — the assertion below would still
    // pass against the canonical file but the underlying state would be
    // wrong. The fix in `src/cli/reindex.rs` removes the sidecar as part
    // of the backup step.
    std::fs::write(
        data_dir.join("federated_graph.bin.payload"),
        b"\xde\xad\xbe\xefsidecar-from-previous-run",
    )
    .expect("plant stale sidecar");

    // `LAIN_TEST_NO_LSP=1` tells `LspMultiplexer::new` to pre-populate
    // its `unavailable` set so `ensure_server` short-circuits without
    // attempting to spawn rust-analyzer / gopls / etc. The scanner's
    // `unwrap_or_default()` then returns an empty ref-set so the test
    // outcome doesn't depend on whether the host has rust-analyzer on
    // PATH.
    let out = Command::new(env!("CARGO_BIN_EXE_lain"))
        .env("LAIN_TEST_NO_LSP", "1")
        .args([
            "reindex",
            "--config",
            repos_yaml.to_str().unwrap(),
            "--verbose",
        ])
        .output()
        .expect("run lain reindex");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "lain reindex failed (status {:?}):\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );

    // The v1 bytes must survive verbatim in the backup.
    let backup = std::fs::read(data_dir.join("federated_graph.bin.bak")).expect("read bak");
    assert_eq!(
        backup, fake_v1,
        "backup must preserve the original v1 payload byte-for-byte"
    );

    // The stale sidecar planted above must not survive the backup step
    // in its planted form. `lain reindex` removes it as part of its
    // pre-flight, and the rebuild re-creates a fresh sidecar from the
    // re-projection; either way the planted garbage cannot be present.
    let planted_sidecar = b"\xde\xad\xbe\xefsidecar-from-previous-run";
    let sidecar_path = data_dir.join("federated_graph.bin.payload");
    if sidecar_path.exists() {
        let sidecar_bytes = std::fs::read(&sidecar_path).expect("read sidecar");
        assert_ne!(
            sidecar_bytes, planted_sidecar,
            "reindex must replace the stale sidecar with a fresh payload; \
             otherwise the next startup hydrates from the previous run's \
             payload instead of from source"
        );
    }

    // The new file must carry the current federation envelope (`LNF2` magic
    // plus the version encoded by `FEDERATION_GRAPH_VERSION`).
    let fresh = std::fs::read(data_dir.join("federated_graph.bin")).expect("read bin");
    assert!(
        fresh.len() >= 8,
        "new graph.bin too short for an envelope: len={}",
        fresh.len()
    );
    assert!(
        fresh.starts_with(b"LNF2"),
        "new graph.bin missing LNF2 magic, got {:02x?}",
        &fresh[..fresh.len().min(8)]
    );
    let version = u32::from_le_bytes([fresh[4], fresh[5], fresh[6], fresh[7]]);
    assert_eq!(
        version,
        FEDERATION_GRAPH_VERSION,
        "new graph.bin must carry version={}, got {version}",
        FEDERATION_GRAPH_VERSION
    );

    // Reopen the rebuilt graph through the real loader and assert the
    // fixture's content actually landed. A header-only assertion can
    // pass against an empty rebuilt graph; this exercises the rebuilt
    // payload end-to-end.
    let backend = PetgraphBackend::new(&data_dir).expect("reopen rebuilt backend");
    let names: Vec<String> = backend
        .list_nodes()
        .expect("list nodes")
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert!(
        backend.node_count() > 0,
        "rebuilt graph has zero nodes; reindex did not actually rebuild. \
         nodes: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "alpha"),
        "expected fixture function `alpha` in rebuilt graph, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "beta"),
        "expected fixture function `beta` in rebuilt graph, got {names:?}"
    );
    let _ = FEDERATION_GRAPH_VERSION;
}

/// The prose around the table must not contradict it — the old copy
/// claimed a subcommand count that matched neither the table nor the
/// binary.
#[test]
fn command_docs_do_not_claim_a_stale_subcommand_count() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for relative in ["README.md", "docs/USER_MANUAL.md"] {
        let text = std::fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("read {relative}: {error}"));
        for spelled in [
            "three subcommands",
            "four subcommands",
            "five subcommands",
            "six subcommands",
            "seven subcommands",
            "eight subcommands",
            "nine subcommands",
            "ten subcommands",
        ] {
            assert!(
                !text.contains(spelled),
                "{relative} hard-codes a subcommand count (\"{spelled}\") that will \
                 go stale the next time a command is added or removed; describe \
                 the table instead of counting it"
            );
        }
    }
}
