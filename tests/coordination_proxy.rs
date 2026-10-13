//! Proxy contract tests — grant / conflict / degraded shapes and the
//! symbol-stamp passthrough, against a fake `zoanthrope` binary so the
//! suite is deterministic (no real zoanthrope install needed; unix-only
//! because the fake is a shell script).
//!
//! These replace the deleted in-memory claims-registry tests: the
//! arbitration core is zoanthrope's now (see
//! `docs/formal/COORDINATION.md`), and what lain must pin is the
//! *proxy contract* — the JSON shapes crossing the subprocess boundary
//! and the `caller_stamps` pins lain computes.

#![cfg(unix)]

use lain::server::coordination::shapes::{symbols_from_stamps, CallerStamp, ClaimOutcome};
use lain::server::coordination::{symbol_stamps, CoordinationProxy};
use lain::server::presence::{AgentId, ClaimIntent, ClaimRequest};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Write the fake `zoanthrope` shell script into `dir` and return its
/// path. Behavior is driven by a sibling `mode` file (`grant` default,
/// `conflict`, `stale`); every invocation appends its argv to a
/// sibling `calls.log` so tests can assert on what the proxy sent.
/// State lives next to the script — not in env vars — so parallel
/// tests never race.
fn install_fake_zoanthrope(dir: &Path, mode: &str) -> PathBuf {
    let path = dir.join("zoanthrope");
    std::fs::write(dir.join("mode"), mode).expect("write mode");
    let script = r#"#!/bin/sh
here="$(dirname "$0")"
log="$here/calls.log"
printf '%s\n' "$*" >> "$log"
mode="$(cat "$here/mode" 2>/dev/null || echo grant)"
cmd="$1"
if [ "$cmd" = "mcp" ]; then
  while IFS= read -r line; do
    case "$line" in
      *'"name":"list_claims"'*)
        printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"ok\":true,\"claims\":[{\"path\":\"a.rs\",\"agent\":\"beta\",\"intent\":\"edit\",\"claimed_at\":1700000000.0,\"caller_stamps\":[{\"name\":\"symbol:parse_token\",\"value\":\"deadbeef\"}]}]}"}],"isError":false}}'
        ;;
      *'"name":"register_agent"'*)
        printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"ok\":true,\"agent_id\":\"alpha\",\"name\":\"alpha\",\"kind\":\"other\",\"last_seen\":1700000000.0}"}],"isError":false}}'
        ;;
      *'"name":"post_intention"'*)
        printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"ok\":true,\"post_id\":\"p-1\",\"post\":{\"post_id\":\"p-1\",\"agent_id\":\"alpha\",\"title\":\"goal\",\"files\":[],\"note\":\"note\",\"status\":\"open\",\"created_at\":1700000000.0,\"updated_at\":1700000000.0}}"}],"isError":false}}'
        ;;
      *'"name":"list_posts"'*)
        printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"ok\":true,\"posts\":[]}"}],"isError":false}}'
        ;;
    esac
  done
  exit 0
fi
files=""
agent=""
stamp_json=""
prev=""
for a in "$@"; do
  case "$prev" in
    --files) files="$a" ;;
    --agent) agent="$a" ;;
    --stamp)
      name="${a%%=*}"; val="${a#*=}"
      if [ -n "$stamp_json" ]; then stamp_json="$stamp_json,"; fi
      stamp_json="$stamp_json{\"name\":\"$name\",\"value\":\"$val\"}"
      ;;
  esac
  prev="$a"
done
case "$cmd" in
  claim)
    case "$mode" in
      conflict)
        printf '{"ok": false, "holder": "beta", "path": "%s", "claimed_at": 1700000000.0, "purpose": "fix the parser", "holder_stale": false, "intent": "edit", "caller_stamps": [{"name": "symbol:parse_token", "value": "cafebabe"}]}\n' "$files"
        exit 1
        ;;
      stale)
        printf '{"ok": false, "reason": "stale-claim", "holder": "%s", "path": "%s", "claimed_at": 1700000000.0, "hint": "re-read the file, then re-claim before writing"}\n' "$agent" "$files"
        exit 1
        ;;
      *)
        printf '{"ok": true, "granted": [{"path": "%s", "claimed_at": 1700000000.0, "purpose": null, "intent": "edit", "caller_stamps": [%s]}]}\n' "$files" "$stamp_json"
        exit 0
        ;;
    esac
    ;;
  release)
    printf '{"ok": true, "released": ["%s"]}\n' "$files"
    exit 0
    ;;
  rejoin)
    printf '{"ok": true, "agent_id": "%s", "claims_kept": [], "claims_at_risk": []}\n' "$agent"
    exit 0
    ;;
  *) exit 2 ;;
