mod common;

use common::{boot_single_repo_in_dir, git_init_committed, jsonrpc, tools_call_envelope};
use jsonschema::Validator;
use serde_json::{json, Value};

struct Fixture {
    _tmp: tempfile::TempDir,
    host: String,
    _server: common::ServerGuard,
}

impl Fixture {
    fn new() -> Self {
        std::env::set_var("LAIN_TOOL_PROFILE", "full");
        let tmp = tempfile::tempdir().expect("fixture tempdir");
        let root = tmp.path().to_path_buf();
        let repo = root.join("orders");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub mod a;\npub mod b;\n").unwrap();
        std::fs::write(
            repo.join("src/a.rs"),
            "pub fn shared() -> u32 { 42 }\n\npub fn caller() -> u32 {\n    let first = shared();\n    first + shared()\n}\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("src/b.rs"),
            "pub fn shared() -> u32 { 43 }\n\npub fn second_caller() -> u32 {\n    shared()\n}\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"orders\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        git_init_committed(&repo);
        let repos_yaml = root.join("repos.yaml");
        std::fs::write(
            &repos_yaml,
            format!(
                "data_dir: {}/data\nrepos:\n  - id: orders\n    source:\n      type: workspace_dir\n      path: {}\n",
                root.display(),
                repo.display()
            ),
        )
        .unwrap();
        let (host, server) =
            boot_single_repo_in_dir(&root, &repo, &repos_yaml, &["shared", "caller"]);
        Self {
            _tmp: tmp,
            host,
            _server: server,
        }
    }

    fn call(&self, name: &str, args: Value) -> Value {
        tools_call_envelope(&self.host, name, args)
    }
}

fn text(result: &Value) -> &str {
    result
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn structured(result: &Value) -> &Value {
    result
        .pointer("/result/structuredContent")
        .unwrap_or(&Value::Null)
}

#[tokio::test]
async fn raw_tools_return_schema_valid_structured_content_and_legacy_text() {
    let fixture = Fixture::new();
    let calls = [
        ("find_symbol", json!({"name": "shared"})),
        ("get_context", json!({"symbol": "shared"})),
        ("get_call_sites", json!({"symbol": "shared"})),
        ("get_cross_runtime_callers", json!({"node_id": "shared"})),
        ("explain_symbol", json!({"symbol": "shared"})),
        ("assess_change", json!({"symbol": "shared"})),
        ("explain_dispatch", json!({"symbol": "shared"})),
        ("search_code", json!({"query": "shared", "mode": "lexical"})),
        (
            "query_graph",
            json!({"spec": {"ops": [{"op": "find", "name": "shared"}]}}),
        ),
    ];
    let listed = jsonrpc(
        &fixture.host,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string(),
    );
    for (name, args) in calls {
        let result = fixture.call(name, args);
        assert_eq!(
            result.pointer("/result/isError").and_then(Value::as_bool),
            Some(false),
            "{name} failed: {result}"
        );
        assert!(text(&result).contains("shared"), "{name} lost legacy text");
        let value = structured(&result);
        assert!(
            value.is_object(),
            "{name} has no structuredContent: {result}"
        );
        let schema = listed
            .pointer("/result/tools")
            .and_then(Value::as_array)
            .and_then(|tools| tools.iter().find(|tool| tool["name"] == name))
            .and_then(|tool| tool.get("outputSchema"))
            .unwrap_or_else(|| panic!("{name} has no outputSchema: {listed}"));
        let validator: Validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .build(schema)
            .unwrap_or_else(|e| panic!("{name} schema invalid: {e}"));
        validator
            .validate(value)
            .unwrap_or_else(|e| panic!("{name} structuredContent failed schema: {e}\n{value}"));
    }

    let found = fixture.call("find_symbol", json!({"name": "shared"}));
    assert_eq!(structured(&found)["count"], 2);
    assert_eq!(
        structured(&found)["symbols"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(structured(&found)["ambiguous"], true);

    let empty = fixture.call("find_symbol", json!({"name": "not_present"}));
    assert_eq!(structured(&empty)["count"], 0);
    assert_eq!(
        structured(&empty)["symbols"].as_array().map(Vec::len),
        Some(0)
    );

    let call_sites = fixture.call("get_call_sites", json!({"symbol": "shared"}));
    assert_eq!(structured(&call_sites)["count"], 2);
    assert!(text(&call_sites).contains("2 call(s) across 1 function(s)"));

    let context_zero = fixture.call("get_context", json!({"symbol": "shared", "depth": 0}));
    assert_eq!(
        structured(&context_zero)["callers"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );
    assert_eq!(
        structured(&context_zero)["callees"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );
    assert!(!text(&context_zero).contains("## Callers"));
    assert!(!text(&context_zero).contains("## Callees"));

    let context_two = fixture.call("get_context", json!({"symbol": "shared", "depth": 2}));
    assert!(!structured(&context_two)["callers"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(structured(&context_two)["callees"].is_array());
    assert!(text(&context_two).contains("## Callers"));
    assert!(text(&context_two).contains("## Callees"));
}
