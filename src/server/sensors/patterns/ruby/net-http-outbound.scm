; Camp-B pattern: net-http-outbound (outbound).
;
; Matches Ruby's `Net::HTTP.get(URI(url))`,
; `Net::HTTP.get_response(URI(url))`, `Net::HTTP.post(...)`, etc.
; The `URI(url)` wrapper is preserved in the URL subtree so the
; walker can extract the URL argument via `parts_from_node_inner`.
;
; Captures:
;   @call — the call subtree
;   @lib  — the receiver (a scope_resolution like `Net::HTTP`)
;   @verb — the verb identifier
;   @url  — the URL argument (possibly a `URI(...)` call)

(call
  receiver: (scope_resolution) @lib
  method: (identifier) @verb
  arguments: (argument_list
    (_) @url)) @call