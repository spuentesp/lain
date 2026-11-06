(call
  function: (attribute
    object: (identifier) @receiver
    attribute: (identifier) @method)
  arguments: (argument_list) @arguments
  (#eq? @receiver "op")
  (#eq? @method "add_column")) @operation
