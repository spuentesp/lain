; Camp-B pattern: rails-entry-point (entrypoint).
;
; Matches Rails controller classes (class `<Name>Controller` with a
; superclass clause) and captures every action method (`def index`,
; `def show`, …) inside the class body. The `#match?` predicate
; pins the file at a controller-class shape — non-controller classes
; never fire this pattern.
;
; Captures:
;   @class_name — the controller class name (e.g. `UsersController`)
;   @handler — the method name (action handler)
;
; Note: the Ruby tree-sitter grammar wraps the class body in
; `body_statement`, not `body`. Build-time validation in
; `patterns/build.rs` caught the wrong node type — see
; data-driven-sensor-patterns plan Task 6.

(class
  name: (constant) @class_name
  superclass: (_)
  body: (body_statement
    (method
      name: (identifier) @handler)+)
  (#match? @class_name "Controller$"))