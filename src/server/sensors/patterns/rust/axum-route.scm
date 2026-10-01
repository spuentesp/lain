; Camp-B pattern: axum-route (route).
;
; Matches `Router::new().route("/path", get(handler))` and similar
; chained `.route(...)` call shapes. Captures:
;   @path    — the route path string literal
;   @verb    — the verb identifier (get, post, …) wrapping the handler
;   @handler — the handler function name
;
; The `#eq?` predicate restricts the match to the `route` field so
; a non-route call like `.something("/x", get(h))` doesn't
; double-fire under this query.
;
; The http_sensor walker reads these captures and emits one
; `HttpRoute` per match. The regex path in `frameworks.yaml`
; (`path_regex` / `handler_regex`) backs it up when this query is
; missing or empty.

(call_expression
  function: (field_expression
    field: (field_identifier) @_route_field)
  arguments: (arguments
    (string_literal) @path
    (call_expression
      function: (identifier) @verb
      arguments: (arguments
        (identifier) @handler)))
  (#eq? @_route_field "route"))
