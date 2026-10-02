; Camp-B pattern: got-outbound (outbound).
;
; Matches `got(url)` and `got.<verb>(url)`. The framework's
; `lib_match` is `^(got|ky)$` — the `.scm` accepts both libraries
; since they share the same call shape.

(call_expression
  function: (identifier) @lib
  arguments: (arguments
    . (_) @url)
  (#any-of? @lib "got" "ky")) @call

(call_expression
  function: (member_expression
    object: (identifier) @lib
    property: (property_identifier) @verb)
  arguments: (arguments
    . (_) @url)
  (#any-of? @lib "got" "ky")) @call