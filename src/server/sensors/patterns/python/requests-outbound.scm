; Camp-B pattern: requests-outbound (outbound).
;
; Matches `requests.<verb>(url, ...)`, `requests.request("M", url)`,
; and `requests.request(method="M", url=url)`. Two pattern families:
;
;   1. Direct library call: `requests.<verb>(url)` — `@lib` is
;      `requests`. The `#eq?` predicate ensures the query doesn't
;      fire on `httpx.get(url)` (owned by `httpx-outbound`).
;   2. Client-instance call: `<recv>.<verb>(url)` where `<recv>`
;      is any identifier. The walker looks `<recv>` up in
;      `FileContext::client_base_urls` (populated by the
;      `requests.Session()` ctor-tracking pass) and surfaces the
;      bound library as the `Library { name }` on the emitted call.
;
; The verb predicate lists both lower- and upper-case spellings
; because Python's grammar doesn't lowercase identifiers — the
; walker would see `requests.GET` and need to match it.

; Pattern A1: direct `requests.<verb>(url)` — verb is HTTP verb.
(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (string) @url)
  (#eq? @lib "requests")
  (#any-of? @verb "get" "GET" "Get" "post" "POST" "Post" "put" "PUT" "Put" "patch" "PATCH" "Patch" "delete" "DELETE" "Delete" "head" "HEAD" "Head" "options" "OPTIONS" "Options")) @call

; Pattern A2: direct `requests.request(<method>, <url>)` — positional.
; The first argument is the method (any expression — string
; literal for "GET"/"POST", or identifier for a dynamic value).
(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (_) @_method
    (string) @url)
  (#eq? @lib "requests")
  (#eq? @verb "request")) @call

; Pattern A3: direct `requests.request(method=<m>, url=<u>)` — kwarg.
(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (keyword_argument
      name: (identifier) @_url_kw
      value: (string) @url))
  (#eq? @lib "requests")
  (#eq? @verb "request")
  (#eq? @_url_kw "url")) @call

; Pattern B1: client-instance `client.<verb>(url)` — verb is HTTP verb.
(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (string) @url)
  (#not-eq? @lib "requests")
  (#not-eq? @lib "httpx")
  (#not-eq? @lib "aiohttp")
  (#any-of? @verb "get" "GET" "Get" "post" "POST" "Post" "put" "PUT" "Put" "patch" "PATCH" "Patch" "delete" "DELETE" "Delete" "head" "HEAD" "Head" "options" "OPTIONS" "Options")) @call

; Pattern B2: client-instance `client.request(<method>, <url>)` —
; positional. Same exclusion predicates as B1 so we don't
; fire on the wrong framework.
(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (_) @_method
    (string) @url)
  (#not-eq? @lib "requests")
  (#not-eq? @lib "httpx")
  (#not-eq? @lib "aiohttp")
  (#eq? @verb "request")) @call