; Camp-B pattern: faraday-outbound (outbound).
;
; Matches `Faraday.<verb>(url)`.
;
; `Faraday` is a Ruby constant (no scope resolution), so the
; receiver is a `constant` node.

(call
  receiver: (constant) @lib
  method: (identifier) @verb
  arguments: (argument_list
    (_) @url)) @call