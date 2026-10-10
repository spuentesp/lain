; Camp-B pattern: spring-route (route).
;
; Matches `@GetMapping("/path")` /
; `@PostMapping(value = "/path")` /
; `@RequestMapping("/path")`. Captures:
;   @path — the path string literal inside the annotation
;   @verb — the verb portion of the annotation name (`get`, `post`, …)

(annotation
  name: (identifier) @_annotation_name
  arguments: (annotation_argument_list
    (string_literal) @path))
