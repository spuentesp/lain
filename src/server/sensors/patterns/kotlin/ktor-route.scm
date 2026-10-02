; Camp-B pattern: ktor-route (route).
;
; Matches `routing { get("/path") { … } }` /
; `get("/path") { call.respondText("ok") }`. Captures:
;   @path — the path string literal inside the verb call
;   @verb — the verb identifier (`get`, `post`, …)
;
; Note: the Kotlin tree-sitter grammar (`tree-sitter-kotlin-ng`)
; has no `function:` field and no `simple_identifier` node type —
; `call_expression` exposes its callee + argument list as unnamed
; children (`identifier`, `value_arguments`) and the string literal
; lives under `value_argument`. Build-time validation in
; `patterns/build.rs` caught the drift from the Rust/Java shape —
; see data-driven-sensor-patterns plan Task 6.

(call_expression
  (identifier) @verb
  (value_arguments
    (value_argument
      (string_literal) @path)))
