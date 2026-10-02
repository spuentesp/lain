; Camp-B pattern: ureq-outbound (outbound).
;
; Matches `ureq::get(url)`, `ureq::post(url)` etc. — covered
; alongside reqwest-outbound since both share the
; `scoped_identifier` shape. The walker resolves the library name
; from the `path` field.

(call_expression
  function: (scoped_identifier
    path: (identifier) @lib
    name: (identifier) @verb)
  arguments: (arguments
    (_) @url)
  (#eq? @lib "ureq")) @call

(call_expression
  function: (field_expression
    value: (identifier) @lib
    field: (field_identifier) @verb)
  arguments: (arguments
    (_) @url)
  (#eq? @lib "ureq")) @call