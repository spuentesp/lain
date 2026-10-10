//! JSON-pointer → line index for OpenAPI / Swagger specs (§6.4).
//!
//! `serde` keeps no source spans, so the OpenAPI sensor cannot ask
//! "where in the spec is this property defined?". This module builds
//! a side index from the raw spec text — block YAML mappings by
//! indentation, JSON by tokenizing object keys. Flow-style YAML falls
//! back to the nearest ancestor with a known line, because the inner
//! braces carry no line-of-origin information the scanner can recover.
//!
//! Keys are JSON-pointer-like paths of property names: a request body
//! for `GET /api/orders/{}` lives at
//! `["paths", "/api/orders/{id}", "get", "responses", "200", "content",
//! "application/json", "schema"]`. Lookups return the line where the
//! outermost known prefix was defined; that is the line a property
//! argument (`Schema`, `Field`) is reported on.

use std::collections::BTreeMap;

/// A side index from a JSON-pointer-like path to its line number.
///
/// Built by [`LineIndex::build`]; consulted by the schema flattener to
/// pick a line number for every `Schema` and `Field` node it emits
/// (§6.4 "Line numbers"). The lookup is "longest known prefix": if
/// the requested path is not in the index but the closest outer
/// property is, the outer one's line is returned. That handles
/// flow-style fallback — a flow-mapped object's children have no line
/// of their own, so the lookup walks back to the nearest block
/// ancestor.
#[derive(Debug, Default, Clone)]
pub struct LineIndex {
    map: BTreeMap<Vec<String>, u32>,
    /// Original source text, one line per entry. Kept so parameter
    /// reverse-lookups (`name → line`) can re-read a line and check
    /// its value. Lines are stored verbatim, with leading whitespace
    /// preserved so callers can re-detect YAML structure if needed.
    source: Vec<String>,
}

impl LineIndex {
    /// Build a line index for `content`. The format is autodetected:
    /// a leading `{` makes it JSON, anything else is treated as block
    /// YAML. Flow-style YAML is recognized and its inner keys fall
    /// back to the nearest block ancestor via `lookup`.
    pub fn build(content: &str) -> Self {
        let trimmed = content.trim_start();
        if trimmed.starts_with('{') {
            build_json(content)
        } else {
            build_yaml(content)
        }
    }

    /// Find the line where any prefix of `path` was defined. Returns
    /// the longest known prefix's line, or `None` if no ancestor is
    /// indexed (a spec with no top-level properties — should not
    /// happen for OpenAPI, which always has `openapi`/`swagger` at
    /// the root).
    pub fn lookup(&self, path: &[String]) -> Option<u32> {
        for i in (0..=path.len()).rev() {
            if let Some(&line) = self.map.get(&path[..i]) {
                return Some(line);
            }
        }
        None
    }

    /// Same as [`Self::lookup`] but accepts a `JsonPath` directly.
    /// Exists so the schema flattener does not have to convert
    /// `JsonPath` into `Vec<String>` on every call.
    pub fn lookup_path(&self, path: &crate::federation::contracts::model::JsonPath) -> Option<u32> {
        let parts: Vec<String> = path.0.iter().map(segment_to_string).collect();
        self.lookup(&parts)
    }

    /// Iterate every `(path, line)` pair in the index. Used by the
    /// OpenAPI sensor to reverse-lookup a parameter name's line
    /// without a separate `name → line` map.
    pub fn iter(&self) -> impl Iterator<Item = (&Vec<String>, &u32)> {
        self.map.iter()
    }

    /// Find the line where `parameters[<i>].name` equals `name` for
    /// the given operation. Walks every `paths.<path>.<method>.parameters.<i>.name`
    /// entry in the index and re-reads the line from the spec text,
    /// returning the line whose value matches `name`. Returns the
    /// first match, or `None` if no parameter with that name exists.
    pub fn lookup_param_line(&self, path: &str, method: &str, name: &str) -> Option<u32> {
        let prefix: Vec<String> = vec![
            "paths".into(),
            path.into(),
            method.into(),
            "parameters".into(),
        ];
        for (key, &line) in &self.map {
            if key.len() != prefix.len() + 2 {
                continue;
            }
            if key[..prefix.len()] != prefix[..] {
                continue;
            }
            // last segment is `name`, second to last is the array index.
            if key[prefix.len() + 1] != "name" {
                continue;
            }
            if let Some(value) = self.name_value_at(line) {
                if value == name {
                    return Some(line);
                }
            }
        }
        None
    }

