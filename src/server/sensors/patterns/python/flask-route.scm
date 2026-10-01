; Camp-B pattern: flask-route (route).
;
; Matches `@app.route("/path", methods=["POST"])` decorators. The
; verb may live in the `methods=` kwarg rather than the decorator
; name; the regex fallback handles the verb extraction.
;
; Captures:
;   @path — the path string literal

(decorator
  (attribute
    (attribute
      attribute: (identifier) @_outer
      (argument_list
        (call
          function: (attribute
            object: (_) @_receiver
            attribute: (identifier) @_route)
          arguments: (argument_list
            (string
              (string_content) @path)))))))
