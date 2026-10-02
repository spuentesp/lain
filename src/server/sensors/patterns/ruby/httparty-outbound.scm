; Camp-B pattern: httparty-outbound (outbound).
;
; Matches `HTTParty.<verb>(url)` for the standard HTTP verbs.
;
; `HTTParty` is a Ruby constant (no scope resolution), so the
; receiver is a `constant` node.

(call
  receiver: (constant) @lib
  method: (identifier) @verb
  arguments: (argument_list
    (_) @url)) @call