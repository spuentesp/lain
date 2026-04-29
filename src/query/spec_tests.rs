//! Tests for query/spec.rs

use crate::query::spec::*;

#[test]
fn test_depth_spec_single() {
    let ds = DepthSpec::Single(5);
    let range = ds.to_range();
    assert!(range.contains(&5));
    assert!(!range.contains(&4));
    assert!(!range.contains(&6));
}

#[test]
fn test_depth_spec_range() {
    let ds = DepthSpec::Range { min: 2, max: 5 };
    let range = ds.to_range();
    assert!(range.contains(&2));
    assert!(range.contains(&3));
    assert!(range.contains(&5));
    assert!(!range.contains(&1));
    assert!(!range.contains(&6));
}

#[test]
fn test_depth_spec_default() {
    let ds = DepthSpec::default();
    let range = ds.to_range();
    let mut iter = range;
    assert_eq!(iter.next(), Some(1));
    assert_eq!(iter.next(), None);
}

#[test]
fn test_type_selector_single() {
    let ts = TypeSelector::Single("Function".to_string());
    assert!(ts.matches("Function"));
    assert!(!ts.matches("Struct"));
    assert!(!ts.matches("Module"));
}

#[test]
fn test_type_selector_or() {
    let ts = TypeSelector::Or(vec!["Function".to_string(), "Method".to_string()]);
    assert!(ts.matches("Function"));
    assert!(ts.matches("Method"));
    assert!(!ts.matches("Struct"));
    assert!(!ts.matches("Module"));
}

#[test]
fn test_label_selector_single() {
    let ls = LabelSelector::Single("deprecated".to_string());
    assert!(ls.matches(Some("deprecated")));
    assert!(!ls.matches(Some("stable")));
    assert!(!ls.matches(None));
}

#[test]
fn test_label_selector_or() {
    let ls = LabelSelector::Or(vec!["deprecated".to_string(), "beta".to_string()]);
    assert!(ls.matches(Some("deprecated")));
    assert!(ls.matches(Some("beta")));
    assert!(!ls.matches(Some("stable")));
    assert!(!ls.matches(None));
}

#[test]
fn test_label_selector_not() {
    let ls = LabelSelector::Not(vec!["deprecated".to_string(), "beta".to_string()]);
    assert!(ls.matches(Some("stable")));
    assert!(ls.matches(None)); // None means no label = not deprecated
    assert!(!ls.matches(Some("deprecated")));
    assert!(!ls.matches(Some("beta")));
}

#[test]
fn test_edge_selector_single() {
    let es = EdgeSelector::Single("Calls".to_string());
    assert!(es.matches("Calls"));
    assert!(!es.matches("Uses"));
}

#[test]
fn test_edge_selector_or() {
    let es = EdgeSelector::Or(vec!["Calls".to_string(), "Imports".to_string()]);
    assert!(es.matches("Calls"));
    assert!(es.matches("Imports"));
    assert!(!es.matches("Uses"));
}

#[test]
fn test_edge_selector_not() {
    let es = EdgeSelector::Not(vec!["Calls".to_string(), "Imports".to_string()]);
    assert!(es.matches("Uses"));
    assert!(es.matches("Contains"));
    assert!(!es.matches("Calls"));
    assert!(!es.matches("Imports"));
}

#[test]
fn test_name_selector_exact() {
    let ns = NameSelector::Exact("main".to_string());
    assert!(ns.matches("main"));
    assert!(!ns.matches("Main"));
    assert!(!ns.matches("main.rs"));
}

#[test]
fn test_name_selector_starts_with() {
    let ns = NameSelector::StartsWith("test_".to_string());
    assert!(ns.matches("test_foo"));
    assert!(ns.matches("test_bar"));
    assert!(!ns.matches("foo_test"));
    assert!(!ns.matches("Test_Foo"));
}

#[test]
fn test_name_selector_ends_with() {
    let ns = NameSelector::EndsWith("_test".to_string());
    assert!(ns.matches("foo_test"));
    assert!(ns.matches("bar_test"));
    assert!(!ns.matches("test_foo"));
}

#[test]
fn test_name_selector_glob_star() {
    let ns = NameSelector::Glob("test_*".to_string());
    assert!(ns.matches("test_foo"));
    assert!(ns.matches("test_bar"));
    assert!(!ns.matches("testfoo"));
    assert!(!ns.matches("Test_Foo"));
}

