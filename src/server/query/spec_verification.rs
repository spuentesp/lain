//! Properties of the query language's selectors and parser.
//!
//! * `Glob` name selection is checked against an obviously-correct reference
//!   (`*` = any run, `?` = any one character, everything else literal). The
//!   previous implementation spliced the pattern into a regex without
//!   escaping it, so `.` matched anything and `(`/`[` silently matched nothing.
//! * The hand-written `NameSelector` serializer/deserializer pair round-trips.
//! * No JSON value, however malformed, panics the parser.
use super::*;
use proptest::prelude::*;

/// The specification of a glob: characters, `*` and `?`, nothing else special.
fn reference_glob(pattern: &[char], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some(('*', rest)) => (0..=text.len()).any(|i| reference_glob(rest, &text[i..])),
        Some(('?', rest)) => !text.is_empty() && reference_glob(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && reference_glob(rest, &text[1..]),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 3000, ..ProptestConfig::default() })]

    /// Regex metacharacters in the alphabet on purpose.
    #[test]
    fn glob_selector_equals_the_reference_glob(
        pattern in "[ab.+()\\[\\]{}^$|\\\\*?]{0,8}",
        text in "[ab.+()\\[\\]{}^$|\\\\]{0,8}",
    ) {
        let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
        prop_assert_eq!(
            NameSelector::Glob(pattern.clone()).matches(&text),
            reference_glob(&p, &t),
            "pattern {:?} text {:?}", pattern, text
        );
    }

    #[test]
    fn name_selector_round_trips_through_json(s in "\\PC{0,12}", which in 0u8..4) {
        let sel = match which {
            0 => NameSelector::Exact(s),
            1 => NameSelector::Glob(s),
            2 => NameSelector::StartsWith(s),
            _ => NameSelector::EndsWith(s),
        };
        let json = serde_json::to_value(&sel).unwrap();
        let back: NameSelector = serde_json::from_value(json.clone()).unwrap();
        prop_assert_eq!(serde_json::to_value(&back).unwrap(), json);
        // And selection behaves identically after the round trip.
        for probe in ["", "a", "abc", "x.y"] {
            prop_assert_eq!(sel.matches(probe), back.matches(probe));
        }
    }

    #[test]
    fn depth_ranges_never_panic_and_single_means_up_to(n in any::<u32>(), lo in any::<u32>(), hi in any::<u32>()) {
        let r = DepthSpec::Single(n).to_range();
        if n == 0 {
            prop_assert_eq!((*r.start(), *r.end()), (0, 0));
        } else {
            prop_assert_eq!((*r.start(), *r.end()), (1, n));
        }
        let _ = DepthSpec::Range { min: lo, max: hi }.to_range();
    }
}

fn arb_json() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i64>().prop_map(|n| serde_json::json!(n)),
        "\\PC{0,8}".prop_map(serde_json::Value::String),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(serde_json::Value::Array),
            prop::collection::vec(
                ("(op|ops|mode|name|type|glob|depth|min|max|limit|n)", inner),
                0..5
            )
            .prop_map(|kv| serde_json::Value::Object(kv.into_iter().collect())),
        ]
    })
}

proptest! {
    /// Parsing arbitrary JSON is total: `Ok` or `Err`, never a panic.
    #[test]
    fn parsing_arbitrary_json_never_panics(v in arb_json()) {
        let _ = serde_json::from_value::<QuerySpec>(v.clone());
        let _ = serde_json::from_value::<GraphOp>(v.clone());
        let _ = serde_json::from_value::<NameSelector>(v.clone());
        let _ = serde_json::from_value::<DepthSpec>(v);
    }

    /// A spec built from valid parts survives a JSON round trip unchanged.
    #[test]
    fn specs_round_trip(
        names in prop::collection::vec("[a-z_]{1,8}", 0..4),
        limit in 1usize..50,
    ) {
        let mut ops: Vec<GraphOp> = names
            .iter()
            .map(|n| GraphOp::Find(FindOp::new().name(n.clone())))
            .collect();
        ops.push(GraphOp::Limit(LimitOp { count: limit, offset: 0 }));
        let spec = QuerySpec::new(ops);
        let json = serde_json::to_value(&spec).unwrap();
        let back: QuerySpec = serde_json::from_value(json.clone()).unwrap();
        prop_assert_eq!(serde_json::to_value(&back).unwrap(), json);
    }
}
