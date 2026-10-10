; Camp-B pattern: sinatra-route (route).
;
; Matches `get '/path' do … end` /
; `post "/path" do … end`. Captures:
;   @path — the path string literal
;   @verb — the verb identifier (`get`, `post`, …)

(call
  method: (identifier) @verb
  arguments: (argument_list
    (string
      (string_content) @path)))
