//! Fuzz target: path-canonicalization surface.
//!
//! The MCP wire format and every audit-log entry store paths as
//! forward-slash strings regardless of host OS. Two layers sit
//! between an incoming path string and the on-disk key:
//!
//!   * `posix_string` (src/server/path_util.rs:22) — render
//!     `&Path` as `/`-separated, strip the Windows `\\?\`
//!     extended-length prefix.
//!   * `lexical_normalize` (src/server/path_util.rs:39) — purely
//!     lexical path normalization (no FS access).
//!   * `canonical_form` (src/server/path_util.rs:68) — lexical
//!     normalize then `std::fs::canonicalize` (resolves symlinks,
//!     touches the FS).
//!   * `canonical_claim_path` (src/server/presence.rs:774) —
//!     given a list of "claim roots" and a candidate path, return
//!     the workspace-relative form the server uses as the graph
//!     key. Joins path against each root, takes the first that
//!     exists, strips the primary root.
//!
//! Every agent operation that takes a path (HTTP body, CLI arg,
//! `extract_refs_with_locals` input, `extract_strings` input, etc.)
//! funnels through one of these. A panic, infinite loop, or
//! quadratic-cost step on adversarial input — `/` traversal,
//! `..` smuggling, mixed separators, embedded NULs, non-UTF-8
//! bytes — is a security/correctness bug. Fuzzing each function
//! with arbitrary `&[u8]` and feeding the lossy-decoded input
//! into a `Path` covers the same attack surface the network sees.

#![no_main]

use lain::server::path_util::{canonical_form, lexical_normalize, posix_string};
use lain::server::presence::canonical_claim_path;
use std::path::{Path, PathBuf};

#[libfuzzer_sys::fuzz_target]
fn fuzz_path_canonicalize(data: &[u8]) {
    // Same lossy conversion the production code uses — invalid
    // UTF-8 must not panic the fuzzer itself.
    let input = String::from_utf8_lossy(data);

    // The string is the production entry point; feed it through a
    // round-trip via `Path` (which is what callers do).
    let path = Path::new(&*input);

    // Build a non-empty list of claim roots so the
    // `canonical_claim_path` branch that joins against each root
    // (the production path with `set_workspace_root` configured)
    // actually gets exercised. Empty roots short-circuit to the
    // `posix_string(path)` fallback which only re-tests the path
    // formatter — the join-and-strip-prefix logic isn't hit.
    //
    // We can't create real on-disk roots (the fuzzer is sandboxed),
    // but `canonical_form` is a no-op on non-existent paths (returns
    // the lexically-normalized form), so a synthetic root that
    // doesn't exist on disk still drives the full prefix-stripping
    // branch — the function doesn't care if the root exists, only
    // whether the candidate path can be stripped against it.
    let roots: Vec<PathBuf> = vec![
        PathBuf::from("/workspace"),
        PathBuf::from("/repo"),
        PathBuf::from("/tmp/lain-test-roots/primary"),
    ];

    let _ = posix_string(&path);
    let _ = lexical_normalize(&path);
    let _ = canonical_form(&path);
    let _ = canonical_claim_path(&roots, &path);
}
