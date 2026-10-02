; Camp-B pattern: httpclient-outbound (outbound).
;
; Matches `HttpClient.send(...)` and `HttpClient.sendAsync(...)` —
; Java 11+'s built-in HTTP client. The URL lives on the
; `HttpRequest` argument (built upstream); the walker emits a
; synthetic URL for the v1 joiner.
;
; Captures:
;   @call — the method_invocation subtree
;   @verb — the verb identifier (`send`, `sendAsync`)

(method_invocation
  name: (identifier) @verb
  arguments: (argument_list)
  (#any-of? @verb "send" "sendAsync")) @call