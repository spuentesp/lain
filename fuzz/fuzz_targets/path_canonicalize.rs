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
//! Claim arbitration and its path normalization belong to Zoanthrope;
//! this target exercises the path helpers still owned by Lain.
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
use std::path::Path;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // Same lossy conversion the production code uses — invalid
    // UTF-8 must not panic the fuzzer itself.
    let input = String::from_utf8_lossy(data);

    // The string is the production entry point; feed it through a
    // round-trip via `Path` (which is what callers do).
    let path = Path::new(&*input);

    let _ = posix_string(&path);
    let _ = lexical_normalize(&path);
    let _ = canonical_form(&path);
});
