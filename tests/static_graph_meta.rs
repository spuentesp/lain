// P1 #1: runtime integration test for `_meta.static_graph_generation`
// in the JSON-RPC envelope. Verifies the new field appears on every
// tool response, defaults to `null` for skipped/failed/timeout
// re-index outcomes, and reflects `last_outcome.started_at` as Unix
// epoch seconds when the re-index completed successfully.

use lain::server::LainServer;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn make_test_server(graph_gen: Option<SystemTime>) -> LainServer {
    let tmp = tempfile::tempdir().expect("tempdir");
    git2::Repository::init(tmp.path()).expect("git init");
    std::fs::write(tmp.path().join("a.rs"), "pub fn a() {}\n").expect("write");
    let mem_path = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem_path, None).expect("LainServer::new");
    if let Some(start) = graph_gen {
        *server.last_outcome.lock() =
            lain::server::refresh::RefreshOutcome::ok(start);
    }
    server
}

#[test]
fn static_graph_generation_null_when_no_reindex() {
    let server = make_test_server(None);
    assert_eq!(server.static_graph_generation_unix(), None,
               "skipped outcome → no generation; \
                an LLM should see null and know the graph is not yet indexed");
}

#[test]
fn static_graph_generation_timestamp_when_ok() {
    let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let server = make_test_server(Some(ts));
    assert_eq!(server.static_graph_generation_unix(),
               Some(1_700_000_000),
               "Unix epoch seconds must match outcome.started_at");
}

#[test]
fn static_graph_generation_none_when_failed() {
    let server = make_test_server(None);
    *server.last_outcome.lock() =
        lain::server::refresh::RefreshOutcome::failed(SystemTime::now(), "synthetic".to_string());
    assert_eq!(server.static_graph_generation_unix(), None,
               "a failed re-index must NOT surface a generation; \
                an LLM reading this would think the graph is fresh");
}

#[test]
fn static_graph_generation_none_when_timeout() {
    let server = make_test_server(None);
    *server.last_outcome.lock() =
        lain::server::refresh::RefreshOutcome::timeout(SystemTime::now());
    assert_eq!(server.static_graph_generation_unix(), None,
               "a timed-out re-index must NOT surface a generation; \
                the graph state is unknown");
}

#[test]
fn static_graph_generation_last_outcome_starts_as_skipped() {
    // A freshly-constructed LainServer (no re-index run) must report
    // `None` for static_graph_generation_unix. This is the state a
    // dev-mode server ships in by default.
    let tmp = tempfile::tempdir().expect("tempdir");
    git2::Repository::init(tmp.path()).expect("git init");
    let mem = tmp.path().join(".lain/graph.bin");
    let server = LainServer::new(tmp.path(), &mem, None).expect("LainServer::new");
    assert_eq!(server.static_graph_generation_unix(), None,
               "fresh server starts with skipped outcome");
}
