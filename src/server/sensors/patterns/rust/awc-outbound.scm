; Camp-B pattern: awc-outbound (outbound).
;
; Matches `awc::Client::new().get(url)` (chained-builder shape is
; OUT OF SCOPE for v1) and the bare `awc::get(url)` shape (covered
; here for completeness; the existing walker doesn't chase the
; chained-builder receiver and neither does this query).
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
  (#eq? @lib "awc")) @call

(call_expression
  function: (field_expression
    value: (identifier) @lib
    field: (field_identifier) @verb)
  arguments: (arguments
    (_) @url)
  (#eq? @lib "awc")) @call