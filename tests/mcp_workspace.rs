//! `lain mcp` workspace parsing: any agent hands the binary the
//! repo(s) it's working on, and `lain mcp` prepares. This file pins
//! the CLI surface so the parsing layer doesn't drift.

use clap::Parser;
use lain::cli::{Args, Commands};

fn parse_mcp(argv: &[&str]) -> Vec<std::path::PathBuf> {
    // Wrap so the rest of the test reads as "the Mcp variant's workspace".
    let full = std::iter::once("lain")
        .chain(argv.iter().copied())
        .collect::<Vec<_>>();
    let parsed = Args::parse_from(full);
    match parsed.command {
        Some(Commands::Mcp { workspace, .. }) => workspace,
        other => panic!("expected Mcp command, got {other:?}"),
    }
}

#[test]
fn no_workspace_flag_yields_empty_vec_for_auto_discovery() {
    // `lain mcp` with no args must NOT pin to a specific workspace;
    // empty Vec triggers the env-var / parent-cwd / process-cwd chain.
    let got = parse_mcp(&["mcp"]);
    assert!(
        got.is_empty(),
        "no --workspace flag should produce an empty Vec, got {got:?}"
    );
}

#[test]
fn single_workspace_flag_yields_single_entry() {
    let got = parse_mcp(&["mcp", "--workspace", "/tmp/one"]);
    assert_eq!(got, vec![std::path::PathBuf::from("/tmp/one")]);
}

#[test]
fn repeated_workspace_flag_yields_all_entries_in_order() {
    let got = parse_mcp(&[
        "mcp",
        "--workspace", "/tmp/a",
        "--workspace", "/tmp/b",
        "--workspace", "/tmp/c",
    ]);
    assert_eq!(
        got,
        vec![
            std::path::PathBuf::from("/tmp/a"),
            std::path::PathBuf::from("/tmp/b"),
            std::path::PathBuf::from("/tmp/c"),
        ]
    );
}

#[test]
fn interleaved_workspace_and_other_flags_preserves_workspace_order() {
    let got = parse_mcp(&[
        "mcp",
        "--workspace", "/tmp/a",
        "--embedding-model", "/path/to/model",
        "--workspace", "/tmp/b",
    ]);
    assert_eq!(
        got,
        vec![
            std::path::PathBuf::from("/tmp/a"),
            std::path::PathBuf::from("/tmp/b"),
        ]
    );
}
