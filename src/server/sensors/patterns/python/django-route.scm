; Camp-B pattern: django-route (route).
;
; Matches Django's `path("foo/", view)` /
; `re_path(r"^bar/$", view)` URL-pattern constructor calls
; (typically in a project's `urls.py` `urlpatterns` list). The
; shape is a non-decorator constructor call with positional
; `(pattern, view)` arguments — tree-sitter distinct from
; `fastapi-route.scm` (decorator with attribute receiver) and
; `flask-route.scm` (decorator with attribute receiver).
;
; Captures:
;   @path    — the route path pattern (the `"foo/"` literal, or
;              the raw-string `r"^bar/$"` content)
;   @handler — the view function name (the second positional arg)
;
; No `@verb` capture is emitted: Django's URL patterns are
; verb-agnostic (a single pattern can serve GET, POST, etc. via
; the view's own dispatch). The walker therefore reports
; `HttpMethod::Any` for Django routes — the same default the
; http_sensor uses for `*` frameworks the `method_capture_for`
; match arm has no override for (e.g. `go-std`'s `HandleFunc`).
; This is deliberate, per §6.7 of the design spec.
;
; Note: see `fastapi-route.scm` for the build-time-validation note —
; the same Python grammar drift applies (string literal drills
; through `string → string_content`; tree-sitter-python 0.25 has no
; extra `attribute` wrapper around a bare `identifier` function).
; Build-time validation in `patterns/build.rs` catches malformed
; queries at compile time — see data-driven-sensor-patterns plan
; Task 6.

(call
  function: (identifier) @_url_fn
  arguments: (argument_list
    (string
      (string_content) @path)
    (identifier) @handler)
  (#any-of? @_url_fn "path" "re_path"))