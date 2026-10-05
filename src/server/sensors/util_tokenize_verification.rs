//! Offset and length invariants of the hand-rolled tokenizer helpers, over
//! arbitrary text including multi-byte characters (the helpers walk bytes).
use super::*;
use proptest::prelude::*;

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("{".to_string()),
            Just("}".to_string()),
            Just("\"".to_string()),
            Just("'".to_string()),
            Just("//".to_string()),
            Just("/*".to_string()),
            Just("*/".to_string()),
            Just("#".to_string()),
            Just("\"\"\"".to_string()),
            Just("\n".to_string()),
            Just("\\".to_string()),
            Just("é".to_string()),
            Just("日本".to_string()),
            Just("😀".to_string()),
            "[a-z ]{0,5}",
        ],
        0..40,
    )
    .prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    /// The contract callers rely on: line structure survives (so line numbers
    /// computed on the result match the source) and nothing is ever added.
    #[test]
    fn strip_comments_keeps_line_structure_and_never_grows(src in text(), hash in any::<bool>()) {
        let syntax = if hash { CommentSyntax::HashBlockString } else { CommentSyntax::CStyle };
        let out = strip_comments(&src, syntax);
        prop_assert!(out.len() <= src.len(), "grew: {:?} -> {:?}", src, out);
        prop_assert_eq!(out.matches('\n').count(), src.matches('\n').count(), "line count changed");
    }

    /// Text with no comment syntax passes through untouched. Non-ASCII text
    /// used to be mangled byte by byte (`é` -> `Ã©`).
    #[test]
    fn comment_free_text_is_identity(
        chars in prop::collection::vec(
            prop::sample::select(vec!['a', 'b', '"', '\n', '\\', 'é', '日', '😀', ' ', '{', '}']),
            0..40,
        )
    ) {
        let src: String = chars.into_iter().collect();
        prop_assert_eq!(strip_comments(&src, CommentSyntax::CStyle), src.clone());
        prop_assert_eq!(strip_comments(&src, CommentSyntax::HashBlockString), src);
    }

    /// Never panics (a slice at a non-boundary would), and the reported end
    /// is inside the source and on a character boundary.
    #[test]
    fn extract_string_literal_offsets_are_usable(src in text(), start in 0usize..80) {
        if let Some((end, value)) = extract_string_literal(&src, start) {
            prop_assert!(end <= src.len(), "end {} past {}", end, src.len());
            prop_assert!(src.is_char_boundary(end), "end {} splits a character in {:?}", end, src);
            let _ = value;
        }
    }

    #[test]
    fn starts_with_keyword_never_panics(src in text(), start in 0usize..80, w in "[a-z]{1,6}") {
        let _ = starts_with_keyword(&src, start, &w);
    }
}

/// The user-visible effect of the doubled newline: every GraphQL type declared
/// after a multi-line description reported a line number too high.
#[test]
fn a_declaration_after_a_multiline_description_keeps_its_line_number() {
    let src = "\"\"\"\nline one\nline two\n\"\"\"\ntype Query { a: Int }\n";
    let out = strip_comments(src, CommentSyntax::HashBlockString);
    let line_of = |s: &str| s[..s.find("type Query").unwrap()].matches('\n').count();
    assert_eq!(line_of(&out), line_of(src));
    assert_eq!(line_of(src), 4);

    let proto =
        "message A {\n  string s = 1; // c\n  string t = \"two\nlines\";\n}\nservice S {}\n";
    let out = strip_comments(proto, CommentSyntax::CStyle);
    let line_of = |s: &str| s[..s.find("service S").unwrap()].matches('\n').count();
    assert_eq!(line_of(&out), line_of(proto));
}

#[test]
fn non_ascii_string_literals_survive_stripping() {
    let src = "option x = \"café 日本\"; // trailing\n";
    let out = strip_comments(src, CommentSyntax::CStyle);
    assert!(out.contains("\"café 日本\""), "mangled: {out:?}");
}
