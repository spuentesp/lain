; Camp-B pattern: aspnet-route (route).
;
; Matches `[HttpGet("/path")]` / `[HttpPost("/path")]` etc. on a
; method declaration. Captures:
;   @path — the path string literal inside the attribute
;   @verb — the verb portion (`Get`, `Post`, …)
;
; Note: `attribute_argument_list` is a *child* of `attribute`, not a
; field (per the C# tree-sitter grammar's `node-types.json`), so the
; pattern addresses it positionally. A string literal lives under
; `attribute_argument → expression`, so the path has to drill one
; level deeper than the obvious `(string_literal)` would suggest.
; Build-time validation in `patterns/build.rs` caught the wrong
; field name — see data-driven-sensor-patterns plan Task 6.

(attribute_list
  (attribute
    name: (identifier) @verb
    (attribute_argument_list
      (attribute_argument
        (string_literal) @path))))
