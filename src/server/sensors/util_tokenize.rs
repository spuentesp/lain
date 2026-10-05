//! Hand-rolled tokenizer plumbing shared by SQL / proto /
//! GraphQL sensors.
//!
//! Each sensor provides its own grammar (the protocol-specific
//! fixtures per spec §7 / §8.2 / §8.3) but the comment-stripping,
//! brace-balancing, string-literal extraction, and keyword-matching
//! scaffolding is shared here.
//!
//! ## Why a hand-rolled tokenizer
//!
//! The protocol-sensor design (`docs/CONTRIBUTING_AGENTS.md` §
//! "sensor pattern") rules out parser-crate dependencies:
//! `sqlparser-rs`, `graphql-parser`, `tree-sitter-proto` were each
//! considered and rejected per the scorecard "no new runtime deps"
//! rule. Each protocol's parser is therefore a per-sensor
//! hand-rolled byte-walk, but the scaffolding below it is shared.

// ─── Comment-stripping ─────────────────────────────────────────────────

/// Which comment syntax `strip_comments` should recognise.
///
/// `CStyle` matches `//` and `/* … */`, with `"…"` string literals
/// preserved verbatim (newlines inside both kept for line-number
/// math). Used by the proto sensor.
///
/// `HashBlockString` matches `#` line comments and `""" … """`
/// block strings. Used by the SDL GraphQL provider and the
/// GraphQL consumer sensors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentSyntax {
    CStyle,
    HashBlockString,
}

/// Strip comments from `src`. The comment text is removed; the newlines in
/// and after it are kept, so line numbers computed from the result still match
/// the source. Everything outside comments, non-ASCII text included, is copied
/// byte for byte.
pub fn strip_comments(src: &str, syntax: CommentSyntax) -> String {
    match syntax {
        CommentSyntax::CStyle => strip_c_style(src),
        CommentSyntax::HashBlockString => strip_hash_block(src),
    }
}

/// The stripped bytes as text. Only ASCII-delimited regions are ever cut, so
/// the copied bytes stay whole UTF-8 sequences; the lossy fallback is a
/// safety net, not an expected path. (Each byte used to be pushed as
/// `bytes[i] as char`, which widens every non-ASCII byte into two UTF-8
/// bytes: `é` became `Ã©`, corrupting extracted string literals.)
fn into_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn strip_c_style(input: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    out.push(b'\n');
                }
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                // Unterminated comment: the loop above stops one byte short
                // of the end, so keep any newline in what remains.
                while i < bytes.len() {
                    if bytes[i] == b'\n' {
                        out.push(b'\n');
                    }
                    i += 1;
                }
            }
            continue;
        }
        if bytes[i] == b'"' {
            out.push(b'"');
            i += 1;
            // The bytes are copied as they are: a newline inside a string
            // literal is one newline in the output. (It used to be pushed
            // explicitly *and* copied, doubling every line break in a string
            // and shifting the line number of everything declared after it.)
            while i < bytes.len() && bytes[i] != b'"' {
                out.push(bytes[i]);
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    out.push(bytes[i]);
                }
                i += 1;
            }
            if i < bytes.len() {
                out.push(b'"');
                i += 1;
            }
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    into_string(out)
}

fn strip_hash_block(input: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 2 < bytes.len() && bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"' {
            out.push(b'"');
            out.push(b'"');
            out.push(b'"');
            i += 3;
            while i + 2 < bytes.len()
                && !(bytes[i] == b'"' && bytes[i + 1] == b'"' && bytes[i + 2] == b'"')
            {
                // Copied as is; see the string branch of `strip_c_style`.
                out.push(bytes[i]);
                i += 1;
            }
            if i + 2 < bytes.len() {
                out.push(b'"');
                out.push(b'"');
                out.push(b'"');
                i += 3;
            } else {
                // Unterminated block string: keep the rest as written.
                out.extend_from_slice(&bytes[i..]);
                i = bytes.len();
            }
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    into_string(out)
}

// ─── Brace balancing ───────────────────────────────────────────────────

