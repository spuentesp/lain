use super::*;
#[allow(unused_imports)]
use crate::federation::contracts::model::{ConsumerFact, SourceSite};
#[allow(unused_imports)]
use crate::schema::StaticSource;

#[test]
fn file_metadata_is_built_once_and_owned_ranges_resolve_to_functions() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("sample.py"),
        "def send():\n    return helper()\n\ndef helper():\n    return {'id': 1}\n",
    )
    .unwrap();
    let mut files = ParsedFiles::default();
    let mut metrics = FieldAccessMetrics::default();

    assert!(files.ensure(tmp.path(), "sample.py", &mut metrics));
    assert!(files.ensure(tmp.path(), "sample.py", &mut metrics));
    assert_eq!(metrics.file_metadata_builds, 1);

    let file = files.get("sample.py").unwrap();
    let send = file.defs.iter().find(|def| def.name == "send").unwrap();
    let node = file.node(send).unwrap();
    assert_eq!(node.kind(), "function_definition");
}
