; Camp-B pattern: axios-outbound (outbound).
;
; Matches `axios(url)`, `axios({method, url})`, and
; `axios.<verb>(url)`.

(call_expression
  function: (identifier) @lib
  arguments: (arguments
    . (_) @url)
  (#eq? @lib "axios")) @call

(call_expression
  function: (member_expression
    object: (identifier) @lib
    property: (property_identifier) @verb)
  arguments: (arguments
    . (_) @url)
  (#eq? @lib "axios")) @call