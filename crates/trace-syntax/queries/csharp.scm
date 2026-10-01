; trace-syntax query for C# (capture contract: README.md, SPEC §6.2).

; ---- declarations ----------------------------------------------------------------------

(class_declaration
  name: (identifier) @name
  body: (declaration_list) @body) @definition.class

(class_declaration
  (base_list [(identifier) (generic_name) (qualified_name)] @base)) @definition.class

(class_declaration
  (attribute_list) @decorator) @definition.class

(struct_declaration
  name: (identifier) @name
  body: (declaration_list) @body) @definition.class

(struct_declaration
  (base_list [(identifier) (generic_name) (qualified_name)] @base)) @definition.class

(record_declaration
  name: (identifier) @name) @definition.class

(interface_declaration
  name: (identifier) @name
  body: (declaration_list) @body) @definition.interface

(interface_declaration
  (base_list [(identifier) (generic_name) (qualified_name)] @base)) @definition.interface

(method_declaration
  name: (identifier) @name
  body: [(block) (arrow_expression_clause)]? @body) @definition.method

(method_declaration
  (attribute_list) @decorator) @definition.method

(constructor_declaration
  name: (identifier) @name
  body: [(block) (arrow_expression_clause)]? @body) @definition.constructor

(constructor_declaration
  (attribute_list) @decorator) @definition.constructor

(local_function_statement
  name: (identifier) @name
  body: [(block) (arrow_expression_clause)]? @body) @definition.function

; ---- calls ----------------------------------------------------------------------------

(invocation_expression
  function: (_) @callee) @reference.call

(object_creation_expression
  type: (_) @callee) @reference.new
