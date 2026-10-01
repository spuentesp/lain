; Camp-B pattern: minimal-api-route (route).
;
; Matches `app.MapGet("/path", () => ...)` /
; `app.MapPost("/path", handler)`. Captures:
;   @path — the path string literal
;   @verb — the verb (`MapGet`, `MapPost`, …)

(invocation_expression
  function: (member_access_expression
    name: (identifier_name) @verb)
  argument_list: (argument_list
    (string_literal) @path))
