//! Fuzz target: federation config → repo source construction.
//!
//! `FederationConfig::load` parses a `repos.yaml` (or similar
//! operator-driven file). For each configured repo, the config
//! carries a `source: SourceConfig` enum with three variants:
//!
//!   * `LocalClone { url, ref }`
//!   * `ShallowClone { url, ref, refresh_interval_secs }`
//!   * `WorkspaceDir { path }`
//!
//! `build_source_for(repo)` walks this enum and constructs a
//! concrete `Box<dyn RepoSource>`. The URL and path fields are
//! operator-supplied; the refresh interval is operator-supplied
//! (defaults to 300s). A panicking `URL::parse` (rare but
//! happens) or a malformed path would break this construction.
//!
//! This fuzzer goes further than `federation_config`:
//!
//!   * `federation_config` only exercises YAML parsing.
//!   * `repo_source_load` exercises the full chain: parse YAML →
//!     iterate repos → build_source_for() for each repo → call the
//!     constructor of whichever Source variant matched.
//!
//! A panic, infinite loop, or unbounded allocation anywhere in
//! that chain is a real bug — `build_source_for` is called once
//! per repo on every server start.

#![no_main]

use lain::server::federation::config::FederationConfig;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // Same lossy round-trip the production loader does. Invalid
    // UTF-8 must not panic the fuzzer itself.
    let input = String::from_utf8_lossy(data);

    // Parse the operator config.
    let cfg = match FederationConfig::load_from_str(&input) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Walk every configured repo, build its source. The
    // FederationConfig::build_source_for call exercises the full
    // Source enum dispatch (LocalClone / ShallowClone /
    // WorkspaceDir) and each constructor's URL/path parsing.
    for repo in &cfg.repos {
        // Result is dropped — we only care that the constructor
        // path returns or errs, not what it returned.
        let _ = cfg.build_source_for(repo);
    }
});
