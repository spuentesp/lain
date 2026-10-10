; Camp-B pattern: httpx-outbound (outbound).
;
; Matches `httpx.<verb>(url, ...)` and the
; `httpx.request(<method>, <url>)` / `httpx.request(method=…, url=…)`
; shapes. Mirrors requests-outbound; the `httpx` receiver is
; pinned via `#eq?` so the query doesn't fire on `requests.<verb>`.

(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (string) @url)
  (#eq? @lib "httpx")
  (#any-of? @verb "get" "GET" "Get" "post" "POST" "Post" "put" "PUT" "Put" "patch" "PATCH" "Patch" "delete" "DELETE" "Delete" "head" "HEAD" "Head" "options" "OPTIONS" "Options")) @call

(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (string) @_method
    (string) @url)
  (#eq? @lib "httpx")
  (#eq? @verb "request")) @call

(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (keyword_argument
      name: (identifier) @_url_kw
      value: (string) @url))
  (#eq? @lib "httpx")
  (#eq? @verb "request")
  (#eq? @_url_kw "url")) @call

(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (string) @url)
  (#not-eq? @lib "httpx")
  (#not-eq? @lib "requests")
  (#not-eq? @lib "aiohttp")
  (#any-of? @verb "get" "GET" "Get" "post" "POST" "Post" "put" "PUT" "Put" "patch" "PATCH" "Patch" "delete" "DELETE" "Delete" "head" "HEAD" "Head" "options" "OPTIONS" "Options")) @call

(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (_) @_method
    (string) @url)
  (#not-eq? @lib "httpx")
  (#not-eq? @lib "requests")
  (#not-eq? @lib "aiohttp")
  (#eq? @verb "request")) @call