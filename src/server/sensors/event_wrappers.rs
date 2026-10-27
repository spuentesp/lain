//! Wrapper call-site expansion for the event sensor (§6.7).
//!
//! A producer whose topic is a parameter of the function around it is a
//! wrapper: `def publish(queue_name, payload): c.send_message(QueueUrl=queue_name, ...)`.
//! The SDK call cannot name its topic, so the sensor records the wrapper
//! here and then reads its call sites in the same repo. A call such as
//! `publish("orders-created", order)` whose argument in the parameter's
//! position is a string literal becomes a `Produces` site for that literal.
//!
//! Scope, deliberately narrow:
//! - Python `def` and TypeScript/JavaScript `function` headers only.
//! - One hop. A wrapper's own body is not followed into another wrapper.
//! - A wrapper name must be defined once in the repo. Two definitions of the
//!   same name are dropped, not guessed between.
//! - `self` and `cls` are not counted, so `obj.publish("x")` lines up with
//!   `def publish(self, queue_name)`.
//! - Calls are matched on one line by bare name and position, or by keyword
//!   name. Multi-line calls and `**kwargs` forwarding are not expanded.
//! - Matching is by name, not by the callee's module. A `publish("x")` on an
//!   unrelated object is read as a call to the wrapper, so wrapper names
//!   should be specific.

use crate::server::sensors::util::{self, Lang};
use crate::server::sensors::util_tokenize::strip_line_comment;
use std::collections::BTreeMap;

/// One wrapper: a function whose parameter `param_name` (at position
/// `param_index`, not counting `self`/`cls`) feeds a producer's topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapperDef {
    pub lang: Lang,
    pub broker: String,
    pub param_name: String,
    pub param_index: usize,
}

/// Wrappers keyed by function name, each defined exactly once in the repo.
pub type WrapperIndex = BTreeMap<String, WrapperDef>;

/// The wrappers defined in one file, as `(function name, definition)` pairs.
/// `producers` must already be filtered to the ones that apply to `content`.
pub fn wrappers_in_file(
    content: &str,
    lang: Lang,
    producers: &[util::CompiledIdiom],
    default_broker: &str,
) -> Vec<(String, WrapperDef)> {
    let lines: Vec<&str> = content.lines().collect();
    let mut out: Vec<(String, WrapperDef)> = Vec::new();
    for (idx, raw) in lines.iter().enumerate() {
        let line = strip_line_comment(raw);
        for m in util::walk_idioms(producers, line, idx as u32 + 1) {
            let Some(identifier) = m.identifier.as_deref() else {
                continue;
            };
            let Some((name, params)) = enclosing_header(&lines, idx, lang) else {
                continue;
            };
            let Some(param_index) = params.iter().position(|p| p == identifier) else {
                continue;
            };
            let def = WrapperDef {
                lang,
                broker: m
                    .broker
                    .clone()
                    .unwrap_or_else(|| default_broker.to_string()),
                param_name: identifier.to_string(),
                param_index,
            };
            if !out.iter().any(|(n, d)| *n == name && *d == def) {
                out.push((name, def));
            }
        }
    }
    out
}

/// The `(broker, topic)` pairs that the wrapper call sites on `line` resolve
/// to. A function header line is never a call site.
pub fn wrapper_calls(line: &str, lang: Lang, wrappers: &WrapperIndex) -> Vec<(String, String)> {
    if header_name(line.trim_start(), lang).is_some() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (name, def) in wrappers {
        if def.lang != lang {
            continue;
        }
        for (pos, _) in line.match_indices(name.as_str()) {
            let preceded_by_word = line[..pos]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if preceded_by_word {
                continue;
            }
            let Some(rest) = line[pos + name.len()..].trim_start().strip_prefix('(') else {
                continue;
            };
            let Some(args) = call_args(rest) else {
                continue;
            };
            if let Some(topic) = literal_arg(&args, def) {
                out.push((def.broker.clone(), topic));
            }
        }
    }
    out
}

