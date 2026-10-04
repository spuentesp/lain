; Camp-B pattern: gin-route (route).
;
; Matches `r.GET("/path", handler)` /
; `router.POST("/path", handler)`. The verb is uppercase in Go
; source, so the regex fallback handles the lowercase mapping.
;
; Captures:
;   @path    — the path string literal
;   @verb    — the verb identifier (`GET`, `POST`, …)
;   @handler — the handler function name

(call_expression
  function: (selector_expression
    field: (field_identifier) @verb)
  arguments: (argument_list
    (interpreted_string_literal) @path
    (identifier) @handler))