esac
"#;
    let mut f = std::fs::File::create(&path).expect("create fake zoanthrope");
    f.write_all(script.as_bytes()).expect("write fake");
    drop(f);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn stamp(name: &str, value: &str) -> CallerStamp {
    CallerStamp {
        name: name.to_string(),
        value: value.to_string(),
    }
}

#[test]
fn grant_shape_parses_with_caller_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let bin = install_fake_zoanthrope(dir.path(), "grant");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let proxy = CoordinationProxy::with_bin(ws, Some(bin));

    let out = proxy
        .claim_file(
            "alpha",
            "a.rs",
            Some("fix the parser"),
            ClaimIntent::Edit,
            &[stamp("symbol:parse_token", "cafebabe")],
        )
        .expect("claim call");
    match out {
        ClaimOutcome::Granted(g) => {
            assert_eq!(g.path, "a.rs");
            assert_eq!(g.intent, "edit");
            assert_eq!(
                g.caller_stamps,
                vec![stamp("symbol:parse_token", "cafebabe")]
            );
            assert_eq!(g.symbols(), vec!["parse_token".to_string()]);
        }
        other => panic!("expected grant, got {other:?}"),
    }
    // The pins crossed the subprocess boundary as `--stamp name=value`.
    let logged = std::fs::read_to_string(dir.path().join("calls.log")).expect("fake log");
    assert!(
        logged.contains("--stamp symbol:parse_token=cafebabe"),
        "caller stamp must be forwarded on the CLI; log: {logged}"
    );
}

#[test]
fn conflict_shape_names_the_holder() {
    let dir = tempfile::tempdir().unwrap();
    let bin = install_fake_zoanthrope(dir.path(), "conflict");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let proxy = CoordinationProxy::with_bin(ws, Some(bin));

    let out = proxy
        .claim_file("alpha", "a.rs", None, ClaimIntent::Edit, &[])
        .expect("claim call");
    match out {
        ClaimOutcome::Refused(r) => {
            // Holder naming is the swarm experiments' contract.
            assert_eq!(r.holder, "beta");
            assert_eq!(r.path, "a.rs");
            assert_eq!(r.intent, "edit");
            assert!(!r.holder_stale);
            assert_eq!(r.purpose.as_deref(), Some("fix the parser"));
            assert_eq!(r.symbols(), vec!["parse_token".to_string()]);
            assert!(!r.is_stale_claim());
        }
        other => panic!("expected refusal, got {other:?}"),
    }
}

#[test]
fn stale_claim_refusal_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let bin = install_fake_zoanthrope(dir.path(), "stale");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let proxy = CoordinationProxy::with_bin(ws, Some(bin));

    let out = proxy
        .claim_file("alpha", "a.rs", None, ClaimIntent::Edit, &[])
        .expect("claim call");
    match out {
        ClaimOutcome::Refused(r) => {
            assert!(r.is_stale_claim());
            assert_eq!(r.holder, "alpha", "stale-claim holder is the caller");
            assert!(r.hint.is_some());
        }
        other => panic!("expected refusal, got {other:?}"),
    }
}

/// Degraded mode: no zoanthrope → advisory semantics. The claim is
/// granted-looking (never blocks) and the batch reports the marker.
#[test]
fn degraded_mode_is_advisory_and_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let proxy = CoordinationProxy::with_bin(ws, None);

    assert!(proxy.is_degraded());
    assert_eq!(
        proxy.degraded_marker(),
        Some("zoanthrope-missing"),
        "degraded responses carry this marker"
    );

    let out = proxy
        .claim_file("alpha", "a.rs", None, ClaimIntent::Edit, &[])
        .expect("degraded claim must not error");
    assert!(
        matches!(out, ClaimOutcome::Degraded { .. }),
        "expected degraded outcome, got {out:?}"
    );

    // The facade grants the request (advisory) and logs it.
    let result = proxy.claim(
        &AgentId("alpha".into()),
        vec![ClaimRequest {
            path: PathBuf::from("a.rs"),
            symbols: vec![],
            intent: ClaimIntent::Edit,
            ttl_seconds: None,
            plan_revision: None,
        }],
    );
    assert_eq!(result.granted.len(), 1, "degraded claim is granted advisory");
    assert!(result.conflicts.is_empty(), "degraded never conflicts");
    assert!(
        !proxy.degraded_notes().is_empty(),
        "degraded claim is logged"
    );
}

