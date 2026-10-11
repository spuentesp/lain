; Camp-B pattern: okhttp-outbound (outbound).
;
; Matches OkHttp's `client.newCall(request).execute()` — the request
; object is the `newCall` argument. The walker resolves it to the
; `Request.Builder().url(...)` chain (Java local bindings in
; `FileContext` + the builder drill in `parts_from_node`), so a URL
; built from `System.getenv("X") + "/path"` surfaces as
; `HostPart::Env` instead of a synthetic unknown URL.
;
; The walker emits `via: Library { name: "okhttp" }` when this
; pattern matches.
;
; Captures:
;   @call — the execute call subtree
;   @verb — the verb identifier (`execute`)
;   @url — the `newCall` argument (the request expression)

(method_invocation
  object: (method_invocation
    arguments: (argument_list
      (_) @url))
  name: (identifier) @verb
  (#eq? @verb "execute")) @call
