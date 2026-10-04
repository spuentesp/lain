//! Phase 3 — precision/recall on real-repos data.
//!
//! Spawns `tests/real_federation/ground_truth.sh` (Phase 1) and
//! asserts it returns 0. The shell script validates the precision
//! invariant: 0 cross-repo Binds, 0 unresolved consumers, complete
//! coverage — for the bytes+serde+tokio fixture.
//!
//! Marked `#[ignore]` so the default `cargo test` (offline CI)
//! skips it. Run with:
//!     cargo test --test metrics -- --ignored --nocapture
//!
//! Prereqs: `scripts/demo-federation-fixture.sh` already ran.

use std::path::PathBuf;
use std::process::Command;

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
#[ignore = "requires network (clones bytes, tokio, serde from GitHub)"]
fn real_repos_ground_truth_holds() {
    let script = project_root().join("tests/real_federation/ground_truth.sh");
    assert!(script.exists(), "missing {}", script.display());

    let bin = std::env::var("LAIN_BIN")
        .unwrap_or_else(|_| "target/debug/lain".to_string());
    let fixture = std::env::var("REAL_FED_FIXTURE_DIR")
        .unwrap_or_else(|_| "/tmp/real-federation-test".to_string());

    let status = Command::new("bash")
        .arg(&script)
        .arg(&fixture)
        .arg("19877") // separate port from any local dev server
        .env("LAIN_BIN", &bin)
        .status()
        .expect("spawn ground_truth.sh");
    assert!(
        status.success(),
        "ground_truth.sh exited {status}; ground truth did NOT hold for {fixture}"
    );
}
