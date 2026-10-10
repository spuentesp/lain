; Camp-B pattern: stdlib-http-outbound (outbound).
;
; Matches `http.Get(url)`, `http.Post(url, ct, body)`,
; `http.NewRequest(method, url, body)`,
; `http.NewRequestWithContext(ctx, method, url, body)`.
;
; The capture contract puts @url at the argument position the
; walker expects for each shape. Three patterns cover the three
; variants:
;
;   Pattern A: http.<Verb>(url, ...) — Verb is a known HTTP verb;
;     @url is the first positional argument. The `. (interpreted_string_literal)`
;     anchor restricts the match to arg 0 (the URL); the rest of
;     the call's arguments are ignored so `http.Post("url", "ct")`
;     doesn't double-fire under this query.
;   Pattern B: http.NewRequest(method, url, body) — @url is the
;     second positional argument; the walker reads the first arg
;     for the method.
;   Pattern C: http.NewRequestWithContext(ctx, method, url, body)
;     — @url is the third positional argument; the walker reads
;     the second arg for the method.
;
; Captures:
;   @call — the call subtree (for line/position)
;   @lib  — the library identifier (`http`)
;   @verb — the verb identifier (`Get`, `Post`, `NewRequest`, …)
;   @url  — the URL argument
;   @_method — the method argument for the NewRequest patterns

; Pattern A: verb is a known HTTP verb
(call_expression
  function: (selector_expression
    operand: (identifier) @lib
    field: (field_identifier) @verb)
  arguments: (argument_list
    . (interpreted_string_literal) @url)
  (#eq? @lib "http")
  (#any-of? @verb "Get" "Post" "Put" "Head" "Patch" "Delete" "Options")) @call

; Pattern B: http.NewRequest(method, url, body) — 3-arg form.
(call_expression
  function: (selector_expression
    operand: (identifier) @lib
    field: (field_identifier) @verb)
  arguments: (argument_list
    (_) @_method
    (interpreted_string_literal) @url
    (_) @_body)
  (#eq? @lib "http")
  (#eq? @verb "NewRequest")) @call

; Pattern C: http.NewRequestWithContext(ctx, method, url, body) — 4-arg form.
(call_expression
  function: (selector_expression
    operand: (identifier) @lib
    field: (field_identifier) @verb)
  arguments: (argument_list
    (_) @_ctx
    (_) @_method
    (interpreted_string_literal) @url
    (_) @_body)
  (#eq? @lib "http")
  (#eq? @verb "NewRequestWithContext")) @call