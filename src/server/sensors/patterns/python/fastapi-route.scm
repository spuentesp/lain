; Camp-B pattern: fastapi-route (route).
;
; Matches `@app.get("/path")` / `@router.post("/path")` decorators.
; Captures:
;   @path — the path string literal
;   @verb — the verb identifier (`get`, `post`, …)

(decorator
  (attribute
    (attribute
      attribute: (identifier) @_outer
      (argument_list
        (call
          function: (attribute
            object: (_) @_receiver
            attribute: (identifier) @verb)
          arguments: (argument_list
            (string
              (string_content) @path)))))))
