; Camp-B pattern: aspnet-route (route).
;
; Matches `[HttpGet("/path")]` / `[HttpPost("/path")]` etc. on a
; method declaration. Captures:
;   @path — the path string literal inside the attribute
;   @verb — the verb portion (`Get`, `Post`, …)

(attribute_list
  (attribute
    name: (identifier) @verb
    argument_list: (attribute_argument_list
      (string_literal) @path)))