/// Walk a brace-balanced region whose opening `{` is at
/// `src.as_bytes()[start]` and return the byte index of the
/// matching close `}`. Returns `None` when the braces are
/// unbalanced (caller may opt to fall through).
///
/// The walk does not skip string literals; the only callers are
/// the proto / GraphQL sensors which feed it already-stripped
/// sources (`strip_comments` runs first). A caller that needs
/// string-aware balance should layer that on top.
pub fn find_matching_close(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if start >= bytes.len() || bytes[start] != b'{' {
        return None;
    }
    let mut depth: u32 = 1;
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            depth += 1;
        } else if bytes[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

// ─── String literals ───────────────────────────────────────────────────

/// Extract the first quoted-string literal starting at `src[start]`.
/// Recognises `"`, `'`, and backtick quotes; respects backslash
/// escapes so a literal containing an escaped quote is not split.
/// Triple-quoted forms (`"""..."""` / `'''...'''`) are also
/// recognised — common in proto / GraphQL SDL / Python docstrings
/// and the SQL string-literal fixture. Leading whitespace before
/// the literal is skipped (the form `func( "literal" )` is common
/// in real code).
///
/// Returns `(end, literal)` where `end` is the index one past the
/// closing quote (or `src.len()` on unterminated input — callers
/// that need a strict check should compare `end` against the byte
/// length). For triple-quoted forms `end` is one past the closing
/// triple-quote and the literal preserves internal newlines
/// verbatim.
pub fn extract_string_literal(src: &str, start: usize) -> Option<(usize, String)> {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let quote = bytes[i];
    if quote != b'"' && quote != b'\'' && quote != b'`' {
        return None;
    }
    // Triple-quoted form: `"""..."""` / `'''...'''`. The body
    // extends to the matching triple-quote, allowing embedded
    // newlines and unescaped single quotes of the same kind.
    if i + 2 < bytes.len() && bytes[i + 1] == quote && bytes[i + 2] == quote {
        let mut j = i + 3;
        while j + 2 < bytes.len() {
            if bytes[j] == quote && bytes[j + 1] == quote && bytes[j + 2] == quote {
                let literal = String::from_utf8_lossy(&bytes[i + 3..j]).to_string();
                return Some((j + 3, literal));
            }
            j += 1;
        }
        return None;
    }
    let mut end: Option<usize> = None;
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b'\\' && j + 1 < bytes.len() {
            j += 2;
            continue;
        }
        if bytes[j] == quote {
            end = Some(j);
            break;
        }
        j += 1;
    }
    let end = end?;
    let mut literal = String::from_utf8_lossy(&bytes[i + 1..end]).to_string();
    if quote == b'"' {
        literal = literal.replace("\"\"", "\"");
    } else if quote == b'\'' {
        literal = literal.replace("''", "'");
    }
    Some((end + 1, literal))
}

// ─── Keyword matching ──────────────────────────────────────────────────

/// True iff `word` appears as a whole token at `src[start..]`.
/// A whole-token match requires the byte before (if any) and the
/// byte after (if any) to be neither ASCII alphanumeric nor `_`.
///
/// Used by the proto and GraphQL SDL sensors to recognise
/// `service`, `rpc`, `returns`, `type` without false-matching
/// `services`, `atype`, etc.
pub fn starts_with_keyword(src: &str, start: usize, word: &str) -> bool {
    let bytes = src.as_bytes();
    let kw = word.as_bytes();
    if start + kw.len() > bytes.len() {
        return false;
    }
    if &bytes[start..start + kw.len()] != kw {
        return false;
    }
    let before_ok = start == 0 || is_word_boundary(bytes[start - 1]);
    let after_idx = start + kw.len();
    let after_ok = after_idx >= bytes.len() || is_word_boundary(bytes[after_idx]);
    before_ok && after_ok
}

fn is_word_boundary(b: u8) -> bool {
    !((b as char).is_ascii_alphanumeric() || b == b'_')
}

// ─── Line iteration ───────────────────────────────────────────────────

/// Yield `(line_no, line)` for every line in `src` matching
/// `predicate`. `line_no` is 1-based to match the `SourceSite::line`
/// convention every sensor uses for error reporting.
///
/// This is the per-extension list-of-needles + line-by-line walk the
/// SQL, gRPC, GraphQL, and event sensors all need. Each call site
/// still owns the per-line extraction logic — the helper only
/// handles the boilerplate of enumerating lines, applying the
/// predicate, and computing the 1-based line number.
pub fn lines_matching_pattern<'a, F>(
    src: &'a str,
    predicate: F,
) -> impl Iterator<Item = (usize, &'a str)> + 'a
where
    F: Fn(&str) -> bool + 'a,
{
    src.lines().enumerate().filter_map(move |(idx, line)| {
        if predicate(line) {
            Some((idx + 1, line))
        } else {
            None
        }
    })
}

// ─── Call-site detection ──────────────────────────────────────────────

/// One call-site match found by [`detect_method_calls`]. The
/// `receiver` and `method` are borrows into the source string
/// (callers can `.to_string()` to take ownership when emitting a
/// record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedCall<'a> {
    pub receiver: &'a str,
    pub method: &'a str,
    pub line_no: u32,
}

