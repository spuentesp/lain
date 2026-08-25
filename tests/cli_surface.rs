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
    "binary",  // "...the lain binary..."
    "expires", // "lain expires sessions 60 seconds after..."
    "hook",    // "lain hook: 1 granted" — an output prefix, not a command
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

/// The README's command table must match the binary, both directions.
///
/// It said "After install, `lain` exposes exactly five subcommands" above
/// a table listing nine, while `lain --help` printed ten — and the
/// paragraph below the table announced that `init` had been removed,
/// which it had not. `oneshot` existed and appeared nowhere. Someone
/// reading the README to learn the tool got a count, a table, and a
/// binary that disagreed with each other three ways.
#[test]
fn the_readme_command_table_matches_the_binary() {
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"),
    )
    .expect("read README.md");

    // Rows look like: | `lain server` | Start the MCP server ... |
    let mut documented = HashSet::new();
    for line in readme.lines() {
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
        "found no `| \\`lain <cmd>\\` |` rows in the README command table"
    );

    let actual = subcommands();

    let phantom: Vec<_> = documented.difference(&actual).cloned().collect();
    assert!(
        phantom.is_empty(),
        "README documents commands the binary does not have: {phantom:?}"
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
        "the binary has commands the README never mentions: {missing:?}"
    );
}

/// The prose around the table must not contradict it — the old copy
/// claimed a subcommand count that matched neither the table nor the
/// binary.
#[test]
fn the_readme_does_not_claim_a_stale_subcommand_count() {
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"),
    )
    .expect("read README.md");

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
            !readme.contains(spelled),
            "README hard-codes a subcommand count (\"{spelled}\") that will \
             go stale the next time a command is added or removed; describe \
             the table instead of counting it"
        );
    }
}
