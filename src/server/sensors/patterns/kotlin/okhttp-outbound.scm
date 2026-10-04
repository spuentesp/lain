; Camp-B pattern: okhttp-outbound (outbound).
;
; Matches OkHttp's `OkHttpClient.newCall(...).execute()` — Kotlin
; shape. The execute() call has no URL on the receiver's text;
; the URL lives on the `Request.Builder().url(...)` chain upstream
; which is captured by a separate concern.
;
; Captures:
;   @call — the call subtree
;   @verb — the verb identifier

(call_expression
  (navigation_expression
    (identifier) @verb)
  (#eq? @verb "execute")) @call