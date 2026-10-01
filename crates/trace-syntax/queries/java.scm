; trace-syntax query for Java (capture contract: README.md, SPEC §6.2).

; ---- declarations ----------------------------------------------------------------------

(class_declaration
  name: (identifier) @name
  body: (class_body) @body) @definition.class

(class_declaration
  superclass: (superclass (_) @base)) @definition.class

(class_declaration
  interfaces: (super_interfaces (type_list (_) @base))) @definition.class

(class_declaration
  (modifiers [(marker_annotation) (annotation)] @decorator)) @definition.class

(record_declaration
  name: (identifier) @name
  body: (class_body) @body) @definition.class

(enum_declaration
  name: (identifier) @name
  body: (enum_body) @body) @definition.class

(interface_declaration
  name: (identifier) @name
  body: (interface_body) @body) @definition.interface

(interface_declaration
  (extends_interfaces (type_list (_) @base))) @definition.interface

(method_declaration
  name: (identifier) @name
  body: (block)? @body) @definition.method

(method_declaration
  (modifiers [(marker_annotation) (annotation)] @decorator)) @definition.method

(constructor_declaration
  name: (identifier) @name
  body: (constructor_body) @body) @definition.constructor

(constructor_declaration
  (modifiers [(marker_annotation) (annotation)] @decorator)) @definition.constructor

(compact_constructor_declaration
  name: (identifier) @name
  body: (block) @body) @definition.constructor

; ---- calls ----------------------------------------------------------------------------

(method_invocation
  name: (identifier) @callee) @reference.call

(object_creation_expression
  type: (_) @callee) @reference.new
