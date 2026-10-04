; Camp-B pattern: rails-route (route).
;
; Matches the Rails routes-DSL shape in `config/routes.rb`:
; `get 'path'`, `post "path"`. Captures:
;   @path — the path string literal
;   @verb — the verb identifier (`get`, `post`, …)

(call
  method: (identifier) @verb
  arguments: (argument_list
    (string
      (string_content) @path)))
