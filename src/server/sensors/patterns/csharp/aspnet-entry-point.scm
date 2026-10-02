; Camp-B pattern: aspnet-entry-point (entrypoint).
;
; Matches `[HttpGet("/path")]`, `[HttpPost("/path")]`,
; `[HttpPost]` etc. attributes attached to a method declaration.
; Captures:
;   @verb — the verb portion of the attribute name (`HttpGet`,
;           `HttpPost`, `HttpPut`, `HttpDelete`, `HttpPatch`,
;           `HttpHead`, `HttpOptions`, `HttpRequest`)
;   @path — the path string literal inside the attribute
;   @handler — the method name (action handler)
;
; Note: `attribute_argument_list` is a *child* of `attribute`, not a
; field (per the C# tree-sitter grammar's `node-types.json`). A
; string literal lives under `attribute_argument → expression`, so
; the pattern drills two levels rather than the obvious one. Build-
; time validation in `patterns/build.rs` caught the wrong field name
; in an earlier shape — see data-driven-sensor-patterns plan Task 6.

(method_declaration
  (attribute_list
    (attribute
      name: (identifier) @verb
      (attribute_argument_list
        (attribute_argument
          (string_literal) @path))))
  name: (identifier) @handler)