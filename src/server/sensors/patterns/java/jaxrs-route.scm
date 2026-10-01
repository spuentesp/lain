; Camp-B pattern: jaxrs-route (route).
;
; Matches `@Path("/api")` on the class + `@GET` / `@POST` on the
; method. The path lives on the class-level `@Path`; the verb lives
; on the method-level annotation. The walker joins them.

(annotation
  name: (identifier) @_annotation_name
  arguments: (annotation_argument_list
    (string_literal) @path))