/// Walk `src` line-by-line and emit one [`DetectedCall`] for every
/// `<receiver>.<method>(` shape where `receiver_predicate(receiver)`
/// and `method_predicate(method)` both hold. The receiver is
/// required to be a single identifier (no dots); the method is the
/// bare identifier between the dot and the `(`.
///
/// This is the per-language line-walk + receiver.method() pattern
/// the gRPC consumer sensor's four `detect_*_stub_calls` functions
/// all need. Each caller still owns the post-processing (suffix
/// stripping, channel-host projection, package inference) — the
/// helper handles the line scan, the per-line shape, and the
/// predicate filter.
pub fn detect_method_calls<F, G>(
    src: &str,
    receiver_predicate: F,
    method_predicate: G,
) -> Vec<DetectedCall<'_>>
where
    F: Fn(&str) -> bool,
    G: Fn(&str) -> bool,
{
    let mut out: Vec<DetectedCall> = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        let line_no = (idx as u32) + 1;
        for (rcv_start, rcv_end, meth_start, meth_end) in find_receiver_method_shapes(line) {
            let receiver = &line[rcv_start..rcv_end];
            let method = &line[meth_start..meth_end];
            if !receiver_predicate(receiver) || !method_predicate(method) {
                continue;
            }
            out.push(DetectedCall {
                receiver,
                method,
                line_no,
            });
        }
    }
    out
}

/// Find every `<receiver>.<method>(` shape on `line`. Returns
/// `(rcv_start, rcv_end, meth_start, meth_end)` byte offsets into
/// `line`. The receiver is the run of identifier characters
/// immediately before `.`; the method is the run of identifier
/// characters between `.` and `(`. Chained calls (`.method(` after
/// the dot) are skipped.
fn find_receiver_method_shapes(line: &str) -> Vec<(usize, usize, usize, usize)> {
    let mut out: Vec<(usize, usize, usize, usize)> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Look for `.` followed by an identifier followed by `(`.
        if bytes[i] != b'.' || i + 1 >= bytes.len() {
            i += 1;
            continue;
        }
        // Method identifier: starts at i+1, ends before `(` or any
        // non-identifier char.
        let meth_start = i + 1;
        if !is_ident_start(bytes[meth_start]) {
            i += 1;
            continue;
        }
        let mut meth_end = meth_start;
        while meth_end < bytes.len() && is_ident_continue(bytes[meth_end]) {
            meth_end += 1;
        }
        if meth_end >= bytes.len() || bytes[meth_end] != b'(' {
            i += 1;
            continue;
        }
        // Receiver identifier: walk back from `.` to the previous
        // non-identifier char.
        let rcv_end = i;
        if rcv_end == 0 {
            i += 1;
            continue;
        }
        let mut rcv_start = rcv_end;
        while rcv_start > 0 && is_ident_continue(bytes[rcv_start - 1]) {
            rcv_start -= 1;
        }
        if rcv_start == rcv_end {
            i += 1;
            continue;
        }
        out.push((rcv_start, rcv_end, meth_start, meth_end));
        i = meth_end;
    }
    out
}

