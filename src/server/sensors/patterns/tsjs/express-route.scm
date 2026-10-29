; Camp-B pattern: express-route (route).
;
; Matches `router.get("/path", handler)` /
; `app.post("/path", handler)`. Captures:
;   @path    — the path string literal
;   @verb    — the verb identifier (`get`, `post`, …)
;   @handler — the handler function name

(call_expression
  function: (member_expression
    property: (property_identifier) @verb)
  arguments: (arguments
    (string
      (string_fragment) @path)
    (identifier) @handler)
  ; Only HTTP verbs declare a route. Without the verb check,
  ; `console.error('…', err)` and `headers.set('x-…', v)` match this
  ; shape and become `ANY` routes. A route path starts with `/`.
  (#match? @verb "^(get|post|put|delete|patch|head|options|all)$")
  (#match? @path "^/"))
