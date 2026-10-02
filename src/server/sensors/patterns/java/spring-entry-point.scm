; Camp-B pattern: spring-entry-point (entrypoint).
;
; Matches `@GetMapping("/path")` /
; `@PostMapping(value = "/path")` /
; `@RequestMapping("/path")` annotations attached to a method
; declaration. Captures:
;   @verb — the verb portion of the annotation name (`GetMapping`,
;           `PostMapping`, `PutMapping`, `DeleteMapping`,
;           `PatchMapping`, `RequestMapping`)
;   @path — the path string literal inside the annotation
;   @handler — the method name (action handler)

(method_declaration
  (modifiers
    (annotation
      name: (identifier) @verb
      arguments: (annotation_argument_list
        (string_literal) @path)))
  name: (identifier) @handler)