/// The nearest less-indented line above `idx`, when it is a function header.
/// A less-indented line that is not a header (a class, a module statement)
/// means the line is not inside a function, so there is no wrapper.
fn enclosing_header(lines: &[&str], idx: usize, lang: Lang) -> Option<(String, Vec<String>)> {
    let own = indent_of(lines[idx]);
    for header in lines[..idx].iter().rev() {
        let trimmed = header.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        if indent_of(header) >= own {
            continue;
        }
        let name = header_name(trimmed, lang)?;
        return Some((name, header_params(trimmed)));
    }
    None
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The function name in a `def`/`async def` (Python) or `function`/
/// `export function`/`async function` (TS/JS) header line.
fn header_name(trimmed: &str, lang: Lang) -> Option<String> {
    let name_of = |rest: &str| {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        (!name.is_empty()).then_some(name)
    };
    match lang {
        Lang::Python => {
            let rest = trimmed
                .strip_prefix("async ")
                .unwrap_or(trimmed)
                .strip_prefix("def ")?;
            name_of(rest.trim_start())
        }
        Lang::TsJs => {
            let rest = trimmed.strip_prefix("export ").unwrap_or(trimmed);
            let rest = rest.strip_prefix("async ").unwrap_or(rest);
            name_of(rest.strip_prefix("function ")?.trim_start())
        }
        _ => None,
    }
}

/// Parameter names of a single-line header, with `self`/`cls`, defaults,
/// annotations and star-prefixes removed.
fn header_params(trimmed: &str) -> Vec<String> {
    let Some(open) = trimmed.find('(') else {
        return Vec::new();
    };
    let Some(args) = call_args(&trimmed[open + 1..]) else {
        return Vec::new();
    };
    args.iter()
        .filter_map(|arg| {
            let arg = arg.trim().trim_start_matches("...").trim_start_matches('*');
            let name = arg.split([':', '=']).next()?.trim();
            (is_ident(name) && name != "self" && name != "cls").then(|| name.to_string())
        })
        .collect()
}

/// The arguments of a call whose `(` has already been consumed. Splits on
/// top-level commas and ignores commas inside quotes or brackets. Returns
/// `None` when the call does not close on this line.
fn call_args(text: &str) -> Option<Vec<&str>> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\u{22}' | '\u{27}' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => depth -= 1,
            ')' => {
                args.push(&text[start..i]);
                return Some(args.into_iter().filter(|a| !a.trim().is_empty()).collect());
            }
            ',' if depth == 0 => {
                args.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    None
}

/// The literal value of the argument that feeds the wrapper's topic: the
/// positional argument at `param_index`, or the keyword argument named
/// `param_name`.
fn literal_arg(args: &[&str], def: &WrapperDef) -> Option<String> {
    let mut position = 0;
    for arg in args {
        if let Some((key, value)) = keyword_arg(arg) {
            if key == def.param_name {
                return literal_value(value);
            }
            continue;
        }
        if position == def.param_index {
            return literal_value(arg);
        }
        position += 1;
    }
    None
}

/// `key=value` where `key` is an identifier. `==` is not a keyword argument.
fn keyword_arg(arg: &str) -> Option<(&str, &str)> {
    let (key, value) = arg.split_once('=')?;
    if value.starts_with('=') {
        return None;
    }
    let key = key.trim();
    is_ident(key).then_some((key, value.trim()))
}

/// A plain quoted string literal. An f-string, a concatenation, or an
/// escaped string is not a literal and resolves to nothing.
fn literal_value(arg: &str) -> Option<String> {
    let arg = arg.trim();
    let quote = arg
        .chars()
        .next()
        .filter(|c| *c == '\u{22}' || *c == '\u{27}')?;
    let inner = arg.strip_prefix(quote)?.strip_suffix(quote)?;
    (!inner.is_empty() && !inner.contains(quote) && !inner.contains('\\'))
        .then(|| inner.to_string())
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python_def(name: &str, param_name: &str, param_index: usize) -> WrapperIndex {
        let mut index = WrapperIndex::new();
        index.insert(
            name.to_string(),
            WrapperDef {
                lang: Lang::Python,
                broker: "sqs".into(),
                param_name: param_name.into(),
                param_index,
            },
        );
        index
    }

    #[test]
    fn call_args_splits_top_level_commas_and_respects_quotes() {
        let args = call_args("\"a,b\", f(x, y), k=[1, 2]) rest").unwrap();
        assert_eq!(args, vec!["\"a,b\"", " f(x, y)", " k=[1, 2]"]);
    }

    #[test]
    fn call_args_is_none_when_the_call_does_not_close() {
        assert!(call_args("\"x\", y").is_none());
    }

    #[test]
    fn header_name_reads_python_and_ts_headers() {
        assert_eq!(
            header_name("def publish(queue_name):", Lang::Python).as_deref(),
            Some("publish")
        );
        assert_eq!(
            header_name("async def publish(q):", Lang::Python).as_deref(),
            Some("publish")
        );
        assert_eq!(
            header_name("export async function publish(q) {", Lang::TsJs).as_deref(),
            Some("publish")
        );
        assert_eq!(header_name("publish(q);", Lang::Python), None);
        assert_eq!(header_name("def publish(q):", Lang::Go), None);
    }

    #[test]
    fn header_params_drop_self_defaults_and_annotations() {
        assert_eq!(
            header_params("def publish(self, queue_name: str, payload=None):"),
            vec!["queue_name".to_string(), "payload".to_string()]
        );
        assert_eq!(
            header_params("function publish(queue: string, body: string) {"),
            vec!["queue".to_string(), "body".to_string()]
        );
    }

    #[test]
    fn enclosing_header_finds_the_def_above_an_indented_producer() {
        let lines = [
            "def publish(queue_name, payload):",
            "    c.send_message(QueueUrl=queue_name)",
        ];
        let (name, params) = enclosing_header(&lines, 1, Lang::Python).unwrap();
        assert_eq!(name, "publish");
        assert_eq!(params, vec!["queue_name", "payload"]);
    }

    #[test]
    fn enclosing_header_is_none_at_module_level() {
        let lines = ["import boto3", "c.send_message(QueueUrl=queue_name)"];
        assert!(enclosing_header(&lines, 1, Lang::Python).is_none());
    }

    #[test]
    fn literal_call_site_resolves_by_position() {
        let index = python_def("publish", "queue_name", 0);
        assert_eq!(
            wrapper_calls("publish(\"orders-created\", order)", Lang::Python, &index),
            vec![("sqs".to_string(), "orders-created".to_string())]
        );
    }

    #[test]
    fn keyword_call_site_resolves_by_param_name() {
        let index = python_def("publish", "queue_name", 0);
        assert_eq!(
            wrapper_calls(
                "publish(payload=p, queue_name='orders-created')",
                Lang::Python,
                &index
            ),
            vec![("sqs".to_string(), "orders-created".to_string())]
        );
    }

    #[test]
    fn method_call_lines_up_with_self_skipped() {
        let index = python_def("publish", "queue_name", 0);
        assert_eq!(
            wrapper_calls("bus.publish(\"orders\", p)", Lang::Python, &index).len(),
            1
        );
    }

    #[test]
    fn non_literal_and_f_string_arguments_resolve_to_nothing() {
        let index = python_def("publish", "queue_name", 0);
        assert!(wrapper_calls("publish(topic_name, p)", Lang::Python, &index).is_empty());
        assert!(wrapper_calls("publish(f\"orders-{x}\", p)", Lang::Python, &index).is_empty());
        assert!(wrapper_calls("publish(\"\", p)", Lang::Python, &index).is_empty());
    }

    #[test]
    fn name_must_match_a_whole_word() {
        let index = python_def("publish", "queue_name", 0);
        assert!(wrapper_calls("republish(\"orders\")", Lang::Python, &index).is_empty());
        assert!(wrapper_calls("publish_all(\"orders\")", Lang::Python, &index).is_empty());
    }

    #[test]
    fn a_def_header_with_a_default_is_not_a_call_site() {
        let index = python_def("publish", "queue_name", 0);
        assert!(
            wrapper_calls("def publish(queue_name='orders'):", Lang::Python, &index).is_empty()
        );
    }

    #[test]
    fn a_wrapper_does_not_resolve_calls_in_another_language() {
        let index = python_def("publish", "queue_name", 0);
        assert!(wrapper_calls("publish(\"orders\")", Lang::Go, &index).is_empty());
    }
}