fn is_ident_start(b: u8) -> bool {
    (b as char).is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    (b as char).is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_c_style_preserves_line_numbers() {
        let src = "service A {\n  // line 2\n  rpc M(R) returns (RR);\n  /* b1\nb2 */\n}\n";
        let out = strip_comments(src, CommentSyntax::CStyle);
        let line_count = out.lines().count();
        assert_eq!(
            line_count, 6,
            "every newline in the source survives the stripper"
        );
    }

    #[test]
    fn strip_hash_block_handles_triple_double_block_strings() {
        let src = "type Query {\n  # comment\n  hello: String\n}\n";
        let out = strip_comments(src, CommentSyntax::HashBlockString);
        assert!(out.contains("type Query"));
        assert!(!out.contains('#'));
    }

    #[test]
    fn find_matching_close_finds_the_brace() {
        let src = "x { a { c } d } z";
        // Outer `{` at index 2 closes at the matching `}` at index 14.
        assert_eq!(find_matching_close(src, 2), Some(14));
    }

    #[test]
    fn find_matching_close_returns_none_when_unbalanced() {
        let src = "x { a { c } d";
        assert_eq!(find_matching_close(src, 2), None);
    }

    #[test]
    fn extract_string_literal_handles_double_single_backtick() {
        assert_eq!(
            extract_string_literal("\"SELECT 1\"", 0),
            Some(("\"SELECT 1\"".len(), "SELECT 1".to_string()))
        );
        assert_eq!(
            extract_string_literal("'SELECT 1'", 0),
            Some(("'SELECT 1'".len(), "SELECT 1".to_string()))
        );
        assert_eq!(
            extract_string_literal("`SELECT 1`", 0),
            Some(("`SELECT 1`".len(), "SELECT 1".to_string()))
        );
    }

    #[test]
    fn extract_string_literal_handles_triple_double_block() {
        // Triple-double-quoted form preserves newlines.
        let src = "\"\"\"\nSELECT *\nFROM orders\n\"\"\"";
        let (end, literal) = extract_string_literal(src, 0).expect("triple-quoted");
        assert_eq!(end, src.len());
        assert_eq!(literal, "\nSELECT *\nFROM orders\n");
    }

    #[test]
    fn extract_string_literal_handles_triple_single_block() {
        let src = "'''SELECT 1'''";
        let (end, literal) = extract_string_literal(src, 0).expect("triple-single");
        assert_eq!(end, src.len());
        assert_eq!(literal, "SELECT 1");
    }

    #[test]
    fn extract_string_literal_unterminated_triple_returns_none() {
        assert!(extract_string_literal("\"\"\"unterminated", 0).is_none());
    }

    #[test]
    fn starts_with_keyword_requires_word_boundary() {
        assert!(starts_with_keyword("service Foo", 0, "service"));
        assert!(!starts_with_keyword("services Foo", 0, "service"));
        assert!(!starts_with_keyword("aservice Foo", 0, "service"));
        assert!(starts_with_keyword("type Query {", 0, "type"));
    }

    #[test]
    fn lines_matching_pattern_yields_one_based_line_numbers() {
        let src = "alpha\nbeta gamma\nalpha again\n";
        let hits: Vec<(usize, &str)> =
            lines_matching_pattern(src, |l| l.contains("alpha")).collect();
        assert_eq!(hits, vec![(1, "alpha"), (3, "alpha again")]);
    }

    #[test]
    fn lines_matching_pattern_returns_empty_when_no_match() {
        let src = "foo\nbar\n";
        let hits: Vec<(usize, &str)> = lines_matching_pattern(src, |l| l.contains("zzz")).collect();
        assert!(hits.is_empty());
    }

    #[test]
    fn detect_method_calls_finds_receiver_method_pairs() {
        let src = "\
ordersClient.Get(ctx, req)
ordersStub.GetOrder(request)
not a call
";
        let calls: Vec<String> = detect_method_calls(
            src,
            |rcv| rcv.ends_with("Client") || rcv.ends_with("Stub"),
            |meth| !meth.is_empty(),
        )
        .iter()
        .map(|c| format!("{}.{}", c.receiver, c.method))
        .collect();
        assert_eq!(calls, vec!["ordersClient.Get", "ordersStub.GetOrder"]);
    }

    #[test]
    fn detect_method_calls_reports_line_numbers() {
        let src = "\nfirst\nordersClient.GetOrder(req)\n";
        let calls = detect_method_calls(src, |_| true, |_| true);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].line_no, 3);
        assert_eq!(calls[0].receiver, "ordersClient");
        assert_eq!(calls[0].method, "GetOrder");
    }

    #[test]
    fn detect_method_calls_skips_chained_calls() {
        // `obj.a.b()` should NOT emit `a.b` (the receiver `a` is
        // itself a method call chain).
        let src = "obj.a.b()\n";
        let calls = detect_method_calls(src, |_| true, |_| true);
        // The outer `.b(` is the only receiver.method( match —
        // receiver is `a`, method is `b`. The inner `.a(` is
        // ignored because `obj` isn't followed by `(`.
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].receiver, "a");
        assert_eq!(calls[0].method, "b");
    }
}

#[cfg(test)]
#[path = "util_tokenize_verification.rs"]
mod verification;
