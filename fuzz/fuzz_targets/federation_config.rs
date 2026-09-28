//! Fuzz target: parse arbitrary bytes as `FederationConfig` (YAML).
//!
//! This is the federation config file format (`repos.yaml`). The
//! parser is `serde_yaml::from_str` against a struct with several
//! nested fields and a `Vec<RepoSource>` enum. A malformed or
//! adversarial input that triggers a panic, infinite loop, or
//! excessive allocation in the parser is a security/correctness
//! issue — `FederationConfig::load_from_str` is the entry point
//! any operator-driven config file goes through.

#![no_main]

use lain::server::federation::config::FederationConfig;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // The parser takes `&str`; lossy conversion here lets us
    // exercise non-UTF-8 input without panicking on the conversion.
    let input = String::from_utf8_lossy(data);
    // We don't care whether parse succeeds or fails — only that it
    // doesn't panic, hang, or allocate gigabytes.
    let _ = FederationConfig::load_from_str(&input);
});
