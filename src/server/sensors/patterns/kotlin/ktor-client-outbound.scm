; Camp-B pattern: ktor-client-outbound (outbound).
;
; Matches Ktor's `client.get<String>(url)` (generic-arg call) and
; `client.get(url)` (non-generic call). The Kotlin grammar parses
; the generic-arg form as a `binary_expression` whose right side
; is the parenthesized argument list and whose left side carries
; the `<Type>` generic args (a sibling `binary_expression` with
; the `navigation_expression` `client.get` as its own left).
;
; The Kotlin grammar's `navigation_expression` exposes the
; receiver and the verb as named children; the verb is always
; the rightmost. The walker extracts the verb from the captured
; `navigation_expression` (`@_nav`) via
; [`extract_navigation_verb`].
;
; The `#any-of?` predicate on @_nav's text (the full
; navigation_expression text) restricts the match to verb
; spellings like `client.get`, `client.post`, etc. — chained
; calls like `Request.Builder().url("/api")` (where the
; navigation_expression text is `Request.Builder().url`) are
; filtered out because their full text doesn't match.
;
; Note: tree-sitter-kotlin-ng does not use field names for
; `call_expression` children (`function` is unnamed in the
; grammar), so patterns below reference the children by node
; type, not by `field:`.
;
; Captures:
;   @call — the call subtree
;   @_nav — the navigation_expression (walker extracts the verb)
;   @url  — the URL argument

; Pattern A: non-generic `client.<verb>(url)` — @url is the first
; positional child of value_arguments.
(call_expression
  (navigation_expression) @_nav
  (value_arguments
    (value_argument
      (string_literal) @url))) @call

; Pattern B: generic-arg `client.<verb><<Type>>(url)` — the URL
; lives inside a `parenthesized_expression` (whose child is the
; string literal); the verb lives on the left side of the outer
; `binary_expression`'s left.
(binary_expression
  left: (binary_expression
    left: (navigation_expression) @_nav)
  right: (parenthesized_expression
    (string_literal) @url)) @call