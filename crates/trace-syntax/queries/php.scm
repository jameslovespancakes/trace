; trace-syntax query for PHP (capture contract: README.md, SPEC §6.2).

; ---- declarations ----------------------------------------------------------------------

(function_definition
  name: (name) @name
  body: (compound_statement) @body) @definition.function

(method_declaration
  name: (name) @name
  body: (compound_statement)? @body) @definition.method

(method_declaration
  attributes: (attribute_list) @decorator) @definition.method

(function_definition
  attributes: (attribute_list) @decorator) @definition.function

(class_declaration
  name: (name) @name
  body: (declaration_list) @body) @definition.class

(class_declaration
  attributes: (attribute_list) @decorator) @definition.class

(class_declaration
  (base_clause [(name) (qualified_name)] @base)) @definition.class

(class_declaration
  (class_interface_clause [(name) (qualified_name)] @base)) @definition.class

(interface_declaration
  name: (name) @name
  body: (declaration_list) @body) @definition.interface

(interface_declaration
  (base_clause [(name) (qualified_name)] @base)) @definition.interface

(trait_declaration
  name: (name) @name
  body: (declaration_list) @body) @definition.interface

; ---- calls ----------------------------------------------------------------------------

(function_call_expression
  function: (_) @callee) @reference.call

(member_call_expression
  name: (_) @callee) @reference.call

(nullsafe_member_call_expression
  name: (_) @callee) @reference.call

(scoped_call_expression
  name: (_) @callee) @reference.call

(object_creation_expression
  [(name) (qualified_name) (variable_name)] @callee) @reference.new