    /// Read the value of a `name: <X>` line. Strips a leading YAML
    /// `- ` if present, then the `name:` prefix, then any quotes
    /// around the scalar.
    fn name_value_at(&self, line: u32) -> Option<String> {
        let raw = self.source.get(line as usize - 1)?;
        let trimmed = raw.trim();
        // Strip leading `- ` for array entries.
        let without_dash = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        // Now `without_dash` should be `name: <value>`.
        let after = without_dash.strip_prefix("name:")?.trim_start();
        // Strip a single layer of matching surrounding quotes if
        // present (single or double).
        let len = after.len();
        let unquoted = if len >= 2 {
            let bytes = after.as_bytes();
            if (bytes[0] == b'"' && bytes[len - 1] == b'"')
                || (bytes[0] == b'\'' && bytes[len - 1] == b'\'')
            {
                &after[1..len - 1]
            } else {
                after
            }
        } else {
            after
        };
        Some(unquoted.to_string())
    }
}

fn segment_to_string(seg: &crate::federation::contracts::model::PathSegment) -> String {
    use crate::federation::contracts::model::PathSegment;
    match seg {
        PathSegment::Name(s) => s.clone(),
        PathSegment::ArrayItems => "[]".to_string(),
        PathSegment::MapValues => "{}".to_string(),
    }
}

// ─── YAML block mapping index ────────────────────────────────────────
//
// Approach: track a stack of `(key_indent, path)` frames. For each
// non-empty, non-comment line:
//   1. Determine whether the line starts an array item (`- `) and
//      compute the key indent accordingly (`line_indent + 2` for
//      arrays, `line_indent` for plain keys).
//   2. Pop frames with `key_indent <= frame.key_indent` — equal
//      means the previous frame was a sibling at the same indent.
//   3. If the line is an array item, advance the per-parent-path
//      counter and push the index onto the new path.
//   4. Push the property name and record the (path, line) pair.

fn build_yaml(content: &str) -> LineIndex {
    let mut map: BTreeMap<Vec<String>, u32> = BTreeMap::new();
    let mut frames: Vec<YamlFrame> = Vec::new();
    // Tracks the next array-item index each parent path will mint.
    // Persists across frame pops because the parent context recurs
    // when a new sibling `- foo:` appears at the same indent.
    let mut next_array_index: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    // Tracks the index of the array item we are currently inside.
    // Stays stable while we walk children of the same item; updated
    // when a new `- foo:` begins the next sibling.
    let mut current_array_index: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    let source: Vec<String> = content.lines().map(str::to_string).collect();

    for (idx, raw) in content.lines().enumerate() {
        let line_no = idx as u32 + 1;
        let line_indent = leading_spaces(raw);
        let stripped = raw[line_indent..].trim_end();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }

        let is_arr = starts_with_dash(stripped);
        let body = if is_arr { &stripped[2..] } else { stripped };
        let key_indent = if is_arr { line_indent + 2 } else { line_indent };

        let Some(key) = yaml_property_key(body) else {
            continue;
        };

        // Pop frames with key_indent <= this entry's key_indent.
        while let Some(top) = frames.last() {
            if top.key_indent >= key_indent {
                frames.pop();
            } else {
                break;
            }
        }

        let mut new_path: Vec<String> = frames.last().map(|f| f.path.clone()).unwrap_or_default();

        if is_arr {
            // The parent path identifies the array (e.g.
            // `[..., "parameters"]`). Mint the next index and
            // remember it as "the index of the array item we are
            // now inside" so subsequent child keys stay in the
            // same item.
            let i = next_array_index.entry(new_path.clone()).or_insert(0);
            let arr_idx = *i;
            *i = arr_idx + 1;
            current_array_index.insert(new_path.clone(), arr_idx);
            new_path.push(arr_idx.to_string());
        } else if let Some(&arr_idx) = current_array_index.get(&new_path) {
            // Non-array key at the same indent (or below) as the
            // preceding array item: still inside that item.
            new_path.push(arr_idx.to_string());
        }

        new_path.push(key.clone());
        map.insert(new_path.clone(), line_no);
        frames.push(YamlFrame {
            key_indent,
            path: new_path,
        });
    }

    LineIndex { map, source }
}