/// Symbol-stamp passthrough: lain computes `symbol:<name>` pins from
/// the declared symbols' bodies; they must reach zoanthrope verbatim
/// and read back out of `caller_stamps` as the same symbols.
#[test]
fn symbol_stamps_pass_through_as_caller_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    // A tiny source file with one function — the code-smarts half.
    let src = ws.join("a.rs");
    std::fs::write(&src, "fn parse_token() -> u8 { 42 }\nfn other() {}\n").unwrap();
    let pins = symbol_stamps(&src, &["parse_token".to_string()]);
    assert_eq!(pins.len(), 1, "one stamp per resolvable symbol");
    assert_eq!(pins[0].name, "symbol:parse_token");
    // 64-char lowercase hex — BLAKE3-256 of the symbol body bytes.
    assert_eq!(pins[0].value.len(), 64);
    assert!(pins[0].value.chars().all(|c| c.is_ascii_hexdigit()));

    // Unresolvable symbols are skipped (a stamp is evidence, never a
    // guess) and empty input yields no pins.
    assert!(symbol_stamps(&src, &["nope".to_string()]).is_empty());
    assert!(symbol_stamps(&src, &[]).is_empty());

    // Round-trip through the wire shapes.
    let back = symbols_from_stamps(&pins);
    assert_eq!(back, vec!["parse_token".to_string()]);

    // And the pins reach the zoanthrope CLI (fake records argv).
    let bin = install_fake_zoanthrope(dir.path(), "grant");
    let proxy = CoordinationProxy::with_bin(ws.clone(), Some(bin));
    let out = proxy
        .claim_file("alpha", "a.rs", None, ClaimIntent::Edit, &pins)
        .expect("claim call");
    match out {
        ClaimOutcome::Granted(g) => {
            assert_eq!(g.symbols(), vec!["parse_token".to_string()]);
        }
        other => panic!("expected grant, got {other:?}"),
    }
    let logged = std::fs::read_to_string(dir.path().join("calls.log")).expect("fake log");
    assert!(
        logged.contains("--stamp symbol:parse_token="),
        "symbol pin must cross the subprocess boundary; log: {logged}"
    );
}

/// MCP mirror path: `list_claims` rows adapt to lain's `Claim` DTO,
/// with symbols recovered from `symbol:*` caller stamps.
#[test]
fn mcp_list_claims_adapts_to_lain_claims() {
    let dir = tempfile::tempdir().unwrap();
    let bin = install_fake_zoanthrope(dir.path(), "grant");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let proxy = CoordinationProxy::with_bin(ws, Some(bin));

    let snap = proxy.claims_snapshot();
    let claims = snap.all();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].agent_id.as_str(), "beta");
    assert_eq!(claims[0].symbols, vec!["parse_token".to_string()]);
    assert_eq!(claims[0].intent, ClaimIntent::Edit);

    let by_path = snap.for_path(Path::new("a.rs")).expect("claim for a.rs");
    assert_eq!(by_path.agent_id.as_str(), "beta");

    let for_beta = proxy.claims_for_agent(&AgentId("beta".into()));
    assert_eq!(for_beta.len(), 1);
}

/// Release and rejoin speak the same CLI JSON contract.
#[test]
fn release_and_rejoin_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let bin = install_fake_zoanthrope(dir.path(), "grant");
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let proxy = CoordinationProxy::with_bin(ws, Some(bin));

    let released = proxy.release(&AgentId("alpha".into()), &[PathBuf::from("a.rs")]);
    assert_eq!(released, vec![PathBuf::from("a.rs")]);

    let body = proxy.rejoin("alpha", None).expect("rejoin");
    assert_eq!(body["ok"], true);
    assert_eq!(body["agent_id"], "alpha");
}
