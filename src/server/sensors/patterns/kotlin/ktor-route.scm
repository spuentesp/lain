; Camp-B pattern: ktor-route (route).
;
; Matches `routing { get("/path") { … } }` /
; `get("/path") { call.respondText("ok") }`. Captures:
;   @path — the path string literal inside the verb call
;   @verb — the verb identifier (`get`, `post`, …)

(call_expression
  function: (simple_identifier) @verb
  value_arguments: (value_arguments
    (string_literal) @path))
