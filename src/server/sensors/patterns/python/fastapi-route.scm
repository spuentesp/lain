; Camp-B pattern: fastapi-route (route).
;
; Matches `@app.get("/path")` / `@router.post("/path")` decorators.
; Captures:
;   @path — the path string content (the `"/path"` literal)
;   @verb — the verb identifier (`get`, `post`, …)
;
; Note: Python's `tree-sitter-python` 0.25 emits `decorator → call`
; (not `decorator → attribute → call`), and a `(string)` node's
; `string_content` is one of its children. Build-time validation in
; `patterns/build.rs` caught the double-`attribute` wrapper and
; the off-by-one path — see data-driven-sensor-patterns plan Task 6.

; A route path starts with `/` (Starlette asserts it). This drops
; `@mock.patch("pkg.module.fn")`, which has the same decorator shape.
(decorator
  (call
    function: (attribute
      attribute: (identifier) @verb)
    arguments: (argument_list
      (string
        (string_content) @path)))
  (#match? @path "^/"))

; Keyword registrations, including multiline calls. The extractor groups
; captures by @route_site so argument order is irrelevant. Each literal
; method is captured separately; unresolved methods do not invent ANY routes.
; Only plain, unescaped string literals qualify. In an f-string a captured
; string_content can be just one suffix after a dynamic prefix. Prefixed or
; escaped literals need language-aware evaluation before claiming a route.
(
  (call
    function: (attribute attribute: (identifier) @_registration)
    arguments: (argument_list
      (keyword_argument
        name: (identifier) @_path_key
        value: (string (string_content) @path) @_path_literal))) @route_site
  (#eq? @_registration "add_api_route")
  (#eq? @_path_key "path")
  (#match? @_path_literal "^[\"']")
  (#not-match? @_path_literal "\\\\")
  (#match? @path "^/"))
(
  (call
    function: (attribute attribute: (identifier) @_registration)
    arguments: (argument_list
      (keyword_argument
        name: (identifier) @_endpoint_key
        value: [(identifier) (attribute)] @handler))) @route_site
  (#eq? @_registration "add_api_route")
  (#eq? @_endpoint_key "endpoint"))
(
  (call
    function: (attribute attribute: (identifier) @_registration)
    arguments: (argument_list
      (keyword_argument
        name: (identifier) @_methods_key
        value: (list (string (string_content) @verb))))) @route_site
  (#eq? @_registration "add_api_route")
  (#eq? @_methods_key "methods")
  (#match? @verb "(?i)^(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS)$"))
