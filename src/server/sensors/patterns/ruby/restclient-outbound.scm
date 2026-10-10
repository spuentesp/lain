; Camp-B pattern: restclient-outbound (outbound).
;
; Matches `RestClient.<verb>(url)`.
;
; `RestClient` is a Ruby constant (no scope resolution), so the
; receiver is a `constant` node.

(call
  receiver: (constant) @lib
  method: (identifier) @verb
  arguments: (argument_list
    (_) @url)) @call