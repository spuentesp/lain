; Camp-B pattern: fetch-outbound (outbound).
;
; Matches `fetch(url)` and `fetch(url, init)`. The URL is the
; first argument; the second argument (if present) is an options
; object whose `method` field the walker reads for the HTTP verb
; when present.

(call_expression
  function: (identifier) @lib
  arguments: (arguments
    . (_) @url)
  (#eq? @lib "fetch")) @call

; Pattern B: wrapper candidate — `<recv>.<verb>(url)` where
; `<recv>` is any identifier (not a known library) and the URL
; starts with `/`. The walker dedupes these by (path, line).
(call_expression
  function: (member_expression
    object: (identifier) @lib
    property: (property_identifier) @verb)
  arguments: (arguments
    . (_) @url)
  (#not-eq? @lib "fetch")
  (#not-eq? @lib "axios")
  (#not-eq? @lib "got")
  (#not-eq? @lib "ky")) @call