#[derive(Debug, Clone)]
struct YamlFrame {
    key_indent: usize,
    path: Vec<String>,
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

fn starts_with_dash(s: &str) -> bool {
    s.starts_with("- ") || s == "-"
}

/// If `body` is a `key:`, `key: value`, or `? key:` mapping entry,
/// return `Some(key)` with the surrounding quotes stripped. Returns
/// `None` for empty bodies, comments, and pure flow collections.
fn yaml_property_key(body: &str) -> Option<String> {
    let s = body.strip_prefix('?').unwrap_or(body).trim_start();
    let colon = s.find(':')?;
    let key = &s[..colon];
    if key.is_empty() {
        return None;
    }
    Some(unquote_yaml_key(key.trim()))
}

fn unquote_yaml_key(key: &str) -> String {
    if key.len() >= 2 {
        let bytes = key.as_bytes();
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
        {
            return key[1..key.len() - 1].to_string();
        }
    }
    key.to_string()
}

// ─── JSON index ─────────────────────────────────────────────────────
//
// Walk the JSON text; track a `(path, container_kind)` stack. For each
// `"key":` pattern, push the key. For each `{`/`[`, descend into a
// new container. For each `}`/`]`, ascend. For each `,`, advance to
// the next sibling — in objects pop the previous key, in arrays
// advance the index counter.

fn build_json(content: &str) -> LineIndex {
    let mut map: BTreeMap<Vec<String>, u32> = BTreeMap::new();
    let bytes = content.as_bytes();
    let mut path: Vec<String> = Vec::new();
    // Per-nesting container type, parallel to `path`. `true` = array.
    let mut container_stack: Vec<bool> = Vec::new();
    // Index counter for each open array. Same depth as container_stack.
    let mut index_stack: Vec<usize> = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    let mut key_start: Option<usize> = None;
    let mut line_no: u32 = 1;
    let source: Vec<String> = content.lines().map(str::to_string).collect();

    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];

        // Newlines outside strings bump the line counter.
        if !in_string && b == b'\n' {
            line_no += 1;
        }

        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
                let raw = &content[key_start.unwrap()..i];
                let key = unescape_json_string(raw);
                // Look ahead for `:` (allowing whitespace).
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b':' {
                    // It's a property name. Determine context.
                    let in_array = container_stack.last().copied().unwrap_or(false);
                    if !in_array {
                        // Object property — push the key.
                        path.push(key);
                    } else {
                        // Inside an array — the next property key
                        // belongs to an object element. Push the
                        // array index first, then the key.
                        let idx = index_stack.last_mut().copied().unwrap_or(0);
                        path.push(idx.to_string());
                        *index_stack.last_mut().unwrap() = idx + 1;
                        path.push(key);
                    }
                    map.insert(path.clone(), line_no);
                    // Advance past the colon for the next iteration.
                    i = j + 1;
                    continue;
                }
                // Not a key (the value was a string) — fall through.
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => {
                in_string = true;
                key_start = Some(i + 1);
            }
            b'{' => {
                container_stack.push(false);
            }
            b'[' => {
                container_stack.push(true);
                index_stack.push(0);
                // The first array element starts at index 0.
                path.push("0".to_string());
            }
            b'}' => {
                container_stack.pop();
                path.pop();
            }
            b']' => {
                container_stack.pop();
                index_stack.pop();
                path.pop();
            }
            b',' => {
                let in_array = container_stack.last().copied().unwrap_or(false);
                if in_array {
                    // Move to the next array item. Increment the
                    // index counter and pop/push.
                    if let Some(idx) = index_stack.last_mut() {
                        *idx += 1;
                    }
                    // The last path element was the previous array
                    // item's index. Pop it, then push the new index.
                    path.pop();
                    let idx = index_stack.last().copied().unwrap_or(0);
                    path.push(idx.to_string());
                } else {
                    // Object: pop the previous key.
                    path.pop();
                }
            }
            _ => {}
        }
        i += 1;
    }

    LineIndex { map, source }
}

