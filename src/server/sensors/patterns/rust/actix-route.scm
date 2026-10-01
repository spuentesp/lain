; Camp-B pattern: actix-route (route).
;
; Matches `#[get("/path")]` / `#[post("/path")]` etc. attributes
; decorating a function. The handler is the function name declared
; on the next `fn` line, which the http_sensor walker resolves via
; the regex fallback when the captured handler is missing.
;
; Captures:
;   @path — the path string literal inside the attribute
;   @verb — the verb identifier (`get`, `post`, …)

(attribute_item
  (attribute
    (identifier) @verb
    (token_tree
      (string_literal) @path)))
