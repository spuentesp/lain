; Camp-B pattern: fastify-route (route).
;
; Matches the Fastify call shape, identical to Express at the
; route declaration site (`fastify.get("/path", handler)`).
; Captures mirror the Express walker.

(call_expression
  function: (member_expression
    property: (property_identifier) @verb)
  arguments: (arguments
    (string
      (string_fragment) @path)
    (identifier) @handler)
  ; Only HTTP verbs declare a route. Without the verb check,
  ; `console.error('…', err)` and `headers.set('x-…', v)` match this
  ; shape and become `ANY` routes. A route path starts with `/`.
  (#match? @verb "^(get|post|put|delete|patch|head|options|all)$")
  (#match? @path "^/"))
