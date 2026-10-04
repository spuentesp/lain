; Camp-B pattern: stdlib-http-route (route).
;
; Matches `http.HandleFunc("/path", handler)` exclusively. The
; verbless shape emits `HttpMethod::Any` per §6.2. The
; `#eq?` predicate restricts the match to the `HandleFunc`
; field so a Gin `r.GET(...)` doesn't double-fire under this
; query.
;
; Captures:
;   @path    — the path string literal
;   @handler — the handler function name

(call_expression
  function: (selector_expression
    field: (field_identifier) @_handle_field)
  arguments: (argument_list
    (interpreted_string_literal) @path
    (identifier) @handler)
  (#eq? @_handle_field "HandleFunc"))
