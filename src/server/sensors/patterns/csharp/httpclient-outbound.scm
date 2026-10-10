; Camp-B pattern: httpclient-outbound (outbound).
;
; Matches `client.GetAsync(url)`, `client.PostAsync(url, content)`,
; and other .NET outbound HTTP calls. The .NET HttpClient verbs are
; spelled `GetAsync`, `PostAsync`, `PutAsync`, `PatchAsync`,
; `DeleteAsync`, `SendAsync`; the walker maps these to the canonical
; HTTP methods.
;
; The `#any-of?` predicate restricts the match to the recognised
; verb spellings — `client.Connect("/api")` doesn't pass and is
; therefore not picked up as an outbound call.
;
; Captures:
;   @call — the invocation subtree
;   @lib  — the receiver identifier (`client`, `wc`, …)
;   @verb — the verb identifier (`GetAsync`, `PostAsync`, …)
;   @url  — the URL argument (first positional child of argument_list)

(invocation_expression
  function: (member_access_expression
    (identifier) @lib
    (identifier) @verb)
  arguments: (argument_list
    . (_) @url)
  (#any-of? @verb "GetAsync" "GetStringAsync" "PostAsync" "PutAsync" "PatchAsync" "DeleteAsync" "SendAsync" "DownloadString" "DownloadStringTaskAsync")) @call