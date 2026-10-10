//! Invariants of source extraction, over arbitrary code-like text in every
//! supported language. Real repositories contain truncated, generated,
//! minified, mixed-encoding and merge-conflicted files; extraction runs on all
//! of them inside the indexer, so it must be total and its offsets trustworthy:
//!
//! * never panics;
//! * deterministic (same bytes, same answer: the graph is content-addressed);
//! * every definition's byte range is inside the source and on character
//!   boundaries (callers slice the source with it) and its line range is
//!   ordered and inside the file.
use super::*;
use proptest::prelude::*;
use std::path::Path;

const EXTS: &[&str] = &[
    "rs", "py", "js", "ts", "tsx", "go", "java", "c", "cpp", "cs", "rb", "swift", "kt", "scala",
    "php", "vue", "svelte", "h",
];

/// Fragments that steer tree-sitter into many different parse states.
fn token() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("fn ".to_string()),
        Just("def ".to_string()),
        Just("function ".to_string()),
        Just("class ".to_string()),
        Just("struct ".to_string()),
        Just("impl ".to_string()),
        Just("interface ".to_string()),
        Just("func ".to_string()),
        Just("public ".to_string()),
        Just("{".to_string()),
        Just("}".to_string()),
        Just("(".to_string()),
        Just(")".to_string()),
        Just("[".to_string()),
        Just("]".to_string()),
        Just(";".to_string()),
        Just(":".to_string()),
        Just("\n".to_string()),
        Just("\r\n".to_string()),
        Just("\t".to_string()),
        Just("\"".to_string()),
        Just("'".to_string()),
        Just("`".to_string()),
        Just("//".to_string()),
        Just("/*".to_string()),
        Just("*/".to_string()),
        Just("#".to_string()),
        Just("<<<<<<< HEAD\n".to_string()),
        Just("=======\n".to_string()),
        Just("<script>".to_string()),
        Just("</script>".to_string()),
        Just("<template>".to_string()),
        Just("é".to_string()),
        Just("日本語".to_string()),
        Just("😀".to_string()),
        Just("\u{0}".to_string()),
        "[a-zA-Z_][a-zA-Z0-9_]{0,6}",
        "\\PC{0,4}",
    ]
}

fn source() -> impl Strategy<Value = String> {
    prop::collection::vec(token(), 0..60).prop_map(|t| t.concat())
}

fn check(ext: &str, src: &str) {
    let path = format!("zz/file.{ext}");
    let path = Path::new(&path);
    let defs = extract_definitions(path, src);
    // Deterministic.
    let again = extract_definitions(path, src);
    assert_eq!(
        defs.len(),
        again.len(),
        "{ext}: nondeterministic definition count"
    );
    for (a, b) in defs.iter().zip(&again) {
        assert_eq!(
            (&a.name, a.line_start, a.line_end, a.byte_start, a.byte_end),
            (&b.name, b.line_start, b.line_end, b.byte_start, b.byte_end),
            "{ext}: nondeterministic definition"
        );
    }
    // Offsets are usable by callers that slice the source.
    let line_count = src.split('\n').count() as u32;
    for d in &defs {
        let (s, e) = (d.byte_start as usize, d.byte_end as usize);
        assert!(
            s <= e && e <= src.len(),
            "{ext}: byte range {s}..{e} outside {} bytes",
            src.len()
        );
        // (the vue/svelte script view is a sub-slice, so only check
        // boundaries for plain-language files, where offsets index `src`)
        if ext != "vue" && ext != "svelte" {
            assert!(
                src.is_char_boundary(s),
                "{ext}: start {s} splits a character in {src:?}"
            );
            assert!(
                src.is_char_boundary(e),
                "{ext}: end {e} splits a character in {src:?}"
            );
        }
        assert!(d.line_start <= d.line_end, "{ext}: line range reversed");
        assert!(
            d.line_start < line_count.max(1) + 1,
            "{ext}: start line past EOF"
        );
    }
    // Refs and strings: total, deterministic, lines in range.
    let refs = extract_refs(path, src);
    assert_eq!(
        refs.len(),
        extract_refs(path, src).len(),
        "{ext}: refs nondeterministic"
    );
    for r in &refs {
        assert!(
            r.source_line <= line_count,
            "{ext}: ref line {} past EOF ({line_count})",
            r.source_line
        );
    }
    let strs = extract_strings(path, src);
    for s in &strs {
        assert!(s.source_line <= line_count, "{ext}: string line past EOF");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 120, max_shrink_iters: 400, ..ProptestConfig::default() })]

    #[test]
    fn extraction_is_total_deterministic_and_offsets_are_trustworthy(src in source()) {
        for ext in EXTS {
            check(ext, &src);
        }
    }
}

/// Plain-text and binary-ish inputs under every extension.
#[test]
fn degenerate_inputs_do_not_panic() {
    let big_line = "x".repeat(100_000);
    let deep = format!("{}{}", "(".repeat(5_000), ")".repeat(5_000));
    let blobs = [
        "",
        "\n",
        "\u{0}\u{0}\u{0}",
        &big_line,
        &deep,
        "\u{feff}fn main() {}",
        "fn main() {}\r\rfn x() {}",
    ];
    for ext in EXTS {
        for b in blobs {
            check(ext, b);
        }
    }
}
