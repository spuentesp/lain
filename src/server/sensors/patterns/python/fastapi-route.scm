; Camp-B pattern: fastapi-route (route).
;
; Matches `@app.get("/path")` / `@router.post("/path")` decorators.
; Captures:
;   @path — the path string content (the `"/path"` literal)
;   @verb — the verb identifier (`get`, `post`, …)
;
; Note: Python's `tree-sitter-python` 0.25 emits `decorator → call`
; (not `decorator → attribute → call`), and a `(string)` node's
; `string_content` is one of its children. Build-time validation in
; `patterns/build.rs` caught the double-`attribute` wrapper and
; the off-by-one path — see data-driven-sensor-patterns plan Task 6.

(decorator
  (call
    function: (attribute
      attribute: (identifier) @verb)
    arguments: (argument_list
      (string
        (string_content) @path))))
