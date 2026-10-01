; trace-syntax query for Scala (capture contract: README.md, SPEC §6.2).

; ---- declarations ----------------------------------------------------------------------

(function_definition
  name: (identifier) @name
  body: (_) @body) @definition.function

(function_declaration
  name: (identifier) @name) @definition.function

(class_definition
  name: (identifier) @name
  body: (template_body)? @body) @definition.class

(class_definition
  extend: (extends_clause [(type_identifier) (generic_type) (stable_type_identifier)] @base)) @definition.class

(object_definition
  name: (identifier) @name
  body: (template_body)? @body) @definition.class

(object_definition
  extend: (extends_clause [(type_identifier) (generic_type) (stable_type_identifier)] @base)) @definition.class

(trait_definition
  name: (identifier) @name
  body: (template_body)? @body) @definition.interface

(trait_definition
  extend: (extends_clause [(type_identifier) (generic_type) (stable_type_identifier)] @base)) @definition.interface

; ---- calls ----------------------------------------------------------------------------

(call_expression
  function: (_) @callee) @reference.call

(instance_expression) @reference.new