#[test]
fn test_name_selector_glob_question() {
    let ns = NameSelector::Glob("test_?".to_string());
    assert!(ns.matches("test_a"));
    assert!(ns.matches("test_b"));
    assert!(!ns.matches("test_aa"));
}

#[test]
fn test_name_selector_glob_complex() {
    let ns = NameSelector::Glob("get_*_test".to_string());
    assert!(ns.matches("get_foo_test"));
    assert!(ns.matches("get_bar_test"));
    assert!(!ns.matches("get_footest"));
}

#[test]
fn test_direction_default() {
    let dir = Direction::default();
    assert_eq!(dir, Direction::Outgoing);
}

#[test]
fn test_direction_outgoing() {
    let dir = Direction::Outgoing;
    let json = serde_json::to_string(&dir).unwrap();
    assert_eq!(json, "\"outgoing\"");
}

#[test]
fn test_direction_incoming() {
    let dir = Direction::Incoming;
    let json = serde_json::to_string(&dir).unwrap();
    assert_eq!(json, "\"incoming\"");
}

#[test]
fn test_direction_both() {
    let dir = Direction::Both;
    let json = serde_json::to_string(&dir).unwrap();
    assert_eq!(json, "\"both\"");
}

#[test]
fn test_direction_deserialize() {
    let out: Direction = serde_json::from_str("\"outgoing\"").unwrap();
    assert_eq!(out, Direction::Outgoing);
    let inc: Direction = serde_json::from_str("\"incoming\"").unwrap();
    assert_eq!(inc, Direction::Incoming);
    let both: Direction = serde_json::from_str("\"both\"").unwrap();
    assert_eq!(both, Direction::Both);
}

#[test]
fn test_find_op_default() {
    let op = FindOp::default();
    assert!(op.type_selector.is_none());
    assert!(op.name.is_none());
    assert!(op.id.is_none());
    assert!(op.label_selector.is_none());
    assert!(op.path.is_none());
}

#[test]
fn test_find_op_builder_type() {
    let op = FindOp::new().r#type("Function");
    assert!(matches!(op.type_selector, Some(TypeSelector::Single(s)) if s == "Function"));
}

#[test]
fn test_find_op_builder_name() {
    let op = FindOp::new().name("main");
    assert!(matches!(op.name, Some(NameSelector::Exact(s)) if s == "main"));
}

#[test]
fn test_find_op_builder_label() {
    let op = FindOp::new().label("deprecated");
    assert!(matches!(op.label_selector, Some(LabelSelector::Single(s)) if s == "deprecated"));
}

#[test]
fn test_connect_op_default() {
    let op = ConnectOp::default();
    assert!(matches!(op.edge, EdgeSelector::Single(s) if s == "Calls"));
    assert_eq!(op.direction, Direction::Outgoing);
    assert!(matches!(op.depth, DepthSpec::Single(1)));
    assert!(op.target.is_none());
}

#[test]
fn test_filter_op_default() {
    let op = FilterOp::default();
    assert!(op.type_filter.is_none());
    assert!(op.label_filter.is_none());
    assert!(op.name.is_none());
}

#[test]
fn test_group_op_default() {
    let op = GroupOp::default();
    assert_eq!(op.by, GroupBy::Type);
}

#[test]
fn test_sort_op_default_fields() {
    let sf = SortField::Name;
    let sd = SortDirection::Asc;
    assert_eq!(format!("{:?}", sf), "Name");
    assert_eq!(format!("{:?}", sd), "Asc");
}

#[test]
fn test_query_spec_new() {
    let spec = QuerySpec::new(vec![]);
    assert!(spec.ops.is_empty());
    assert_eq!(spec.mode, QueryMode::Auto);
    assert!(spec.named.is_none());
}

#[test]
fn test_query_mode_default() {
    let mode = QueryMode::default();
    assert_eq!(mode, QueryMode::Auto);
}

#[test]
fn test_query_mode_serde() {
    let mode = QueryMode::Query;
    let json = serde_json::to_string(&mode).unwrap();
    assert_eq!(json, "\"query\"");
    let deserialized: QueryMode = serde_json::from_str("\"query\"").unwrap();
    assert_eq!(deserialized, QueryMode::Query);
}

#[test]
fn test_query_mode_auto_deserialize() {
    let deserialized: QueryMode = serde_json::from_str("\"auto\"").unwrap();
    assert_eq!(deserialized, QueryMode::Auto);
    let tool: QueryMode = serde_json::from_str("\"tool\"").unwrap();
    assert_eq!(tool, QueryMode::Tool);
}

