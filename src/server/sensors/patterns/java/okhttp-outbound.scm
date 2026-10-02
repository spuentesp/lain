; Camp-B pattern: okhttp-outbound (outbound).
;
; Matches OkHttp's `client.newCall(request).execute()` — the URL
; lives on the `Request.Builder().url(url)` ctor which is detected
; via a separate concern (the walker emits a synthetic URL for
; OkHttp today since the URL is buried in the builder chain).
;
; The walker emits `via: Library { name: "okhttp" }` when this
; pattern matches. The URL is captured but flagged as dynamic.
;
; Captures:
;   @call — the execute call subtree
;   @verb — the verb identifier (`execute`)

(method_invocation
  name: (identifier) @verb
  arguments: (argument_list)
  (#eq? @verb "execute")) @call