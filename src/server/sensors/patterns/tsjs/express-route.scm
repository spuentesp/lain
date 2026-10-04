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
    (identifier) @handler))
