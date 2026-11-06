(decorator
  (call_expression
    function: (identifier) @decorator
    arguments: (arguments) @options)
  (#any-of? @decorator "Entity" "Column")) @orm_decorator