#[test]
fn test_query_spec_named_blast_radius() {
    let spec = QuerySpec::named("get_blast_radius").unwrap();
    assert_eq!(spec.ops.len(), 3);
}

#[test]
fn test_query_spec_named_call_chain() {
    let spec = QuerySpec::named("get_call_chain").unwrap();
    assert_eq!(spec.ops.len(), 2);
}

#[test]
fn test_query_spec_named_unknown() {
    let spec = QuerySpec::named("nonexistent_query");
    assert!(spec.is_none());
}

#[test]
fn test_query_spec_named_all() {
    let names = [
        "get_blast_radius",
        "get_call_chain",
        "get_file_functions",
        "get_function_imports",
        "get_callers",
        "get_callees",
        "get_module_functions",
        "get_test_coverage",
        "get_deprecated_functions",
    ];
    for name in names {
        let spec = QuerySpec::named(name);
        assert!(spec.is_some(), "named query '{}' should exist", name);
    }
}

#[test]
fn test_query_spec_ops_with_find_and_connect() {
    let spec = QuerySpec::new(vec![
        GraphOp::Find(FindOp::new().r#type("Function")),
        GraphOp::Connect(ConnectOp {
            edge: EdgeSelector::Single("Calls".into()),
            direction: Direction::Outgoing,
            depth: DepthSpec::Single(1),
            target: None,
        }),
    ]);
    assert_eq!(spec.ops.len(), 2);
    assert!(matches!(&spec.ops[0], GraphOp::Find(_)));
    assert!(matches!(&spec.ops[1], GraphOp::Connect(_)));
}

#[test]
fn test_query_spec_deserialize() {
    let json = r#"{"ops":[{"op":"find","type":"Function"},{"op":"connect","edge":"Calls","direction":"incoming","depth":{"min":1,"max":3}}]}"#;
    let spec: QuerySpec = serde_json::from_str(json).unwrap();
    assert_eq!(spec.ops.len(), 2);
}

#[test]
fn test_connect_op_with_target() {
    let json = r#"{"edge":"Calls","direction":"outgoing","depth":{"min":1,"max":2},"target":{"type":"Function"}}"#;
    let op: ConnectOp = serde_json::from_str(json).unwrap();
    assert!(op.target.is_some());
}

#[test]
fn test_find_op_with_path() {
    let json = r#"{"type":"Function","path":"/src/main.rs"}"#;
    let op: FindOp = serde_json::from_str(json).unwrap();
    assert_eq!(op.path, Some("/src/main.rs".to_string()));
}

#[test]
fn test_filter_op_deserialize() {
    // Test FilterOp with just type selector
    let json = r#"{"type":"Function"}"#;
    let op: FilterOp = serde_json::from_str(json).unwrap();
    assert!(op.type_filter.is_some());
    assert!(op.label_filter.is_none());
    assert!(op.name.is_none());
}

#[test]
fn test_graph_op_variants() {
    let find = GraphOp::Find(FindOp::default());
    let connect = GraphOp::Connect(ConnectOp::default());
    let filter = GraphOp::Filter(FilterOp::default());
    let sort = GraphOp::Sort(SortOp {
        by: SortField::Name,
        direction: SortDirection::Asc,
    });
    let limit = GraphOp::Limit(LimitOp {
        count: 1,
        offset: 0,
    });
    let group = GraphOp::Group(GroupOp::default());
    let semantic_filter = GraphOp::SemanticFilter(SemanticFilterOp::default());

    assert!(matches!(find, GraphOp::Find(_)));
    assert!(matches!(connect, GraphOp::Connect(_)));
    assert!(matches!(filter, GraphOp::Filter(_)));
    assert!(matches!(sort, GraphOp::Sort(_)));
    assert!(matches!(limit, GraphOp::Limit(_)));
    assert!(matches!(group, GraphOp::Group(_)));
    assert!(matches!(semantic_filter, GraphOp::SemanticFilter(_)));
}

#[test]
fn test_semantic_filter_op() {
    let sem = GraphOp::SemanticFilter(SemanticFilterOp {
        like: "auth handler".to_string(),
        threshold: 0.4,
    });
    assert!(matches!(sem, GraphOp::SemanticFilter(_)));
    if let GraphOp::SemanticFilter(s) = sem {
        assert_eq!(s.like, "auth handler");
        assert_eq!(s.threshold, 0.4);
    }
}

#[test]
fn test_semantic_filter_default() {
    let sem = SemanticFilterOp::default();
    assert_eq!(sem.like, "");
    assert_eq!(sem.threshold, 0.3);
}

#[test]
fn test_semantic_filter_serde() {
    let sem = SemanticFilterOp {
        like: "database connection".to_string(),
        threshold: 0.5,
    };
    let json = serde_json::to_string(&sem).unwrap();
    let back: SemanticFilterOp = serde_json::from_str(&json).unwrap();
    assert_eq!(back.like, "database connection");
    assert_eq!(back.threshold, 0.5);
}

#[test]
fn test_semantic_filter_in_graph_op() {
    // Test that semantic_filter can be used in GraphOp enum
    let json = r#"{"op":"semantic_filter","like":"error handling","threshold":0.35}"#;
    let op: GraphOp = serde_json::from_str(json).unwrap();
    assert!(matches!(op, GraphOp::SemanticFilter(_)));
}

#[test]
fn test_limit_op() {
    let limit = GraphOp::Limit(LimitOp {
        count: 10,
        offset: 0,
    });
    assert!(matches!(limit, GraphOp::Limit(LimitOp { count: 10, .. })));
}

#[test]
fn test_sort_op() {
    let sort = GraphOp::Sort(SortOp {
        by: SortField::Type,
        direction: SortDirection::Desc,
    });
    assert!(matches!(
        sort,
        GraphOp::Sort(SortOp {
            by: SortField::Type,
            direction: SortDirection::Desc
        })
    ));
}

#[test]
fn test_depth_spec_serde() {
    let single = DepthSpec::Single(3);
    let json = serde_json::to_string(&single).unwrap();
    let back: DepthSpec = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, DepthSpec::Single(3)));

    let range = DepthSpec::Range { min: 1, max: 5 };
    let json2 = serde_json::to_string(&range).unwrap();
    let back2: DepthSpec = serde_json::from_str(&json2).unwrap();
    assert!(matches!(back2, DepthSpec::Range { min: 1, max: 5 }));
}

