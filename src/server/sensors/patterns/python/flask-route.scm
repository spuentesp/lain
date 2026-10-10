; Camp-B pattern: flask-route (route).
;
; Matches `@app.route("/path", methods=["POST"])` decorators. The
; verb may live in the `methods=` kwarg rather than the decorator
; name; the regex fallback handles the verb extraction.
;
; Captures:
;   @path — the path string content
;
; Note: see `fastapi-route.scm` for the build-time-validation note —
; the same Python grammar drift applies (decorator → call, no extra
; attribute wrapper). Build-time validation in `patterns/build.rs`
; caught the double-`attribute` wrapper — see data-driven-sensor-
; patterns plan Task 6.

(decorator
  (call
    function: (attribute
      attribute: (identifier) @_route)
    arguments: (argument_list
      (string
        (string_content) @path))))
