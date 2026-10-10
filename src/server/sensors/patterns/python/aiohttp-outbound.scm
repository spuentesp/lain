; Camp-B pattern: aiohttp-outbound (outbound).
;
; Matches `aiohttp.<verb>(url, ...)` and the
; `session.<verb>(url, ...)` client-instance form (the
; `aiohttp.ClientSession()` ctor binds `session`).

(call
  function: (attribute
    object: (identifier) @lib
    attribute: (identifier) @verb)
  arguments: (argument_list
    (string) @url)
  (#any-of? @verb "get" "GET" "Get" "post" "POST" "Post" "put" "PUT" "Put" "patch" "PATCH" "Patch" "delete" "DELETE" "Delete" "head" "HEAD" "Head" "options" "OPTIONS" "Options")) @call