#[test]
fn test_type_selector_serde() {
    let single = TypeSelector::Single("Function".to_string());
    let json = serde_json::to_string(&single).unwrap();
    let back: TypeSelector = serde_json::from_str(&json).unwrap();
    assert!(back.matches("Function"));

    let or = TypeSelector::Or(vec!["A".to_string(), "B".to_string()]);
    let json2 = serde_json::to_string(&or).unwrap();
    let back2: TypeSelector = serde_json::from_str(&json2).unwrap();
    assert!(back2.matches("A"));
    assert!(back2.matches("B"));
}

#[test]
fn test_name_selector_serde() {
    let exact = NameSelector::Exact("main".to_string());
    let json = serde_json::to_string(&exact).unwrap();
    let back: NameSelector = serde_json::from_str(&json).unwrap();
    assert!(back.matches("main"));

    let glob = NameSelector::Glob("*test*".to_string());
    let json = serde_json::to_string(&glob).unwrap();
    let back: NameSelector = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, NameSelector::Glob(s) if s == "*test*"));
}

#[test]
fn test_name_selector_deserialize() {
    // Test that we can deserialize known name selector variants
    let exact: NameSelector = serde_json::from_str(r#""main""#).unwrap();
    assert!(matches!(exact, NameSelector::Exact(s) if s == "main"));

    let glob: NameSelector = serde_json::from_str(r#"{"glob":"*test*"}"#).unwrap();
    assert!(matches!(glob, NameSelector::Glob(s) if s == "*test*"));

    let starts_with: NameSelector = serde_json::from_str(r#"{"starts_with":"test_"}"#).unwrap();
    assert!(matches!(starts_with, NameSelector::StartsWith(s) if s == "test_"));

    let ends_with: NameSelector = serde_json::from_str(r#"{"endsWith":"_test"}"#).unwrap();
    assert!(matches!(ends_with, NameSelector::EndsWith(s) if s == "_test"));
}

#[test]
fn test_label_selector_serde() {
    let single = LabelSelector::Single("deprecated".to_string());
    let json = serde_json::to_string(&single).unwrap();
    let back: LabelSelector = serde_json::from_str(&json).unwrap();
    assert!(back.matches(Some("deprecated")));

    // Test Or variant round-trip
    let or = LabelSelector::Or(vec!["a".to_string(), "b".to_string()]);
    let json2 = serde_json::to_string(&or).unwrap();
    let back2: LabelSelector = serde_json::from_str(&json2).unwrap();
    assert!(back2.matches(Some("a")));
    assert!(back2.matches(Some("b")));
}

#[test]
fn test_edge_selector_serde() {
    let single = EdgeSelector::Single("Calls".to_string());
    let json = serde_json::to_string(&single).unwrap();
    let back: EdgeSelector = serde_json::from_str(&json).unwrap();
    assert!(back.matches("Calls"));

    // Test Or variant round-trip
    let or = EdgeSelector::Or(vec!["Calls".to_string(), "Imports".to_string()]);
    let json2 = serde_json::to_string(&or).unwrap();
    let back2: EdgeSelector = serde_json::from_str(&json2).unwrap();
    assert!(back2.matches("Calls"));
    assert!(back2.matches("Imports"));
}

#[test]
fn test_query_spec_default() {
    let spec = QuerySpec::default();
    assert!(spec.ops.is_empty());
    assert_eq!(spec.mode, QueryMode::Auto);
    assert!(spec.named.is_none());
}

#[test]
fn test_query_spec_with_named() {
    let spec = QuerySpec::named("get_blast_radius").unwrap();
    assert!(spec.named.is_none()); // named is only set when explicitly provided
    assert!(!spec.ops.is_empty());
}

#[test]
fn test_graph_op_deserialize_find() {
    let json = r#"{"op":"find","type":"Function"}"#;
    let op: GraphOp = serde_json::from_str(json).unwrap();
    assert!(matches!(op, GraphOp::Find(_)));
}

#[test]
fn test_graph_op_deserialize_connect() {
    let json = r#"{"op":"connect","edge":"Calls","direction":"outgoing","depth":2}"#;
    let op: GraphOp = serde_json::from_str(json).unwrap();
    assert!(matches!(op, GraphOp::Connect(_)));
}

#[test]
fn test_graph_op_deserialize_filter() {
    let json = r#"{"op":"filter","type":"Function"}"#;
    let op: GraphOp = serde_json::from_str(json).unwrap();
    assert!(matches!(op, GraphOp::Filter(_)));
}

#[test]
fn test_graph_op_deserialize_limit() {
    let json = r#"{"op":"limit","count":20}"#;
    let op: GraphOp = serde_json::from_str(json).unwrap();
    assert!(matches!(op, GraphOp::Limit(LimitOp { count: 20, .. })));
}

#[test]
fn test_graph_op_deserialize_sort() {
    let json = r#"{"op":"sort","by":"name","direction":"desc"}"#;
    let op: GraphOp = serde_json::from_str(json).unwrap();
    assert!(matches!(
        op,
        GraphOp::Sort(SortOp {
            by: SortField::Name,
            direction: SortDirection::Desc,
            ..
        })
    ));
}

#[test]
fn test_graph_op_deserialize_group() {
    let json = r#"{"op":"group","by":"label"}"#;
    let op: GraphOp = serde_json::from_str(json).unwrap();
    assert!(matches!(op, GraphOp::Group(GroupOp { by: GroupBy::Label })));
}

#[test]
fn test_graph_op_deserialize_unknown() {
    let json = r#"{"op":"unknown"}"#;
    let result: Result<GraphOp, _> = serde_json::from_str(json);
    assert!(result.is_err());
}

#[test]
fn test_find_op_id_filter() {
    let json = r#"{"id":"abc123"}"#;
    let op: FindOp = serde_json::from_str(json).unwrap();
    assert_eq!(op.id, Some("abc123".to_string()));
}

#[test]
fn test_sort_direction_serde() {
    let asc = SortDirection::Asc;
    let json = serde_json::to_string(&asc).unwrap();
    assert_eq!(json, "\"asc\"");
    let back: SortDirection = serde_json::from_str("\"desc\"").unwrap();
    assert_eq!(back, SortDirection::Desc);
}

#[test]
fn test_group_by_serde() {
    let by_type = GroupBy::Type;
    let json = serde_json::to_string(&by_type).unwrap();
    assert_eq!(json, "\"type\"");
    let back: GroupBy = serde_json::from_str("\"label\"").unwrap();
    assert_eq!(back, GroupBy::Label);
}
