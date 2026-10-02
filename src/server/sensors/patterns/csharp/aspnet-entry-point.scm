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

(method_declaration
  (attribute_list
    (attribute
      name: (identifier) @verb
      argument_list: (attribute_argument_list
        (string_literal) @path)))
  name: (identifier) @handler)