fn unescape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let code: String = chars.by_ref().take(4).collect();
                    if let Ok(cp) = u32::from_str_radix(&code, 16) {
                        if let Some(ch) = char::from_u32(cp) {
                            out.push(ch);
                        }
                    }
                }
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn yaml_block_index_records_property_lines() {
        let spec = "\
openapi: 3.0.0
info:
  title: T
  version: '1'
paths:
  /api/orders/{id}:
    get:
      operationId: getOrder
      responses:
        '200':
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties:
                  customer_id:
                    type: string
";
        let idx = LineIndex::build(spec);
        assert_eq!(idx.lookup(&p(&["openapi"])), Some(1));
        assert_eq!(idx.lookup(&p(&["info", "title"])), Some(3));
        assert_eq!(idx.lookup(&p(&["paths"])), Some(5));
        assert_eq!(idx.lookup(&p(&["paths", "/api/orders/{id}"])), Some(6));
        assert_eq!(
            idx.lookup(&p(&["paths", "/api/orders/{id}", "get"])),
            Some(7)
        );
        assert_eq!(
            idx.lookup(&p(&["paths", "/api/orders/{id}", "get", "responses"])),
            Some(9)
        );
        assert_eq!(
            idx.lookup(&p(&[
                "paths",
                "/api/orders/{id}",
                "get",
                "responses",
                "200"
            ])),
            Some(10)
        );
        assert_eq!(
            idx.lookup(&p(&[
                "paths",
                "/api/orders/{id}",
                "get",
                "responses",
                "200",
                "content",
                "application/json",
                "schema"
            ])),
            Some(14)
        );
        assert_eq!(
            idx.lookup(&p(&[
                "paths",
                "/api/orders/{id}",
                "get",
                "responses",
                "200",
                "content",
                "application/json",
                "schema",
                "properties"
            ])),
            Some(16)
        );
        assert_eq!(
            idx.lookup(&p(&[
                "paths",
                "/api/orders/{id}",
                "get",
                "responses",
                "200",
                "content",
                "application/json",
                "schema",
                "properties",
                "customer_id"
            ])),
            Some(17)
        );
    }

    #[test]
    fn yaml_block_index_falls_back_to_nearest_ancestor_for_unknown_path() {
        let spec = "\
paths:
  /a:
    get:
      responses:
        '200':
          description: ok
";
        let idx = LineIndex::build(spec);
        let lookup = p(&[
            "paths",
            "/a",
            "get",
            "responses",
            "200",
            "content",
            "application/json",
            "schema",
        ]);
        // Longest known prefix is "200" at line 5.
        assert_eq!(idx.lookup(&lookup), Some(5));
    }

    #[test]
    fn yaml_block_index_handles_parameters_array() {
        let spec = "\
paths:
  /items:
    get:
      parameters:
        - name: limit
          in: query
          schema:
            type: integer
        - name: offset
          in: query
";
        let idx = LineIndex::build(spec);
        // parameters itself at line 4.
        assert_eq!(
            idx.lookup(&p(&["paths", "/items", "get", "parameters"])),
            Some(4)
        );
        // First array item (index "0"), schema at line 7.
        assert_eq!(
            idx.lookup(&p(&["paths", "/items", "get", "parameters", "0", "schema"])),
            Some(7)
        );
        // Second array item, "name" at line 9.
        assert_eq!(
            idx.lookup(&p(&["paths", "/items", "get", "parameters", "1", "name"])),
            Some(9)
        );
    }

    #[test]
    fn yaml_flow_falls_back_to_block_ancestor() {
        let spec = "\
paths:
  /a:
    get: { operationId: getA, responses: { '200': { description: ok } } }
";
        let idx = LineIndex::build(spec);
        // /a.get is at line 3 (the block property "get:").
        assert_eq!(
            idx.lookup(&p(&["paths", "/a", "get", "operationId"])),
            Some(3)
        );
        // "responses.200.description" falls back to the get line.
        assert_eq!(
            idx.lookup(&p(&[
                "paths",
                "/a",
                "get",
                "responses",
                "200",
                "description"
            ])),
            Some(3)
        );
    }

    #[test]
    fn json_index_records_keys_at_their_lines() {
        let spec = "{
  \"openapi\": \"3.0.0\",
  \"paths\": {
    \"/a\": {
      \"get\": {
        \"operationId\": \"getA\",
        \"responses\": {
          \"200\": {
            \"description\": \"ok\"
          }
        }
      }
    }
  }
}";
        let idx = LineIndex::build(spec);
        assert_eq!(idx.lookup(&p(&["openapi"])), Some(2));
        assert_eq!(idx.lookup(&p(&["paths"])), Some(3));
        assert_eq!(idx.lookup(&p(&["paths", "/a", "get"])), Some(5));
        assert_eq!(
            idx.lookup(&p(&["paths", "/a", "get", "operationId"])),
            Some(6)
        );
    }

    #[test]
    fn unknown_root_returns_none() {
        let idx = LineIndex::build("");
        assert_eq!(idx.lookup(&p(&["nope"])), None);
    }

    #[test]
    fn lookup_param_line_resolves_by_name() {
        let spec = "\
paths:
  /items:
    get:
      parameters:
        - name: limit
          in: query
          schema:
            type: integer
        - name: offset
          in: query
        - name: cursor
          in: path
          schema:
            type: string
";
        let idx = LineIndex::build(spec);
        assert_eq!(
            idx.lookup_param_line("/items", "get", "limit"),
            Some(5),
            "limit at line 5"
        );
        assert_eq!(
            idx.lookup_param_line("/items", "get", "offset"),
            Some(9),
            "offset at line 9"
        );
        // `cursor` is a path parameter, not `in: query`, but the
        // index is name-driven: it does not filter by `in`. This is
        // intentional — the sensor filters by `in` before calling
        // lookup_param_line.
        assert_eq!(
            idx.lookup_param_line("/items", "get", "cursor"),
            Some(11),
            "cursor at line 11"
        );
        assert_eq!(
            idx.lookup_param_line("/items", "get", "missing"),
            None,
            "unknown parameter name → None"
        );
    }
}
