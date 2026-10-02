; Camp-B pattern: reqwest-outbound (outbound).
;
; Matches `reqwest::get(url)`, `reqwest::blocking::get(url)`,
; `ureq::post(url)` etc. — the `scoped_identifier` shape where the
; rightmost identifier is the verb. The walker resolves the
; library name from the `path` field.
;
; Captures:
;   @call — the call subtree
;   @lib  — the library identifier (path)
;   @verb — the verb identifier (name)
;   @url  — the first argument

(call_expression
  function: (scoped_identifier
    path: (identifier) @lib
    name: (identifier) @verb)
  arguments: (arguments
    (_) @url)
  (#any-of? @lib "reqwest" "ureq")) @call

; Bare `reqwest.get(url)` / `ureq.post(url)` — the receive-text
; must be one of the known libraries (predicate).
(call_expression
  function: (field_expression
    value: (identifier) @lib
    field: (field_identifier) @verb)
  arguments: (arguments
    (_) @url)
  (#any-of? @lib "reqwest" "ureq")) @call