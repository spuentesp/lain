(call_expression
  function: (identifier) @table_constructor
  arguments: (arguments
    (string) @table_name
    (object) @columns)
  (#any-of? @table_constructor "pgTable" "mysqlTable" "sqliteTable")) @table
