; Camp-B pattern: minimal-api-route (route).
;
; Matches `app.MapGet("/path", () => ...)` /
; `app.MapPost("/path", handler)`. Captures:
;   @path — the path string literal
;   @verb — the verb (`MapGet`, `MapPost`, …)
;
; Note: the C# grammar uses bare `identifier` (not `identifier_name`
; as in some other tree-sitter grammars) and the string literal
; lives under `argument → expression`. Build-time validation in
; `patterns/build.rs` caught all three — see data-driven-sensor-
; patterns plan Task 6.

(invocation_expression
  function: (member_access_expression
    name: (identifier) @verb)
  (argument_list
    (argument
      (string_literal) @path)))
